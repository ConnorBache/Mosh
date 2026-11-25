mod commands;
mod content;
mod fsmeta;
mod git;
mod intelligence;
mod lean;
pub mod models;
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

const LEDGER_VERSION: i64 = 5;

#[derive(Debug, Serialize, Deserialize)]
pub struct Object {
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
        id: row.get("id")?,
        kind: row.get("kind")?,
        path: row.get("path")?,
        parent: row.get("parent")?,
        name: row.get("name")?,
        extension,
        mime,
        size: row.get("size")?,
        allocated: row.get("allocated")?,
        created: row.get("created")?,
        modified: row.get("modified")?,
        accessed: row.get("accessed")?,
        changed: row.get("changed")?,
        attributes: row.get("attributes")?,
        native_id: fsmeta::unpack_id(row.get_ref("native_id")?)?,
        volume: row.get("volume")?,
        filesystem: row.get("filesystem")?,
        hardlinks: row.get("hardlinks")?,
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
    db.execute("INSERT INTO audit(time,object,action,actor,previous,current,outcome) VALUES(?,?,?,?,?,?,?)",
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
        fsmeta::register(&db)?;
        db.execute_batch("PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA cache_size=-32768; PRAGMA temp_store=FILE;")?;
        let version: i64 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version != LEDGER_VERSION {
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
            tx.pragma_update(None, "user_version", LEDGER_VERSION)?;
            tx.commit()?;
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
            fsmeta::populate(&mut selected);
        }
        Ok(selected)
    }

    pub(crate) fn find(&self, reference: &str) -> Result<Object> {
        let by_key = |key: &str| {
            self.db
                .prepare_cached("SELECT * FROM objects WHERE id=? AND missing=0")?
                .query_row([key], object)
                .optional()
        };
        let mut by_id = by_key(reference)?;
        if by_id.is_none()
            && let Some(alias) = lean::resolve(&self.db, reference)?
        {
            by_id = by_key(&alias)?;
        }
        let selected = match by_id {
            Some(selected) => Some(selected),
            None => {
                let path = indexed_path(Path::new(reference)).ok();
                let key = path.as_deref().and_then(Path::to_str).unwrap_or(reference);
                self.db
                    .query_row(
                        "SELECT * FROM objects WHERE path=? AND missing=0",
                        [key],
                        object,
                    )
                    .optional()?
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
                .query_map([&selected.id], |row| row.get::<_, String>(0))?
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
            "SELECT time,object,action,actor,previous,current,outcome FROM audit
             WHERE (?1 IS NULL OR object=?1 OR seq IN(
                 SELECT event FROM tag_audit_members WHERE mosh_keys_contains(keys,
                     (SELECT key FROM object_keys WHERE object=?1))
             ))
             ORDER BY seq DESC LIMIT ?2",
        )?;
        let rows = statement.query_map(
            params![selected.as_ref().map(|o| &o.id), limit.clamp(1, 1000)],
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
