mod commands;
mod content;
mod fsmeta;
mod git;
mod individuality;
mod intelligence;
mod lean;
mod metadata_search;
pub mod models;
mod nested;
mod operations;
mod organization;
mod packed_tags;
mod passages;
mod query;
mod registry;
mod scan;
mod security;
mod storage;
pub mod tiers;

use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub use operations::{Action, Manifest};
pub use query::{SearchOptions, Sort};
pub use scan::ScanReport;

pub struct Mosh {
    db: Connection,
    db_path: PathBuf,
    actor: String,
}

const LEDGER_VERSION: i64 = 14;

#[derive(Debug, Serialize, Deserialize)]
pub struct Object {
    #[serde(skip)]
    pub(crate) key: i64,
    #[serde(skip)]
    pub(crate) scan_stamp: Option<Vec<u8>>,
    pub id: String,
    pub kind: String,
    pub path: Option<String>,
    pub parent: Option<String>,
    pub name: String,
    pub extension: Option<String>,
    pub mime: Option<String>,
    pub size: i64,
    pub allocated: Option<i64>,
    pub created: Option<i64>,
    pub modified: Option<i64>,
    pub accessed: Option<i64>,
    pub changed: Option<i64>,
    pub attributes: Option<u32>,
    pub native_id: Option<String>,
    pub volume: Option<String>,
    pub filesystem: Option<String>,
    pub hardlinks: Option<i64>,
    pub reparse: bool,
    pub link_target: Option<String>,
    pub hash: Option<String>,
    pub revision: i64,
    pub git_root: Option<String>,
    pub git_status: Option<String>,
    pub git_author: Option<String>,
    pub git_committed: Option<i64>,
}

fn object(row: &Row<'_>) -> rusqlite::Result<Object> {
    let extension: Option<String> = row.get("extension")?;
    let mime: Option<String> = row.get("mime")?;
    let mime = mime.or_else(|| fsmeta::mime(extension.as_deref()).map(str::to_owned));
    Ok(Object {
        key: row.get("key")?,
        scan_stamp: row.get("scan_stamp")?,
        id: row.get("id")?,
        kind: row.get("kind")?,
        path: row.get("path")?,
        parent: row.get("parent")?,
        name: row.get("name")?,
        extension,
        mime,
        size: 0,
        allocated: None,
        created: row.get("created")?,
        modified: None,
        accessed: None,
        changed: None,
        attributes: None,
        native_id: fsmeta::unpack_id(row.get_ref("native_id")?)?,
        volume: None,
        filesystem: None,
        hardlinks: None,
        reparse: row.get("reparse")?,
        link_target: row.get("link_target")?,
        hash: row.get("hash")?,
        revision: row.get("revision")?,
        git_root: row.get("git_root")?,
        git_status: row.get("git_status")?,
        git_author: row.get("git_author")?,
        git_committed: row.get("git_committed")?,
    })
}

pub fn default_db() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("XDG_DATA_HOME"))
        .map(PathBuf::from)
        .or_else(|| std::env::home_dir().map(|home| home.join(".local").join("share")))
        .unwrap_or_else(std::env::temp_dir)
        .join("Mosh")
        .join("ledger.sqlite3")
}

pub(crate) fn uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}
pub(crate) fn millis(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_millis().try_into().unwrap_or(i64::MAX),
        Err(error) => -i64::try_from(error.duration().as_millis()).unwrap_or(i64::MAX),
    }
}
pub(crate) fn now() -> i64 {
    millis(SystemTime::now())
}
pub(crate) fn path_text(path: &Path) -> Result<&str> {
    path.to_str()
        .context("the path cannot be represented as Unicode")
}
pub(crate) fn canonical(path: &Path) -> Result<PathBuf> {
    dunce::canonicalize(path).with_context(|| format!("opening {}", path.display()))
}
pub(crate) fn indexed_path(path: &Path) -> Result<PathBuf> {
    if fs::symlink_metadata(path).is_ok_and(|metadata| fsmeta::reparse(&metadata)) {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        Ok(canonical(parent)?.join(path.file_name().context("link has no name")?))
    } else {
        canonical(path)
    }
}
pub(crate) fn prefix(path: &str) -> (String, String) {
    let separator = std::path::MAIN_SEPARATOR;
    let plain = path.trim_end_matches(separator);
    (
        format!("{plain}{separator}"),
        format!("{plain}{}", char::from_u32(separator as u32 + 1).unwrap()),
    )
}
pub(crate) fn event(
    db: &Connection,
    id: Option<&str>,
    action: &str,
    actor: Option<&str>,
    before: Option<&Value>,
    after: Option<&Value>,
    outcome: &str,
) -> Result<()> {
    db.execute("INSERT INTO audit(time,object,action,actor,previous,current,outcome) VALUES(?,mosh_key(?),?,?,?,?,?)",
        params![now(),id,action,actor,before.map(Value::to_string),after.map(Value::to_string),outcome])?;
    Ok(())
}

impl Mosh {
    pub fn open(path: &Path) -> Result<Self> {
        Self::as_actor(path, "local")
    }

    pub fn as_actor(path: &Path, actor: &str) -> Result<Self> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        let mut db = Connection::open(path)?;
        db.busy_timeout(Duration::from_secs(15))?;
        packed_tags::register(&db)?;
        content::register(&db)?;
        lean::register(&db)?;
        metadata_search::register(&db)?;
        fsmeta::register(&db)?;
        nested::register(&db)?;
        db.execute_batch("PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA cache_size=-32768; PRAGMA temp_store=FILE;")?;
        let version: i64 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
        ensure!(
            (0..=LEDGER_VERSION).contains(&version),
            "unsupported Ledger format {version}"
        );
        if version < 10 {
            let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let locked_version: i64 =
                tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
            ensure!(
                (0..=LEDGER_VERSION).contains(&locked_version),
                "unsupported Ledger format {locked_version}"
            );
            if locked_version == 0 {
                tx.execute_batch(include_str!("schema.sql"))?;
            }
            if locked_version < 2 {
                packed_tags::migrate(&tx)?;
                tx.execute_batch(include_str!("ledger_v2.sql"))?;
            }
            if locked_version < 3 {
                tiers::migrate(&tx)?;
            }
            if locked_version < 4 {
                lean::migrate(&tx)?;
            }
            if matches!(locked_version, 1..=4) {
                lean::normalize_legacy_paths(&tx)?;
            }
            if locked_version < 6 {
                tx.execute_batch(include_str!("ledger_v6.sql"))?;
            }
            if locked_version < 7 {
                tx.execute_batch(include_str!("nested.sql"))?;
                tx.execute("INSERT INTO metadata VALUES('mosh_id',?)", [uuid()])?;
            }
            if (2..8).contains(&locked_version) {
                packed_tags::repack_dense(&tx)?;
            }
            if locked_version < 9 {
                tx.execute_batch(include_str!("ledger_v9.sql"))?;
            }
            if locked_version < 10 {
                tx.execute_batch(include_str!("ledger_v10.sql"))?;
                metadata_search::rebuild_v10(&tx)?;
            }
            if locked_version < 10 {
                tx.pragma_update(None, "user_version", 10)?;
            }
            tx.commit()?;
        }
        if version < LEDGER_VERSION {
            db.execute_batch("PRAGMA foreign_keys=OFF; PRAGMA legacy_alter_table=ON;")?;
            let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let current: i64 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
            ensure!(
                (10..=LEDGER_VERSION).contains(&current),
                "unsupported Ledger format {current}"
            );
            if current == 10 {
                lean::consolidate_keys(&tx)?;
            }
            if current <= 11 {
                lean::slim_files(&tx)?;
            }
            if current <= 12 {
                tx.execute_batch(include_str!("ledger_v13.sql"))?;
                let violations: i64 =
                    tx.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
                        row.get(0)
                    })?;
                ensure!(
                    violations == 0,
                    "folder migration found {violations} foreign key violations"
                );
            }
            if current <= 13 {
                tx.execute_batch(
                    "CREATE INDEX object_missing_native
                    ON objects(native_volume,native_ref,created)
                    WHERE missing=1 AND native_ref IS NOT NULL AND created IS NOT NULL;",
                )?;
                tx.pragma_update(None, "user_version", LEDGER_VERSION)?;
            }
            tx.commit()?;
            db.execute_batch("PRAGMA legacy_alter_table=OFF; PRAGMA foreign_keys=ON;")?;
        }
        ensure!(
            db.query_row(
                "SELECT EXISTS(SELECT 1 FROM people WHERE id=?)",
                [actor],
                |r| r.get::<_, bool>(0)
            )?,
            "unknown Mosh person"
        );
        Ok(Self {
            db,
            db_path: canonical(path)?,
            actor: actor.to_owned(),
        })
    }

    pub fn db_path(&self) -> &Path {
        &self.db_path
    }
    pub(crate) fn admin(&self) -> Result<()> {
        ensure!(
            self.actor == "local",
            "this operation requires the local administrator"
        );
        Ok(())
    }
    pub fn get(&self, reference: &str) -> Result<Object> {
        let mut selected = self.find(reference)?;
        if self.permits(&selected, "paint")? {
            if !fsmeta::populate(&mut selected) {
                #[cfg(windows)]
                if self.recover_windows_path(&mut selected, "paint")? {
                    fsmeta::populate(&mut selected);
                }
            }
        }
        Ok(selected)
    }

    #[cfg(windows)]
    fn recover_windows_path(&self, selected: &mut Object, right: &str) -> Result<bool> {
        if selected.kind != "file" || selected.reparse {
            return Ok(false);
        }
        let Some((path, native, birth)) = selected
            .path
            .as_deref()
            .zip(selected.native_id.as_deref())
            .zip(selected.created)
            .map(|((path, native), birth)| (path, native, birth))
        else {
            return Ok(false);
        };
        let Some(found) = fsmeta::win::path_by_id(Path::new(path), native, birth, false) else {
            return Ok(false);
        };
        let Some(found) = found.to_str().filter(|found| *found != path) else {
            return Ok(false);
        };
        // The old location was checked by find. A rename can also cross a restricted Coll or
        // a nested Mosh boundary, so check the proposed path before returning it or reading it.
        let visible = organization::VISIBLE;
        let readable = security::allowed(right);
        let auth = organization::AUTHORITY;
        let accepted: bool = self
            .db
            .prepare_cached(&format!(
                "{auth}, o(key,path) AS (VALUES(?3,?4))
             SELECT {visible} AND {readable} AND EXISTS (
                 SELECT 1 FROM roots WHERE mosh_within_path(o.path,roots.path)
             ) FROM o"
            ))?
            .query_row(
                params![
                    self.actor,
                    std::path::MAIN_SEPARATOR_STR,
                    selected.key,
                    found
                ],
                |row| row.get(0),
            )?;
        if accepted && let Some(name) = Path::new(found).file_name().and_then(|name| name.to_str())
        {
            selected.path = Some(found.to_owned());
            selected.name = name.to_owned();
            let extension = lean::file_extension(name, true);
            selected.mime = fsmeta::mime(Some(&extension)).map(str::to_owned);
            selected.extension = Some(extension);
            selected.parent = Path::new(found)
                .parent()
                .and_then(Path::to_str)
                .and_then(|path| self.find(path).ok())
                .map(|parent| parent.id);
            return Ok(true);
        }
        Ok(false)
    }

    pub(crate) fn find(&self, reference: &str) -> Result<Object> {
        let by_id = lean::resolve(&self.db, reference)?
            .map(|key| {
                self.db
                    .prepare_cached("SELECT * FROM objects WHERE key=? AND missing=0")?
                    .query_row([key], object)
                    .optional()
            })
            .transpose()?
            .flatten();
        let selected = match by_id {
            Some(selected) => Some(selected),
            None => {
                let path = indexed_path(Path::new(reference)).ok();
                let key = path.as_deref().and_then(Path::to_str).unwrap_or(reference);
                lean::path_key(&self.db, key)?
                    .map(|key| {
                        self.db
                            .prepare_cached("SELECT * FROM objects WHERE key=? AND missing=0")?
                            .query_row([key], object)
                            .optional()
                    })
                    .transpose()?
                    .flatten()
            }
        };
        match selected {
            Some(selected) if self.can_read(&selected)? => Ok(selected),
            _ => bail!("indexed object not found"),
        }
    }

    pub fn roots(&self) -> Result<Vec<String>> {
        let mut query = self.db.prepare("SELECT path FROM roots ORDER BY path")?;
        let mut roots = Vec::new();
        for path in query.query_map([], |row| row.get::<_, String>(0))? {
            let path = path?;
            if self.find(&path).is_ok() {
                roots.push(path);
            }
        }
        Ok(roots)
    }

    pub fn inspect(&self, reference: &str) -> Result<Value> {
        let selected = self.get(reference)?;
        let readable = self.permits(&selected, "paint")?;
        let native = if selected.reparse || !readable {
            Value::Null
        } else {
            selected
                .path
                .as_deref()
                .map(|path| fsmeta::security(Path::new(path)))
                .unwrap_or(Value::Null)
        };
        let mut owners = self
            .db
            .prepare("SELECT person FROM owners WHERE object=? ORDER BY person")?;
        let owners = if readable {
            owners
                .query_map([selected.key], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        } else {
            vec![]
        };
        Ok(
            json!({"object":selected,"windows_security":native,"owners":owners,
            "tags":self.tags(&selected.id)?,"paint":self.paint(&selected.id)?,
            "tier":if readable { self.file_tier_for(&selected)? } else { Value::Null }}),
        )
    }

    pub fn history(&self, reference: Option<&str>, limit: usize) -> Result<Vec<Value>> {
        self.admin()?;
        let selected = reference.map(|value| self.get(value)).transpose()?;
        let mut statement = self.db.prepare(
            "SELECT time,mosh_public_id(object),action,actor,previous,current,outcome FROM audit
             WHERE (?1 IS NULL OR object=?1 OR seq IN(
                 SELECT event FROM tag_audit_members WHERE mosh_keys_contains(keys,
                     ?1)
             ))
             ORDER BY seq DESC LIMIT ?2",
        )?;
        let rows = statement.query_map(
            params![selected.as_ref().map(|o| o.key), limit.clamp(1, 1000)],
            |row| {
                let decode = |index| -> rusqlite::Result<Value> {
                    Ok(row
                        .get::<_, Option<String>>(index)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or(Value::Null))
                };
                Ok(
                    json!({"time":row.get::<_,i64>(0)?,"object":row.get::<_,Option<String>>(1)?,
                "action":row.get::<_,String>(2)?,"actor":row.get::<_,Option<String>>(3)?,
                "before":decode(4)?,"after":decode(5)?,"outcome":row.get::<_,String>(6)?}),
                )
            },
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}
