use crate::content::with_fts_sample;
use rusqlite::{Connection, functions::FunctionFlags};
use std::{path::Path, sync::Mutex};

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

pub(crate) fn matches(
    scratch: &Mutex<Option<Connection>>,
    text: &str,
    value: &str,
) -> rusqlite::Result<bool> {
    if text.is_ascii() && value.is_ascii() {
        fn tokens(value: &str) -> impl Iterator<Item = &str> + Clone {
            value
                .split(|c: char| !c.is_ascii_alphanumeric())
                .filter(|token| !token.is_empty())
        }
        let phrase = tokens(value).collect::<Vec<_>>();
        let Some((first, tail)) = phrase.split_first() else {
            return Ok(false);
        };
        let mut words = tokens(text);
        while let Some(word) = words.next() {
            if word.eq_ignore_ascii_case(first) {
                let mut rest = words.clone();
                if tail.iter().all(|term| {
                    rest.next()
                        .is_some_and(|word| word.eq_ignore_ascii_case(term))
                }) {
                    return Ok(true);
                }
            }
        }
        return Ok(false);
    }
    let phrase = format!("\"{}\"", value.replace('"', "\"\""));
    with_fts_sample(scratch, text, |conn| {
        conn.prepare_cached("SELECT EXISTS(SELECT 1 FROM sample WHERE sample MATCH ?)")?
            .query_row([phrase], |row| row.get(0))
    })
}

pub(crate) fn register(db: &Connection) -> rusqlite::Result<()> {
    let flags = FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC;
    db.create_scalar_function("mosh_search_relative", 2, flags, |ctx| {
        let path: Option<String> = ctx.get(0)?;
        let root: String = ctx.get(1)?;
        Ok(path.and_then(|path| relative(Path::new(&path), Path::new(&root))))
    })?;
    let scratch = Mutex::new(None::<Connection>);
    db.create_scalar_function(
        "mosh_metadata_match",
        2,
        flags | FunctionFlags::SQLITE_DIRECTONLY,
        move |ctx| {
            let Some(text) = ctx.get::<Option<String>>(0)? else {
                return Ok(false);
            };
            let value: String = ctx.get(1)?;
            matches(&scratch, &text, &value)
        },
    )?;
    Ok(())
}

pub(crate) fn rebuild_v10(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch(
        "INSERT INTO ledger(ledger) VALUES('delete-all');
        INSERT INTO ledger(rowid,name,paint)
        SELECT k.key,o.name,coalesce(p.body,'') FROM objects o
        JOIN object_keys k ON k.object=o.id LEFT JOIN paint p ON p.object=o.id WHERE o.missing=0;",
    )?;
    db.execute_batch("INSERT INTO directory_fts(directory_fts) VALUES('delete-all');
        INSERT INTO directory_fts(rowid,relative) SELECT id,relative FROM path_folder_search WHERE relative<>'';")
}
