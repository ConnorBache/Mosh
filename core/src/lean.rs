use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, functions::FunctionFlags, params, types::Value};
use std::{
    fs,
    path::{Path, PathBuf},
};

fn canonical_alias(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }
    let canonical = crate::indexed_path(path).ok()?;
    if canonical == path
        || path.ancestors().skip(1).any(|parent| {
            !parent.as_os_str().is_empty()
                && fs::symlink_metadata(parent).is_ok_and(|meta| crate::fsmeta::reparse(&meta))
        })
    {
        return None;
    }
    Some(canonical)
}

pub(crate) fn normalize_legacy_paths(db: &Connection) -> Result<()> {
    let mut after = 0;
    loop {
        let mut query = db.prepare_cached(
            "SELECT rowid,path,reparse,native_id,created FROM objects
            WHERE rowid>? AND missing=0 AND path IS NOT NULL ORDER BY rowid LIMIT 1024",
        )?;
        let batch = query
            .query_map([after], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, bool>(2)?,
                    crate::fsmeta::unpack_id(row.get_ref(3)?)?,
                    row.get::<_, Option<i64>>(4)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let Some(last) = batch.last().map(|row| row.0) else {
            break;
        };
        after = last;
        for (row, spelling, reparse, native, birth) in batch {
            let path = Path::new(&spelling);
            let Some(corrected) = canonical_alias(path) else {
                continue;
            };
            let Ok(meta) = fs::symlink_metadata(&corrected) else {
                continue;
            };
            if crate::fsmeta::reparse(&meta) != reparse
                || birth.is_some_and(|birth| meta.created().ok().map(crate::millis) != Some(birth))
                || native.as_ref().is_some_and(|id| {
                    crate::fsmeta::native(&corrected, &meta).id.as_ref() != Some(id)
                })
            {
                continue;
            }
            if let Some(corrected) = corrected.to_str() {
                db.prepare_cached(
                    "UPDATE objects SET path=?1 WHERE rowid=?2 AND
                    NOT EXISTS(SELECT 1 FROM objects WHERE path=?1 AND missing=0)",
                )?
                .execute(params![corrected, row])?;
            }
        }
    }
    let roots = db
        .prepare("SELECT path,scanned FROM roots")?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (root, scanned) in roots {
        if let Some(path) = canonical_alias(Path::new(&root))
            && let Some(path) = path.to_str()
            && fs::symlink_metadata(path)
                .is_ok_and(|meta| meta.is_dir() && !crate::fsmeta::reparse(&meta))
        {
            db.execute(
                "INSERT INTO roots VALUES(?,?) ON CONFLICT(path)
                DO UPDATE SET scanned=max(scanned,excluded.scanned)",
                params![path, scanned],
            )?;
            db.execute("DELETE FROM roots WHERE path=?", [&root])?;
        }
    }
    Ok(())
}

pub(crate) fn encode_key(mut key: u64) -> String {
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

pub(crate) fn next_key(db: &Connection) -> Result<i64> {
    let last: i64 = db
        .prepare_cached("SELECT seq FROM sqlite_sequence WHERE name='objects'")?
        .query_row([], |row| row.get(0))
        .optional()?
        .unwrap_or_default();
    ensure!(last >= 0 && last < i64::MAX, "Ledger object keys exhausted");
    Ok(last + 1)
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

fn decode_key(reference: &str) -> Option<i64> {
    let digits = reference.strip_prefix('@')?;
    let key = digits.bytes().try_fold(0i64, |key, digit| {
        let value = match digit {
            b'0'..=b'9' => digit - b'0',
            b'a'..=b'z' => digit - b'a' + 10,
            b'A'..=b'Z' => digit - b'A' + 36,
            _ => return None,
        };
        key.checked_mul(62)?.checked_add(i64::from(value))
    })?;
    (key > 0 && !digits.starts_with('0')).then_some(key)
}

pub(crate) fn resolve(db: &Connection, reference: &str) -> rusqlite::Result<Option<i64>> {
    if let Some(key) = decode_key(reference)
        && let Some(found) = db
            .prepare_cached(
                "SELECT key FROM objects WHERE key=?1 AND (legacy_id IS NULL OR legacy_id=?2)",
            )?
            .query_row(params![key, reference], |row| row.get(0))
            .optional()?
    {
        return Ok(Some(found));
    }
    db.prepare_cached(
        "SELECT key FROM objects WHERE legacy_id=?1
         UNION ALL SELECT object FROM legacy_object_ids WHERE alias=?2 LIMIT 1",
    )?
    .query_row(params![reference, alias_value(reference)], |row| row.get(0))
    .optional()
}

fn parent_prefix<'a>(path: &'a str, name: &str) -> Option<&'a str> {
    if name.is_empty() {
        return None;
    }
    path.strip_suffix(name)
        .filter(|prefix| prefix.ends_with(std::path::MAIN_SEPARATOR))
}

fn directory_prefix(path: &str) -> String {
    if path.ends_with(std::path::MAIN_SEPARATOR) {
        path.to_owned()
    } else {
        format!("{path}{}", std::path::MAIN_SEPARATOR)
    }
}

pub(crate) fn parent_coll_key(db: &Connection, path: &str) -> rusqlite::Result<Option<i64>> {
    db.prepare_cached(
        "SELECT key FROM objects WHERE missing=0 AND kind_code=1 AND path_literal IS NOT NULL
         AND mosh_path_key(path_literal)=mosh_path_key(?1) AND path_literal=?1",
    )?
    .query_row([path], |row| row.get(0))
    .optional()
}

pub(crate) fn path_key(db: &Connection, path: &str) -> rusqlite::Result<Option<i64>> {
    let name = Path::new(path).file_name().and_then(|name| name.to_str());
    let folder = name.and_then(|name| parent_prefix(path, name));
    db.prepare_cached(
        "SELECT key FROM objects WHERE missing=0
          AND path_folder=(SELECT id FROM path_folders WHERE prefix=?2)
          AND mosh_name_key(name)=mosh_name_key(?3) AND name=?3
         UNION ALL SELECT key FROM objects WHERE missing=0 AND path_literal IS NOT NULL
          AND mosh_path_key(path_literal)=mosh_path_key(?1) AND path_literal=?1
         LIMIT 1",
    )?
    .query_row(params![path, folder, name], |row| row.get(0))
    .optional()
}

pub(crate) fn store_location<'a>(
    db: &Connection,
    path: &'a str,
    basename: &str,
    file: bool,
) -> Result<(Option<i64>, Option<&'a str>)> {
    let Some(prefix) = parent_prefix(path, basename).filter(|_| file) else {
        return Ok((None, Some(path)));
    };
    let mut lookup = db.prepare_cached("SELECT id FROM path_folders WHERE prefix=?")?;
    if let Some(id) = lookup.query_row([prefix], |row| row.get(0)).optional()? {
        return Ok((Some(id), None));
    }
    let coll = Path::new(path)
        .parent()
        .and_then(Path::to_str)
        .map(|parent| parent_coll_key(db, parent))
        .transpose()?
        .flatten();
    let id = db
        .prepare_cached("INSERT INTO path_folders(prefix,coll_key) VALUES(?,?) RETURNING id")?
        .query_row(params![prefix, coll], |row| row.get(0))?;
    Ok((Some(id), None))
}

pub(crate) fn register(db: &Connection) -> Result<()> {
    let flags = FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC;
    let generated = flags | FunctionFlags::SQLITE_INNOCUOUS;
    db.create_scalar_function("mosh_path_prefix", 2, generated, |ctx| {
        let path: Option<String> = ctx.get(0)?;
        let name: Option<String> = ctx.get(1)?;
        Ok(path
            .as_deref()
            .zip(name.as_deref())
            .and_then(|(p, name)| parent_prefix(p, name))
            .map(str::to_owned))
    })?;
    db.create_scalar_function("mosh_directory_prefix", 1, generated, |ctx| {
        Ok(ctx
            .get::<Option<String>>(0)?
            .as_deref()
            .map(directory_prefix))
    })?;
    db.create_scalar_function("mosh_basename", 1, generated, |ctx| {
        Ok(ctx
            .get::<Option<String>>(0)?
            .as_deref()
            .and_then(|path| Path::new(path).file_name())
            .and_then(|name| name.to_str())
            .map(str::to_owned))
    })?;
    db.create_scalar_function("mosh_name_key", 1, generated, |ctx| {
        Ok(ctx
            .get::<Option<String>>(0)?
            .map(|name| blake3::hash(name.as_bytes()).as_bytes()[..2].to_vec()))
    })?;
    db.create_scalar_function("mosh_folder_coll", 1, generated, |ctx| {
        let Some(folder) = ctx.get::<Option<i64>>(0)? else {
            return Ok(None);
        };
        let db = unsafe { ctx.get_connection()? };
        db.prepare_cached("SELECT coll_key FROM path_folders WHERE id=?")?
            .query_row([folder], |row| row.get::<_, Option<i64>>(0))
    })?;
    db.create_scalar_function("mosh_expand_path", 3, generated, |ctx| {
        let folder: Option<i64> = ctx.get(0)?;
        let name: Option<String> = ctx.get(1)?;
        let Some((folder, name)) = folder.zip(name) else {
            return ctx.get::<Option<String>>(2);
        };
        // The reference stays in this synchronous callback. Read the immutable
        // dictionary on this connection so it participates in the caller's snapshot.
        let db = unsafe { ctx.get_connection()? };
        let mut prefix: String = db.query_row(
            "SELECT prefix FROM main.path_folders WHERE id=?",
            [folder],
            |row| row.get(0),
        )?;
        prefix.push_str(&name);
        Ok(Some(prefix))
    })?;
    db.create_scalar_function("mosh_file_extension", 2, generated, |ctx| {
        let name: Option<String> = ctx.get(0)?;
        let kind: Option<i64> = ctx.get(1)?;
        Ok(name
            .zip(kind)
            .map(|(name, kind)| file_extension(&name, kind == 0)))
    })?;
    db.create_scalar_function(
        "mosh_path_key",
        1,
        flags | FunctionFlags::SQLITE_INNOCUOUS,
        |ctx| {
            let path: Option<String> = ctx.get(0)?;
            Ok(path.map(|path| blake3::hash(path.as_bytes()).as_bytes()[..8].to_vec()))
        },
    )?;
    db.create_scalar_function("mosh_object_id", 1, generated, |ctx| {
        Ok(ctx.get::<Option<i64>>(0)?.map(|key| encode_key(key as u64)))
    })?;
    db.create_scalar_function("mosh_key", 1, flags, |ctx| {
        let Some(reference) = ctx.get::<Option<String>>(0)? else {
            return Ok(None);
        };
        // The lookup stays on this connection and in the caller's snapshot.
        let db = unsafe { ctx.get_connection()? };
        resolve(&db, &reference)
    })?;
    db.create_scalar_function("mosh_public_id", 1, generated, |ctx| {
        let Some(key) = ctx.get::<Option<i64>>(0)? else {
            return Ok(None);
        };
        let db = unsafe { ctx.get_connection()? };
        db.prepare_cached("SELECT id FROM objects WHERE key=?")?
            .query_row([key], |row| row.get::<_, String>(0))
            .optional()
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

pub(crate) fn file_extension(name: &str, file: bool) -> String {
    if !file {
        return String::new();
    }
    Path::new(name)
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_lowercase()
}

fn schema_for(db: &Connection, table: &str, triggers: bool) -> Result<Vec<String>> {
    let mut query = db.prepare(
        "SELECT sql FROM sqlite_schema WHERE tbl_name=?1 AND sql IS NOT NULL
        AND (type='index' OR (?2 AND type='trigger')) ORDER BY type,name",
    )?;
    Ok(query
        .query_map(params![table, triggers], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

pub(crate) fn slim_files(db: &Connection) -> Result<()> {
    db.execute_batch(
        "DROP TRIGGER ledger_insert; DROP TRIGGER ledger_update; DROP TRIGGER text_invalidate;
         DROP TRIGGER paint_insert; DROP TRIGGER paint_update; DROP TRIGGER paint_delete;
         DROP TRIGGER path_search_directory_insert; DROP TRIGGER path_search_directory_delete;
         DROP TRIGGER path_search_root_insert; DROP TRIGGER path_search_root_delete;
         DROP TABLE ledger; DROP TABLE directory_fts; DROP TABLE text_terms; DROP TABLE text_cached;
         DELETE FROM metadata WHERE key='text_budget';",
    )?;
    let sequence: Option<i64> = db
        .query_row(
            "SELECT seq FROM sqlite_sequence WHERE name='objects'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let old_links = db.prepare(
        "SELECT name,sql FROM sqlite_schema s WHERE type='table' AND EXISTS(
            SELECT 1 FROM pragma_foreign_key_list(s.name) fk WHERE fk.[table]='objects' AND fk.[to]='id')",
    )?.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let object_schema = schema_for(db, "objects", true)?;
    db.execute_batch(include_str!("ledger_v12_objects.sql"))?;
    for (table, original) in old_links {
        let columns = db
            .prepare("SELECT name FROM pragma_table_info(?) ORDER BY cid")?
            .query_map([&table], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let references = db.prepare(
            "SELECT [from] FROM pragma_foreign_key_list(?) WHERE [table]='objects' AND [to]='id'",
        )?.query_map([&table], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut definition = original[original.find('(').context("missing table definition")?..]
            .replace("REFERENCES objects(id)", "REFERENCES objects(key)");
        for column in &references {
            let old = format!("{column} TEXT");
            ensure!(
                definition.contains(&old),
                "missing reference definition for {table}.{column}"
            );
            definition = definition.replace(&old, &format!("{column} INTEGER"));
        }
        let projection = columns
            .iter()
            .map(|column| {
                if references.contains(column) {
                    format!("(SELECT key FROM objects WHERE id=old.\"{column}\")")
                } else {
                    format!("old.\"{column}\"")
                }
            })
            .collect::<Vec<_>>()
            .join(",");
        let related = schema_for(db, &table, true)?;
        db.execute_batch(&format!(
            "CREATE TABLE mosh_rebind{definition};
             INSERT INTO mosh_rebind SELECT {projection} FROM \"{table}\" old;
             DROP TABLE \"{table}\"; ALTER TABLE mosh_rebind RENAME TO \"{table}\";"
        ))?;
        for statement in related {
            db.execute_batch(&statement)?;
        }
    }
    db.execute_batch("DROP TABLE objects; ALTER TABLE objects_next RENAME TO objects;
        CREATE UNIQUE INDEX legacy_public_id ON objects(legacy_id) WHERE legacy_id IS NOT NULL;
        CREATE INDEX object_parent_key ON objects(parent_key) WHERE missing=0;
        CREATE INDEX object_native ON objects(native_volume,native_ref) WHERE missing=0 AND native_ref IS NOT NULL;")?;
    if let Some(sequence) = sequence {
        db.execute(
            "UPDATE sqlite_sequence SET seq=max(seq,?1) WHERE name='objects'",
            [sequence],
        )?;
        db.execute(
            "INSERT INTO sqlite_sequence(name,seq) SELECT 'objects',?1 WHERE NOT EXISTS(
            SELECT 1 FROM sqlite_sequence WHERE name='objects')",
            [sequence],
        )?;
    }
    for statement in object_schema {
        if [
            "CREATE INDEX object_native ",
            "CREATE INDEX object_size ",
            "CREATE INDEX object_parent ",
        ]
        .iter()
        .all(|prefix| !statement.starts_with(prefix))
        {
            db.execute_batch(
                &statement
                    .replace("id<>NEW.id", "key<>NEW.key")
                    .replace("id<>OLD.id", "key<>OLD.key"),
            )?;
        }
    }
    let violations: i64 =
        db.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
            r.get(0)
        })?;
    ensure!(
        violations == 0,
        "lean Ledger migration found {violations} foreign key violations"
    );
    Ok(())
}

pub(crate) fn consolidate_keys(db: &Connection) -> Result<()> {
    let sequence: i64 = db.query_row(
        "SELECT coalesce((SELECT seq FROM sqlite_sequence WHERE name='object_keys'),0)",
        [],
        |r| r.get(0),
    )?;
    let dependents: Vec<(String, String)> = db
        .prepare(
            "SELECT name,sql FROM sqlite_schema s WHERE type='table' AND EXISTS(
            SELECT 1 FROM pragma_foreign_key_list(s.name) AS fk WHERE fk.[table]='object_keys')",
        )?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let indexes_and_triggers = schema_for(db, "objects", true)?;
    db.execute_batch("DROP TRIGGER refresh_tag_minimum;
        ALTER TABLE tiers DROP COLUMN minimum_tags;
        ALTER TABLE tiers ADD COLUMN minimum_individuality INTEGER NOT NULL DEFAULT 0 CHECK(minimum_individuality BETWEEN 0 AND 100);
        UPDATE tiers SET minimum_individuality=CASE level WHEN 1 THEN 0 WHEN 2 THEN 60 WHEN 3 THEN 80 WHEN 4 THEN 90 ELSE 95 END;
        DELETE FROM file_tiers WHERE assessor IN ('f','filesystem') AND assessment IS NULL
            AND requested IS NULL AND locked=0 AND human_accesses=0 AND last_access IS NULL;
        ALTER TABLE file_tiers DROP COLUMN effective;")?;
    db.execute_batch(include_str!("ledger_v11_objects.sql"))?;
    let loss: i64 = db.query_row(
        "SELECT (SELECT count(*) FROM objects)-(SELECT count(*) FROM objects_next)",
        [],
        |row| row.get(0),
    )?;
    ensure!(loss == 0, "a file is missing its Ledger key");
    db.execute_batch(
        "DROP TABLE objects; ALTER TABLE objects_next RENAME TO objects;
        DROP TABLE object_keys;
        CREATE VIEW object_keys AS SELECT key,id AS object,tag_set FROM objects;
        CREATE TRIGGER object_keys_tag_update INSTEAD OF UPDATE OF tag_set ON object_keys BEGIN
            UPDATE objects SET tag_set=NEW.tag_set WHERE key=OLD.key;
        END;
        CREATE INDEX tag_set_owners ON objects(tag_set) WHERE tag_set IS NOT NULL;",
    )?;
    db.execute(
        "UPDATE sqlite_sequence SET seq=max(seq,?) WHERE name='objects'",
        [sequence],
    )?;
    db.execute(
        "INSERT INTO sqlite_sequence(name,seq) SELECT 'objects',?1 WHERE ?1>0
         AND NOT EXISTS(SELECT 1 FROM sqlite_sequence WHERE name='objects')",
        [sequence],
    )?;
    for statement in indexes_and_triggers {
        if !statement.starts_with("CREATE TRIGGER ledger_insert ") {
            db.execute_batch(&statement)?;
        }
    }
    db.execute_batch(
        "CREATE TRIGGER ledger_insert AFTER INSERT ON objects WHEN NEW.missing=0 BEGIN
         INSERT INTO ledger(rowid,name,paint) VALUES(NEW.key,NEW.name,''); END;",
    )?;
    for (table, original) in dependents {
        let sql = if table == "file_tiers" {
            db.query_row(
                "SELECT sql FROM sqlite_schema WHERE name='file_tiers'",
                [],
                |row| row.get(0),
            )?
        } else {
            original
        };
        let table_definition = &sql[sql.find('(').context("missing table definition")?..];
        let replaced =
            table_definition.replace("REFERENCES object_keys(key)", "REFERENCES objects(key)");
        ensure!(
            replaced != table_definition,
            "cannot remap foreign keys for {table}"
        );
        let related = schema_for(db, &table, true)?;
        let table = format!("\"{}\"", table.replace('"', "\"\""));
        db.execute_batch(&format!(
            "CREATE TABLE mosh_rebind{replaced};
             INSERT INTO mosh_rebind SELECT * FROM {table};
             DROP TABLE {table}; ALTER TABLE mosh_rebind RENAME TO {table};"
        ))?;
        for statement in related {
            db.execute_batch(&statement)?;
        }
    }
    let violations: i64 =
        db.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })?;
    ensure!(
        violations == 0,
        "identity migration found {violations} foreign key violations"
    );
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

#[cfg(test)]
mod upgrade_tests {
    use super::*;
    use crate::{Mosh, SearchOptions};

    #[test]
    fn filename_lookup_checks_spelling_after_a_shared_fingerprint() -> Result<()> {
        let fixture = tempfile::tempdir()?;
        let root = fixture.path().join("sources");
        fs::create_dir(&root)?;
        let mut names = std::collections::HashMap::new();
        let (a, b) = (0..=65_536)
            .find_map(|number| {
                let name = format!("unit-{number}.txt");
                let hash = blake3::hash(name.as_bytes());
                let fingerprint: [u8; 2] = hash.as_bytes()[..2].try_into().unwrap();
                names
                    .insert(fingerprint, name.clone())
                    .map(|other| (other, name))
            })
            .expect("two names must share a two-byte fingerprint");
        let a = root.join(a);
        let b = root.join(b);
        fs::write(&a, "first")?;
        fs::write(&b, "second")?;
        let mosh = Mosh::open(&fixture.path().join("ledger.sqlite3"))?;
        assert_eq!(
            mosh.scan(&root, &std::sync::atomic::AtomicBool::new(false))?
                .errors,
            0
        );
        let a = a.to_str().unwrap();
        let b = b.to_str().unwrap();
        assert_ne!(mosh.get(a)?.id, mosh.get(b)?.id);
        assert_eq!(mosh.preview(a)?, "first");
        assert_eq!(mosh.preview(b)?, "second");
        Ok(())
    }

    #[test]
    fn v2_keeps_aliases_access_links_and_history_through_restore() -> Result<()> {
        let fixture = tempfile::tempdir()?;
        let folder = fixture.path().join("restricted");
        fs::create_dir(&folder)?;
        let file = folder.join("source.txt");
        fs::write(&file, "Current résumé and timeline.")?;
        let db_path = fixture.path().join("v2.sqlite3");
        {
            let db = Connection::open(&db_path)?;
            crate::packed_tags::register(&db)?;
            db.execute_batch(include_str!("schema.sql"))?;
            db.execute("INSERT INTO objects(id,kind,path,name,secret) VALUES('v2-coll','coll',?,'restricted',1)",
                [folder.to_str().unwrap()])?;
            db.execute("INSERT INTO objects(id,kind,path,parent,name,extension) VALUES('v2-file','file',?,'v2-coll','source.txt','txt')",
                [file.to_str().unwrap()])?;
            db.execute_batch("INSERT INTO people VALUES('reader',NULL),('outsider',NULL);
                INSERT INTO owners VALUES('v2-coll','local');
                INSERT INTO guests VALUES('v2-coll','reader','read');
                INSERT INTO objects(id,kind,name) VALUES('v2-marriage','marriage','References');
                INSERT INTO edges VALUES('v2-marriage','v2-file','reference','');
                INSERT INTO paint VALUES('v2-file','standard','Approved handoff',1,1,'user');
                INSERT INTO audit(time,object,action,actor,outcome) VALUES(1,'v2-file','user-note','local','ok');
                INSERT INTO tag_dictionary VALUES(1,'project');
                INSERT INTO tag_aliases VALUES('workstream',1);
                INSERT INTO tag_assignments VALUES('v2-file',1,'user',0.8,'Reviewed by a person');")?;
            crate::packed_tags::migrate(&db)?;
            db.execute_batch(include_str!("ledger_v2.sql"))?;
            db.pragma_update(None, "user_version", 2)?;
        }
        let mosh = Mosh::open(&db_path)?;
        let key = mosh.get("v2-file")?.id;
        let check = |mosh: &Mosh| -> Result<()> {
            let (literal, prefix, extension): (Option<String>, String, String) =
                mosh.db.query_row(
                    "SELECT o.path_literal,f.prefix,o.extension FROM objects o
                 JOIN path_folders f ON f.id=o.path_folder WHERE o.id=?",
                    [&key],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )?;
            assert!(literal.is_none());
            assert_eq!(
                format!("{prefix}source.txt"),
                crate::path_text(&crate::canonical(&file)?)?
            );
            assert_eq!(extension, "txt");
            let hits = mosh.search(
                "content:resume ext:txt direct-tag:workstream paint:handoff marriage:References",
                &SearchOptions::default(),
            )?;
            assert_eq!(hits.len(), 1);
            assert_eq!(hits[0].id, key);
            assert_eq!(
                mosh.tags("v2-file")?
                    .iter()
                    .find(|tag| tag["name"] == "project")
                    .unwrap()["evidence"],
                "Reviewed by a person"
            );
            assert_eq!(mosh.history(Some("v2-file"), 10)?[0]["action"], "user-note");
            Ok(())
        };
        check(&mosh)?;
        assert_eq!(Mosh::as_actor(&db_path, "reader")?.get("v2-file")?.id, key);
        assert!(
            Mosh::as_actor(&db_path, "outsider")?
                .get("v2-file")
                .is_err()
        );
        mosh.rebuild()?;
        mosh.compact()?;
        check(&mosh)?;
        let backup = fixture.path().join("snapshot.sqlite3");
        mosh.backup(&backup)?;
        let restored = Mosh::restore(&backup, &fixture.path().join("restore.sqlite3"))?;
        check(&Mosh::open(&restored)?)?;
        assert!(
            Mosh::as_actor(&restored, "outsider")?
                .get("v2-file")
                .is_err()
        );
        Ok(())
    }
}
