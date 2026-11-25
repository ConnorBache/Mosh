use crate::content::with_fts_sample;
use rusqlite::{Connection, OptionalExtension, functions::FunctionFlags};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

fn relative(path: &Path, root: &Path) -> Option<String> {
    if let Ok(relative) = path.strip_prefix(root) {
        return relative.to_str().map(str::to_owned);
    }
    #[cfg(windows)]
    if crate::nested::within(path, root) {
        let mut parts = path.components();
        for _ in root.components() {
            parts.next()?;
        }
        return parts.as_path().to_str().map(str::to_owned);
    }
    None
}

pub(crate) fn phrase(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn indexed_tokens(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    conn.prepare_cached("SELECT term FROM sample_tokens ORDER BY offset")?
        .query_map([], |row| row.get(0))?
        .collect()
}

pub(crate) fn register(db: &Connection) -> rusqlite::Result<()> {
    let flags = FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC;
    db.create_scalar_function("mosh_search_relative", 2, flags, |ctx| {
        let path: Option<String> = ctx.get(0)?;
        let root: String = ctx.get(1)?;
        Ok(path.and_then(|path| relative(Path::new(&path), Path::new(&root))))
    })?;
    db.create_scalar_function("mosh_directory_prefix", 1, flags, |ctx| {
        let Some(mut path) = ctx.get::<Option<String>>(0)? else {
            return Ok(None);
        };
        if !path.ends_with(std::path::MAIN_SEPARATOR) {
            path.push(std::path::MAIN_SEPARATOR);
        }
        Ok(Some(path))
    })?;

    let scratch = Arc::new(Mutex::new(None::<Connection>));
    let splits = scratch.clone();
    db.create_scalar_function(
        "mosh_metadata_splits",
        1,
        flags | FunctionFlags::SQLITE_DIRECTONLY,
        move |ctx| {
            let value: String = ctx.get(0)?;
            with_fts_sample(&splits, &value, |conn| {
                let tokens = indexed_tokens(conn)?;
                // Very long phrases use candidate verification to keep the split list bounded.
                if tokens.len() > 32 {
                    return Ok(None);
                }
                let chunks = (1..tokens.len())
                    .map(|mid| (tokens[..mid].join(" "), phrase(&tokens[mid..].join(" "))))
                    .collect::<Vec<_>>();
                Ok(Some(serde_json::json!(chunks).to_string()))
            })
        },
    )?;
    let suffix = scratch.clone();
    db.create_scalar_function(
        "mosh_metadata_suffix",
        2,
        flags | FunctionFlags::SQLITE_DIRECTONLY,
        move |ctx| {
            let Some(directory) = ctx.get::<Option<String>>(0)? else {
                return Ok(false);
            };
            let head: String = ctx.get(1)?;
            let terms = head.split(' ').collect::<Vec<_>>();
            with_fts_sample(&suffix, &directory, |conn| {
                let tokens = indexed_tokens(conn)?;
                Ok(tokens.len() >= terms.len()
                    && tokens
                        .iter()
                        .rev()
                        .zip(terms.iter().rev())
                        .all(|(a, b)| a == b))
            })
        },
    )?;
    let edges = scratch.clone();
    db.create_scalar_function("mosh_metadata_edge", 2, flags | FunctionFlags::SQLITE_DIRECTONLY, move |ctx| {
        let value: String = ctx.get(0)?;
        let last: bool = ctx.get(1)?;
        with_fts_sample(&edges, &value, |conn| {
            let query = if last { "SELECT term FROM sample_tokens WHERE offset>0 ORDER BY offset DESC LIMIT 1" }
                        else { "SELECT term FROM sample_tokens WHERE offset=0 AND EXISTS(SELECT 1 FROM sample_tokens WHERE offset=1)" };
            let token: Option<String> = conn.prepare_cached(query)?.query_row([], |row| row.get(0)).optional()?;
            Ok(token.as_deref().map_or_else(|| phrase(""), phrase))
        })
    })?;
    db.create_scalar_function(
        "mosh_metadata_match",
        3,
        flags | FunctionFlags::SQLITE_DIRECTONLY,
        move |ctx| {
            let Some(directory) = ctx.get::<Option<String>>(0)? else {
                return Ok(false);
            };
            let name: String = ctx.get(1)?;
            let phrase: String = ctx.get(2)?;
            let path = format!("{directory}/{name}");
            with_fts_sample(&scratch, &path, |conn| {
                conn.prepare_cached("SELECT EXISTS(SELECT 1 FROM sample WHERE sample MATCH ?)")?
                    .query_row([phrase], |row| row.get(0))
            })
        },
    )?;
    Ok(())
}

pub(crate) fn rebuild(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch(
        "INSERT INTO ledger(ledger) VALUES('delete-all');
        INSERT INTO ledger(rowid,name,paint)
        SELECT k.key,o.name,coalesce(p.body,'') FROM objects o
        JOIN object_keys k ON k.object=o.id LEFT JOIN paint p ON p.object=o.id WHERE o.missing=0;",
    )?;
    rebuild_directories(db)
}

pub(crate) fn rebuild_directories(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch("INSERT INTO directory_fts(directory_fts) VALUES('delete-all');
        INSERT INTO directory_fts(rowid,relative) SELECT id,relative FROM path_folder_search WHERE relative<>'';")
}
