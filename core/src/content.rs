use crate::{Object, canonical, fsmeta};
use anyhow::Result;
use rusqlite::{Connection, functions::FunctionFlags};
use std::{fs, path::Path, sync::Mutex};

pub(crate) fn with_fts_sample<T>(
    scratch: &Mutex<Option<Connection>>,
    text: &str,
    read: impl FnOnce(&Connection) -> rusqlite::Result<T>,
) -> rusqlite::Result<T> {
    let mut scratch = scratch.lock().map_err(|_| rusqlite::Error::InvalidQuery)?;
    if scratch.is_none() {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("CREATE VIRTUAL TABLE sample USING fts5(body,content='',contentless_delete=1,tokenize='unicode61');")?;
        *scratch = Some(conn);
    }
    let conn = scratch.as_ref().unwrap();
    conn.prepare_cached("INSERT INTO sample(sample) VALUES('delete-all')")?
        .execute([])?;
    conn.prepare_cached("INSERT INTO sample(rowid,body) VALUES(1,?)")?
        .execute([text])?;
    read(conn)
}

fn extract(
    path: &str,
    extension: &str,
    native_id: Option<&str>,
    created: Option<i64>,
) -> Option<String> {
    if !fsmeta::text_supported(extension) {
        return None;
    }
    let path = Path::new(path);
    let metadata = fs::symlink_metadata(path).ok()?;
    if !metadata.is_file() || fsmeta::reparse(&metadata) || canonical(path).ok()?.as_path() != path
    {
        return None;
    }
    if let Some(id) = native_id {
        if fsmeta::native(path, &metadata).id.as_deref() != Some(id) {
            return None;
        }
    }
    if let Some(created) = created {
        if metadata.created().ok().map(crate::millis) != Some(created) {
            return None;
        }
    }
    fsmeta::text(path, extension).ok()
}

pub fn preview(object: &Object) -> String {
    if object.kind != "file" || object.reparse {
        return String::new();
    }
    object
        .path
        .as_deref()
        .zip(object.extension.as_deref())
        .and_then(|(path, extension)| {
            extract(path, extension, object.native_id.as_deref(), object.created)
        })
        .unwrap_or_default()
}

pub fn register(db: &Connection) -> Result<()> {
    db.create_scalar_function(
        "mosh_text_supported",
        1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| {
            Ok(ctx
                .get::<Option<String>>(0)?
                .is_some_and(|extension| fsmeta::text_supported(&extension)))
        },
    )?;
    // A private in-memory FTS document uses the same tokenizer and phrase semantics as the cache.
    let scratch = Mutex::new(None::<Connection>);
    db.create_scalar_function(
        "mosh_live_match",
        5,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DIRECTONLY,
        move |ctx| {
            let path = ctx.get::<String>(0)?;
            let extension = ctx.get::<String>(1)?;
            let native = fsmeta::unpack_id(ctx.get_raw(2))?;
            let created = ctx.get::<Option<i64>>(3)?;
            let term = ctx.get::<String>(4)?;
            let Some(text) = extract(&path, &extension, native.as_deref(), created) else {
                return Ok(false);
            };
            if text.is_empty() {
                return Ok(false);
            }
            crate::metadata_search::matches(&scratch, &text, &term)
        },
    )?;
    Ok(())
}
