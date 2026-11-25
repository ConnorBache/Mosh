use anyhow::{Result, ensure};
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

fn parent_prefix<'a>(path: &'a str, name: &str) -> Option<&'a str> {
    if name.is_empty() {
        return None;
    }
    path.strip_suffix(name)
        .filter(|prefix| prefix.ends_with(std::path::MAIN_SEPARATOR))
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
    let id = db
        .prepare_cached("INSERT INTO path_folders(prefix) VALUES(?) RETURNING id")?
        .query_row([prefix], |row| row.get(0))?;
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
