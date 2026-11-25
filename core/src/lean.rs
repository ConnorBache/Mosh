use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, functions::FunctionFlags, types::Value};

fn encode_key(mut key: u64) -> String {
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let mut bytes = [0; 12];
    let mut start = bytes.len();
    loop {
        start -= 1;
        bytes[start] = DIGITS[(key % 62) as usize];
        key /= 62;
        if key == 0 {
            break;
        }
    }
    start -= 1;
    bytes[start] = b'@';
    String::from_utf8(bytes[start..].to_vec()).expect("object key uses ASCII")
}

pub(crate) fn next_id(db: &Connection) -> Result<String> {
    let last: i64 = db
        .prepare_cached("SELECT seq FROM sqlite_sequence WHERE name='object_keys'")?
        .query_row([], |row| row.get(0))
        .optional()?
        .unwrap_or_default();
    ensure!(last >= 0 && last < i64::MAX, "Ledger object keys exhausted");
    Ok(encode_key((last + 1) as u64))
}

pub(crate) fn alias_value(id: &str) -> Value {
    if id.len() == 36
        && let Ok(uuid) = uuid::Uuid::parse_str(id)
        && uuid.hyphenated().to_string() == id
    {
        return Value::Blob(uuid.as_bytes().to_vec());
    }
    Value::Text(id.to_owned())
}

pub(crate) fn resolve(db: &Connection, reference: &str) -> Result<Option<String>> {
    Ok(db
        .prepare_cached("SELECT object FROM legacy_object_ids WHERE alias=?")?
        .query_row([alias_value(reference)], |row| row.get(0))
        .optional()?)
}

pub(crate) fn register(db: &Connection) -> Result<()> {
    let flags = FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC;
    db.create_scalar_function("mosh_object_id", 1, flags, |ctx| {
        Ok(encode_key(ctx.get::<i64>(0)? as u64))
    })?;
    db.create_scalar_function("mosh_legacy_id", 1, flags, |ctx| {
        Ok(alias_value(ctx.get::<String>(0)?.as_str()))
    })?;
    db.create_scalar_function("mosh_pack_native_id", 1, flags, |ctx| {
        Ok(ctx
            .get::<Option<String>>(0)?
            .map_or(Value::Null, |id| crate::fsmeta::pack_id(&id)))
    })?;
    Ok(())
}

pub(crate) fn migrate(db: &Connection) -> Result<()> {
    db.execute_batch(
        "PRAGMA defer_foreign_keys=ON;
         CREATE TABLE legacy_object_ids(
             alias BLOB PRIMARY KEY,
             object TEXT NOT NULL REFERENCES objects(id)
         ) WITHOUT ROWID;
         CREATE TEMP TABLE object_remap(old TEXT PRIMARY KEY, new TEXT NOT NULL) WITHOUT ROWID;
         INSERT INTO object_remap SELECT object,mosh_object_id(key) FROM object_keys;",
    )?;
    // Preserve an explicitly imported ID if it already occupies a compact key.
    db.execute_batch(
        "UPDATE object_remap SET new=old WHERE new IN (SELECT old FROM object_remap);
         DROP TRIGGER audit_no_updates;",
    )?;
    for (table, column) in [
        ("object_keys", "object"),
        ("objects", "parent"),
        ("owners", "object"),
        ("guests", "object"),
        ("paint", "object"),
        ("edges", "container"),
        ("edges", "target"),
        ("collection_queries", "container"),
        ("audit", "object"),
        ("jobs", "object"),
        ("coll_security", "object"),
        ("guest_rights", "object"),
    ] {
        db.execute_batch(&format!(
            "UPDATE {table} SET {column}=(SELECT new FROM object_remap WHERE old={table}.{column})
             WHERE EXISTS(SELECT 1 FROM object_remap WHERE old={table}.{column} AND old<>new);"
        ))?;
    }
    db.execute_batch(
        "UPDATE objects SET id=(SELECT new FROM object_remap WHERE old=objects.id)
         WHERE id IN (SELECT old FROM object_remap WHERE old<>new);
         INSERT INTO legacy_object_ids
         SELECT mosh_legacy_id(old),new FROM object_remap WHERE old<>new;
         DROP TABLE object_remap;
         CREATE TRIGGER audit_no_updates BEFORE UPDATE ON audit
         BEGIN SELECT RAISE(ABORT,'audit is append-only'); END;
         DROP INDEX object_path;
         DROP INDEX object_native;
         UPDATE objects SET native_id=mosh_pack_native_id(native_id),
             allocated=NULL,accessed=NULL,attributes=NULL,volume=NULL,filesystem=NULL,hardlinks=NULL,
             mime=CASE WHEN mime=mosh_mime(extension) THEN NULL ELSE mime END;
         CREATE INDEX object_native ON objects(native_id,created)
             WHERE missing=0 AND native_id IS NOT NULL;
         ALTER TABLE objects DROP COLUMN seen;",
    )?;
    Ok(())
}
