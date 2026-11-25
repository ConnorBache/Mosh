use crate::{Mosh, canonical, event, fsmeta, now, path_text, prefix};
use anyhow::{Context, Result, ensure};
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::Serialize;
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant, SystemTime},
};

fn tracked_directories(db: &Connection, root: &Path) -> Result<BTreeSet<PathBuf>> {
    let roots = crate::nested::boundaries(db)?;
    let mut desired = BTreeSet::new();
    let mut query=db.prepare_cached("SELECT path_literal FROM objects WHERE missing=0 AND kind_code=1 AND reparse=0 AND path_literal IS NOT NULL
        AND mosh_within_path(path_literal,?)")?;
    for path in query.query_map([path_text(root)?], |r| {
        r.get::<_, String>(0).map(PathBuf::from)
    })? {
        let path = path?;
        if !roots
            .iter()
            .any(|boundary| crate::nested::within(&path, boundary))
        {
            desired.insert(path);
        }
    }
    Ok(desired)
}

fn directory_marks(db: &Connection, root: &Path) -> Result<BTreeMap<PathBuf, Option<SystemTime>>> {
    let mut paths = tracked_directories(db, root)?;
    paths.insert(root.to_owned());
    Ok(paths
        .into_iter()
        .filter_map(|path| {
            let meta = fs::symlink_metadata(&path).ok()?;
            (meta.is_dir() && !fsmeta::reparse(&meta)).then(|| (path, meta.modified().ok()))
        })
        .collect())
}

fn sync_watches(
    db: &Connection,
    root: &Path,
    watcher: &mut RecommendedWatcher,
    watched: &mut BTreeSet<PathBuf>,
) -> Result<()> {
    let desired = tracked_directories(db, root)?;
    for path in watched.difference(&desired) {
        let _ = watcher.unwatch(path);
    }
    let mut active = watched
        .intersection(&desired)
        .cloned()
        .collect::<BTreeSet<_>>();
    for path in desired.difference(watched) {
        if fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir() && !fsmeta::reparse(&meta)) {
            match watcher.watch(path, RecursiveMode::NonRecursive) {
                Ok(()) => {
                    active.insert(path.clone());
                }
                // Missed or unsupported subscriptions are covered by the periodic scan.
                Err(_) => {}
            }
        }
    }
    *watched = active;
    Ok(())
}

#[derive(Default, Debug, Serialize)]
pub struct ScanReport {
    pub scanned: u64,
    pub changed: u64,
    pub missing: u64,
    pub errors: u64,
    pub warnings: Vec<String>,
    pub elapsed_ms: u64,
}

impl ScanReport {
    fn warning(&mut self, error: impl ToString) {
        self.errors += 1;
        if self.warnings.len() < 20 {
            self.warnings.push(error.to_string());
        }
    }

    fn merge(&mut self, report: Self) {
        self.scanned += report.scanned;
        self.changed += report.changed;
        self.missing += report.missing;
        self.errors += report.errors;
        self.elapsed_ms += report.elapsed_ms;
        self.warnings
            .extend(report.warnings.into_iter().take(20 - self.warnings.len()));
    }
}

#[derive(Debug)]
struct Previous {
    key: i64,
    id: String,
    path: String,
    kind: String,
    native: Option<String>,
    created: Option<i64>,
    scan_stamp: Option<Vec<u8>>,
    reparse: bool,
    missing: bool,
}

fn previous(row: &rusqlite::Row<'_>) -> rusqlite::Result<Previous> {
    Ok(Previous {
        id: row.get(0)?,
        path: row.get(1)?,
        kind: row.get(2)?,
        native: fsmeta::unpack_id(row.get_ref(3)?)?,
        created: row.get(4)?,
        scan_stamp: row.get(5)?,
        reparse: row.get(6)?,
        key: row.get(7)?,
        missing: row.get(8)?,
    })
}

const STAMP: &str = "id,path,kind,native_id,created,scan_stamp,reparse,key,missing";

fn directory_identity(path: &Path, previous: &Previous) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| {
        meta.is_dir()
            && !fsmeta::reparse(&meta)
            && previous
                .created
                .is_none_or(|birth| meta.created().ok().map(crate::millis) == Some(birth))
            && previous
                .native
                .as_deref()
                .is_none_or(|id| fsmeta::native(path, &meta).id.as_deref() == Some(id))
    })
}

fn locate_root(path: &Path, previous: Option<&Previous>) -> Result<Option<PathBuf>> {
    let observed = match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() && !fsmeta::reparse(&meta) => Some(canonical(path)?),
        Ok(_) => None,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    if let Some(previous) = previous
        && previous.native.is_some()
        && previous.created.is_some()
        && observed
            .as_deref()
            .is_none_or(|path| !directory_identity(path, previous))
    {
        #[cfg(windows)]
        if let Some(found) = fsmeta::win::path_by_id(
            path,
            previous.native.as_deref().unwrap(),
            previous.created.unwrap(),
            true,
        ) && directory_identity(&found, previous)
        {
            return Ok(Some(found));
        }
        if let Some(parent) = path.parent()
            && let Ok(entries) = fs::read_dir(parent)
        {
            for entry in entries.flatten() {
                if entry.file_type().is_ok_and(|kind| kind.is_dir())
                    && directory_identity(&entry.path(), previous)
                {
                    return Ok(Some(canonical(&entry.path())?));
                }
            }
        }
    }
    Ok(observed)
}

fn forgotten(db: &Connection, path: &str, full_scan: bool) -> Result<u64> {
    let (low, high) = prefix(path);
    let unseen = if full_scan {
        "AND NOT EXISTS(SELECT 1 FROM scan_present WHERE scan_present.id=objects.key)
         AND NOT EXISTS(SELECT 1 FROM scan_unreadable WHERE mosh_within_path(objects.path,scan_unreadable.path))"
    } else {
        ""
    };
    let where_clause =
        format!("missing=0 AND path>=?1 AND path<?3 AND (path=?1 OR path>=?2) {unseen}");
    db.execute(
        &format!(
            "INSERT INTO audit(time,object,action,previous,outcome)
        SELECT ?4,key,'external-missing',json_object('path',path),'observed'
        FROM objects WHERE {where_clause}"
        ),
        params![path, low, high, now()],
    )?;
    Ok(db.execute(
        &format!("UPDATE objects SET missing=1 WHERE {where_clause}"),
        params![path, low, high],
    )? as u64)
}

fn identity(
    db: &Connection,
    path: &str,
    kind: &str,
    native: Option<&str>,
    volume: Option<i64>,
    native_ref: Option<&[u8]>,
    created: Option<i64>,
) -> Result<Option<Previous>> {
    let existing = crate::lean::path_key(db, path)?
        .map(|key| {
            db.prepare_cached(&format!("SELECT {STAMP} FROM objects WHERE key=?"))?
                .query_row([key], previous)
        })
        .transpose()?;
    if let Some(existing) = existing {
        let birth_changed = existing.created.zip(created).is_some_and(|(a, b)| a != b);
        let id_changed = existing
            .native
            .as_deref()
            .zip(native)
            .is_some_and(|(a, b)| a != b);
        if existing.kind == kind && !birth_changed && !id_changed {
            return Ok(Some(existing));
        }
        forgotten(db, path, false)?;
    }
    if let Some((volume, native_ref)) = volume.zip(native_ref) {
        let suspended=db.prepare_cached(&format!("SELECT {STAMP} FROM objects
            WHERE path=?1 AND missing=1 AND native_volume=?2 AND native_ref=?3 AND kind=?4 AND created IS ?5
            AND key IN(SELECT object_key FROM nested_suspended WHERE child IS NULL) LIMIT 2"))?
            .query_map(params![path, volume, native_ref, kind, created],previous)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if suspended.len() == 1 {
            return Ok(suspended.into_iter().next());
        }
    }
    let Some(((volume, native_ref), created)) = volume.zip(native_ref).zip(created) else {
        return Ok(None);
    };
    let mut stmt = db.prepare_cached(&format!("SELECT {STAMP} FROM objects WHERE native_volume=? AND native_ref=? AND created=? AND kind=? AND missing=0 LIMIT 3"))?;
    let candidates = stmt
        .query_map(params![volume, native_ref, created, kind], previous)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if !candidates.is_empty() {
        if candidates.len() != 1 {
            return Ok(None);
        }
        let candidate = candidates.into_iter().next().unwrap();
        let old = Path::new(&candidate.path);
        let moved = match fs::symlink_metadata(old) {
            Ok(meta) => {
                crate::indexed_path(old)
                    .is_ok_and(|resolved| resolved == Path::new(path) && old != resolved)
                    || fsmeta::native(old, &meta)
                        .id
                        .as_deref()
                        .is_some_and(|id| Some(id) != candidate.native.as_deref())
            }
            Err(error) => error.kind() == std::io::ErrorKind::NotFound,
        };
        return Ok(moved.then_some(candidate));
    }
    // The source notification can be delivered and committed before Windows reports
    // the destination. A unique native identity and birth time still identify the row.
    let missing = db
        .prepare_cached(&format!(
            "SELECT {STAMP} FROM objects o WHERE missing=1
        AND native_volume=? AND native_ref=? AND created=? AND kind=?
        AND NOT EXISTS(SELECT 1 FROM nested_suspended WHERE object_key=o.key) LIMIT 2"
        ))?
        .query_map(params![volume, native_ref, created, kind], previous)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(if missing.len() == 1 {
        missing.into_iter().next()
    } else {
        None
    })
}

fn deterministic_tags(extension: &str, kind: &str, mime: Option<&str>) -> Vec<String> {
    let mut tags = vec![kind.to_owned()];
    if !extension.is_empty() && extension.len() < 120 {
        tags.push(extension.to_owned());
    }
    if let Some((family, _)) = mime.and_then(|name| name.split_once('/')) {
        tags.push(format!("media-{family}"));
    }
    let language = match extension {
        "rs" => "rust",
        "ts" | "tsx" => "typescript",
        "js" | "jsx" => "javascript",
        "py" => "python",
        "ps1" => "powershell",
        "cs" => "csharp",
        "c" | "h" => "c",
        "cpp" | "hpp" => "cpp",
        "go" => "golang",
        "java" => "java",
        "rb" => "ruby",
        "sql" => "sql",
        _ => "",
    };
    if !language.is_empty() {
        tags.push("source-code".to_owned());
        if language != extension {
            tags.push(language.to_owned());
        }
    }
    tags
}

fn index_one(
    db: &Connection,
    tag_writer: &mut crate::packed_tags::Writer<'_>,
    path: &Path,
    full_scan: bool,
    actor: &str,
) -> Result<bool> {
    let meta = fs::symlink_metadata(path)?;
    let key = path_text(path)?;
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or(key);
    let is_reparse = fsmeta::reparse(&meta);
    let kind = if meta.is_dir() { "coll" } else { "file" };
    let extension = crate::lean::file_extension(name, kind == "file");
    let native = fsmeta::native(path, &meta);
    let (size, modified, created) = fsmeta::revision_stamp(&meta);
    let size: i64 = if kind == "file" {
        i64::try_from(size).context("file size exceeds SQLite range")?
    } else {
        0
    };
    let (volume, file_id) = fsmeta::native_parts(db, native.id.as_deref())?;
    let stamp = fsmeta::scan_stamp(size, modified, native.changed);
    let previous = identity(
        db,
        key,
        kind,
        native.id.as_deref(),
        volume,
        file_id.as_deref(),
        created,
    )?;
    let changed = previous.as_ref().is_none_or(|old| {
        old.scan_stamp.as_deref() != Some(stamp.as_slice()) || old.reparse != is_reparse
    });
    let renamed = previous.as_ref().is_some_and(|old| old.path != key);
    let new = previous.is_none();
    let (object_key, id) = match &previous {
        Some(previous) => (previous.key, previous.id.clone()),
        None => {
            let key = crate::lean::next_key(db)?;
            (key, crate::lean::encode_key(key as u64))
        }
    };
    let link_target = is_reparse
        .then(|| fs::read_link(path).ok())
        .flatten()
        .and_then(|p| p.to_str().map(str::to_owned));
    let (folder, literal) = crate::lean::store_location(db, key, name, kind == "file")?;
    let parent = if folder.is_none() {
        path.parent()
            .and_then(Path::to_str)
            .map(|parent| crate::lean::parent_coll_key(db, parent))
            .transpose()?
            .flatten()
    } else {
        None
    };
    db.prepare_cached("INSERT INTO objects(key,kind_code,path_literal,parent_override,name,path_folder,scan_stamp,created,native_volume,native_ref,reparse,link_target)
        VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)
        ON CONFLICT(key) DO UPDATE SET path_literal=?3,name=?5,path_folder=?6,scan_stamp=?7,
        parent_override=CASE WHEN path_folder IS ?6 AND ?6 IS NOT NULL THEN parent_override ELSE ?4 END,
        created=coalesce(?8,created),native_volume=coalesce(?9,native_volume),native_ref=coalesce(?10,native_ref),
        reparse=?11,link_target=?12,revision=revision+?13,missing=0,
        hash=CASE WHEN ?13=1 THEN NULL ELSE hash END")?
        .execute(params![object_key,i64::from(meta.is_dir()),literal,parent,name,folder,stamp.as_slice(),created,
            volume,file_id,is_reparse,link_target,changed])?;
    if full_scan {
        db.prepare_cached("INSERT OR IGNORE INTO scan_present VALUES(?)")?
            .execute([object_key])?;
    }
    db.prepare_cached("DELETE FROM nested_suspended WHERE child IS NULL AND object_key=?")?
        .execute([object_key])?;
    if new && kind == "coll" {
        db.prepare_cached("INSERT INTO owners VALUES(?,?)")?
            .execute(params![object_key, actor])?;
        let (low, high) = prefix(key);
        db.prepare_cached(
            "UPDATE objects SET parent_override=?1 WHERE parent_override IS NULL
            AND path_literal IS NOT NULL AND path_literal>=?2 AND path_literal<?3 AND missing=0
            AND instr(substr(path_literal,length(?2)+1),?4)=0",
        )?
        .execute(params![
            object_key,
            low,
            high,
            std::path::MAIN_SEPARATOR_STR
        ])?;
    }
    let restored = previous.as_ref().is_some_and(|old| old.missing);
    if new || renamed {
        let ids = deterministic_tags(&extension, kind, fsmeta::mime(Some(&extension)))
            .iter()
            .map(|name| crate::organization::dictionary_id(db, name))
            .collect::<Result<Vec<_>>>()?;
        tag_writer.filesystem(object_key, &ids)?;
    }
    if kind == "file" && (changed || renamed) {
        if changed && !new {
            let revision: i64 = db
                .prepare_cached("SELECT revision FROM objects WHERE key=?")?
                .query_row([object_key], |row| row.get(0))?;
            tag_writer.invalidate_enrichment(object_key, revision - 1)?;
        }
        crate::tiers::indexed(db, object_key)?;
    }
    if renamed {
        let before = previous.map(|old| json!({"path":old.path}));
        event(
            db,
            Some(&id),
            "external-move",
            None,
            before.as_ref(),
            Some(&json!({"path":key})),
            "observed",
        )?;
    }
    Ok(new || changed || renamed || restored)
}

fn in_storage(path: &Path, storage: &[PathBuf]) -> bool {
    storage
        .iter()
        .any(|reserved| crate::nested::within(path, reserved))
}

fn skip(path: &Path, db: &Path, storage: &[PathBuf]) -> bool {
    path == db
        || in_storage(path, storage)
        || path
            .file_name()
            .and_then(|p| p.to_str())
            .is_some_and(|name| {
                name == ".git"
                    || name == ".mosh-quarantine"
                    || name.starts_with(".mosh-copy-")
                    || (path.parent() == db.parent()
                        && db.file_name().and_then(|p| p.to_str()).is_some_and(|file| {
                            name.strip_prefix(file).is_some_and(|suffix| {
                                matches!(suffix, "-wal" | "-shm" | "-journal")
                            })
                        }))
            })
}

impl Mosh {
    /// Exclude the application's workspace catalog, default Ledger and its sidecars.
    pub fn set_scan_storage(&self, directory: &Path, default_ledger: &Path) -> Result<()> {
        self.admin()?;
        let mut paths = vec![canonical(directory)?];
        let parent = default_ledger
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let parent = canonical(parent)?;
        let filename = default_ledger
            .file_name()
            .context("the default Ledger has no filename")?;
        for suffix in ["", "-wal", "-shm", "-journal"] {
            let mut name = filename.to_owned();
            name.push(suffix);
            paths.push(parent.join(name));
        }
        self.db.execute("INSERT INTO metadata VALUES('scan_storage',?1)
            ON CONFLICT(key) DO UPDATE SET value=excluded.value WHERE metadata.value<>excluded.value",
            [serde_json::to_string(&paths)?])?;
        Ok(())
    }

    fn scan_storage(&self) -> Result<Vec<PathBuf>> {
        let stored = self
            .db
            .prepare_cached("SELECT value FROM metadata WHERE key='scan_storage'")?
            .query_row([], |row| row.get::<_, String>(0))
            .optional()?;
        Ok(stored
            .map(|text| serde_json::from_str(&text))
            .transpose()?
            .unwrap_or_default())
    }

    pub fn registered_roots(&self) -> Result<Vec<PathBuf>> {
        self.admin()?;
        Ok(self
            .db
            .prepare_cached("SELECT path FROM roots ORDER BY path")?
            .query_map([], |row| row.get::<_, String>(0).map(PathBuf::from))?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn scan(&self, root: &Path, cancel: &AtomicBool) -> Result<ScanReport> {
        self.admin()?;
        let meta = fs::symlink_metadata(root)?;
        ensure!(
            meta.is_dir() && !fsmeta::reparse(&meta),
            "choose a physical directory to index"
        );
        let root = canonical(root)?;
        self.outside_children(&root)?;
        let storage = self.scan_storage()?;
        ensure!(
            !in_storage(&root, &storage),
            "choose a directory outside the Mosh data folder"
        );
        let tx = Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO roots VALUES(?,?) ON CONFLICT DO UPDATE SET scanned=excluded.scanned",
            params![path_text(&root)?, now()],
        )?;
        let report = self.walk(&tx, &root, cancel)?;
        tx.commit()?;
        self.apply_rules()?;
        Ok(report)
    }

    fn walk(&self, db: &Connection, root: &Path, cancel: &AtomicBool) -> Result<ScanReport> {
        let started = Instant::now();
        let boundaries = crate::nested::boundaries(db)?;
        let boundary_ids = crate::nested::boundary_ids(db)?;
        let storage = self.scan_storage()?;
        db.execute_batch(
            "CREATE TEMP TABLE IF NOT EXISTS scan_present(id INTEGER PRIMARY KEY) WITHOUT ROWID;
            CREATE TEMP TABLE IF NOT EXISTS scan_unreadable(path TEXT PRIMARY KEY) WITHOUT ROWID;
            DELETE FROM scan_present; DELETE FROM scan_unreadable;",
        )?;
        let mut report = ScanReport::default();
        let mut tag_writer = crate::packed_tags::Writer::new(db);
        let mut repos = BTreeSet::new();
        if let Some(repo) = crate::git::root(root) {
            repos.insert(repo);
        }
        let mut walker = walkdir::WalkDir::new(root)
            .follow_links(false)
            .follow_root_links(false)
            .max_open(32)
            .into_iter();
        while let Some(entry) = walker.next() {
            ensure!(!cancel.load(Ordering::Relaxed), "scan cancelled");
            let entry = match entry {
                Ok(item) => item,
                Err(error) => {
                    if error
                        .io_error()
                        .is_none_or(|io| io.kind() != std::io::ErrorKind::NotFound)
                    {
                        let path = error
                            .path()
                            .and_then(Path::to_str)
                            .unwrap_or(path_text(root)?);
                        db.execute("INSERT OR IGNORE INTO scan_unreadable VALUES(?)", [path])?;
                        report.warning(error);
                    }
                    continue;
                }
            };
            let path = entry.path();
            if boundaries
                .iter()
                .any(|root| crate::nested::within(path, root))
            {
                if entry.file_type().is_dir() {
                    walker.skip_current_dir();
                }
                continue;
            }
            if path.to_str().is_none() {
                report.warning(format!(
                    "{}: the path cannot be represented as Unicode",
                    path.display()
                ));
                continue;
            }
            if path != root && skip(path, &self.db_path, &storage) {
                if entry.file_name() == ".git" {
                    if let Some(parent) = path.parent() {
                        repos.insert(parent.to_owned());
                    }
                }
                if entry.file_type().is_dir() {
                    walker.skip_current_dir();
                }
                continue;
            }
            match fs::symlink_metadata(path) {
                Ok(meta)
                    if entry.file_type().is_dir()
                        && crate::nested::moved_boundary(path, &meta, &boundary_ids) =>
                {
                    walker.skip_current_dir();
                    continue;
                }
                Ok(meta) if fsmeta::reparse(&meta) && entry.file_type().is_dir() => {
                    walker.skip_current_dir()
                }
                Ok(_) => {}
                Err(error) => {
                    if error.kind() != std::io::ErrorKind::NotFound {
                        db.execute(
                            "INSERT OR IGNORE INTO scan_unreadable VALUES(?)",
                            [path_text(path)?],
                        )?;
                        report.warning(error);
                    }
                    continue;
                }
            }
            match index_one(db, &mut tag_writer, path, true, &self.actor) {
                Ok(changed) => {
                    report.scanned += 1;
                    report.changed += u64::from(changed);
                }
                Err(error) if error.is::<std::io::Error>() => {
                    if error
                        .downcast_ref::<std::io::Error>()
                        .is_none_or(|io| io.kind() != std::io::ErrorKind::NotFound)
                    {
                        db.execute(
                            "INSERT OR IGNORE INTO scan_unreadable VALUES(?)",
                            [path_text(path)?],
                        )?;
                        report.warning(format!("{}: {error:#}", path.display()));
                    }
                }
                Err(error) => return Err(error.context(format!("indexing {}", path.display()))),
            }
        }
        tag_writer.flush()?;
        report.missing = forgotten(db, path_text(root)?, true)?;
        db.execute_batch("DELETE FROM scan_present; DELETE FROM scan_unreadable;")?;
        for repo in repos {
            if boundaries
                .iter()
                .any(|boundary| crate::nested::within(boundary, &repo))
            {
                continue;
            }
            if let Err(error) = crate::git::refresh(db, &repo) {
                report.warning(format!("Git {}: {error:#}", repo.display()));
            }
        }
        report.elapsed_ms = started.elapsed().as_millis() as u64;
        Ok(report)
    }

    fn apply_rules(&self) -> Result<()> {
        let mut statement = self
            .db
            .prepare("SELECT tag,query FROM rules ORDER BY tag,query")?;
        let rows = statement
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        if rows.is_empty() {
            return Ok(());
        }
        let tx = Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)?;
        let mut writer = crate::packed_tags::Writer::new(&tx);
        for (tag, query) in rows {
            let tag = crate::organization::dictionary_id(&tx, &tag)?;
            let origin = crate::packed_tags::origin_bit("automatic")?;
            let mut offset = 0;
            loop {
                let page = self.search(
                    &query,
                    &crate::SearchOptions {
                        limit: 1000,
                        offset,
                        ..Default::default()
                    },
                )?;
                if page.is_empty() {
                    break;
                }
                for item in &page {
                    if let Err(error) =
                        writer.assign(&item.id, tag, origin, Some((1.0, Some(&query))))
                    {
                        return Err(error);
                    }
                }
                writer.flush()?;
                offset += page.len();
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn refresh_path(&self, path: &Path, cancel: &AtomicBool) -> Result<ScanReport> {
        self.admin()?;
        ensure!(!cancel.load(Ordering::Relaxed), "scan cancelled");
        let path = crate::indexed_path(path).unwrap_or_else(|_| path.to_owned());
        let path = path.as_path();
        let starts = self.registered_roots()?;
        ensure!(
            starts.iter().any(|root| crate::nested::within(path, root)),
            "path is outside registered roots"
        );
        let started = Instant::now();
        let tx = Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)?;
        self.outside_children(path)?;
        let report = match fs::symlink_metadata(path) {
            Ok(_) if skip(path, &self.db_path, &self.scan_storage()?) => ScanReport::default(),
            Ok(meta) if meta.is_dir() && !fsmeta::reparse(&meta) => self.walk(&tx, path, cancel)?,
            Ok(_) => {
                let mut writer = crate::packed_tags::Writer::new(&tx);
                let changed = index_one(&tx, &mut writer, path, false, &self.actor)?;
                writer.flush()?;
                if let Some(repo) = crate::git::root(path) {
                    if !crate::nested::boundaries(&tx)?
                        .iter()
                        .any(|boundary| crate::nested::within(boundary, &repo))
                    {
                        crate::git::refresh(&tx, &repo)?;
                    }
                }
                ScanReport {
                    scanned: 1,
                    changed: u64::from(changed),
                    elapsed_ms: started.elapsed().as_millis() as u64,
                    ..Default::default()
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => ScanReport {
                missing: forgotten(&tx, path_text(path)?, false)?,
                ..Default::default()
            },
            Err(error) => return Err(error.into()),
        };
        tx.commit()?;
        Ok(report)
    }

    fn root_record(&self, root: &Path) -> Result<Option<Previous>> {
        Ok(self
            .db
            .prepare_cached(&format!(
                "SELECT {STAMP} FROM objects
            WHERE kind_code=1 AND path_literal=? ORDER BY missing,key DESC LIMIT 1"
            ))?
            .query_row([path_text(root)?], previous)
            .optional()?)
    }

    fn move_root(&self, old: &Path, new: &Path, cancel: &AtomicBool) -> Result<ScanReport> {
        self.outside_children(new)?;
        let tx = Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)?;
        tx.execute("DELETE FROM roots WHERE path=?", [path_text(old)?])?;
        tx.execute("INSERT INTO roots VALUES(?,?) ON CONFLICT(path) DO UPDATE SET scanned=excluded.scanned",
            params![path_text(new)?,now()])?;
        let mut result = self.walk(&tx, new, cancel)?;
        if !crate::nested::within(new, old) {
            result.missing += forgotten(&tx, path_text(old)?, false)?;
        }
        tx.commit()?;
        self.apply_rules()?;
        Ok(result)
    }

    fn refresh_changes(
        &self,
        paths: BTreeSet<PathBuf>,
        cancel: &AtomicBool,
        mut report: impl FnMut(Result<ScanReport>),
    ) {
        if paths.is_empty() {
            return;
        }
        let (present, missing): (Vec<_>, Vec<_>) = paths
            .into_iter()
            .partition(|path| fs::symlink_metadata(path).is_ok());
        let mut batch = ScanReport::default();
        for path in present.into_iter().chain(missing) {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            match self.refresh_path(&path, cancel) {
                Ok(result) => batch.merge(result),
                Err(error) => batch.warning(format!("{}: {error:#}", path.display())),
            }
        }
        report(Ok(batch));
    }

    pub fn watch(
        &self,
        root: &Path,
        cancel: &AtomicBool,
        mut report: impl FnMut(Result<ScanReport>),
    ) -> Result<()> {
        self.admin()?;
        let mut root = if self
            .registered_roots()?
            .iter()
            .any(|registered| registered == root)
        {
            root.to_owned()
        } else {
            canonical(root)?
        };
        self.outside_children(&root)?;
        let storage = self.scan_storage()?;
        let (send, recv) = mpsc::sync_channel::<notify::Result<Event>>(512);
        let overflow = std::sync::Arc::new(AtomicBool::new(false));
        let watcher_overflow = overflow.clone();
        // Windows directory subscriptions keep handles open and can block renaming a
        // parent directory. Poll metadata there and reserve subscriptions for Unix.
        let mut watcher = if cfg!(windows) {
            None
        } else {
            Some(RecommendedWatcher::new(
                move |event| {
                    if matches!(send.try_send(event), Err(mpsc::TrySendError::Full(_))) {
                        watcher_overflow.store(true, Ordering::Relaxed);
                    }
                },
                notify::Config::default(),
            )?)
        };
        let mut watched = BTreeSet::<PathBuf>::new();
        let mut marks = BTreeMap::new();
        let mut mark_at = Instant::now();
        let mut anchor = self.root_record(&root)?;
        let mut available = None;
        let mut probe = Instant::now();
        let mut rescan_at = Instant::now();
        let mut boundary_version = 0;
        while !cancel.load(Ordering::Relaxed) {
            if Instant::now() >= probe {
                probe = Instant::now() + Duration::from_secs(1);
                let located = locate_root(&root, anchor.as_ref())?;
                let located = located.filter(|path| {
                    self.outside_children(path).is_ok() && !in_storage(path, &storage)
                });
                if let Some(located) = located {
                    let moved = root != located;
                    let replaced = anchor
                        .as_ref()
                        .is_some_and(|old| !directory_identity(&root, old));
                    if moved || available != Some(true) || replaced {
                        if let Some(watcher) = watcher.as_mut() {
                            if moved || replaced {
                                for path in std::mem::take(&mut watched) {
                                    let _ = watcher.unwatch(&path);
                                }
                            }
                            if !watched.contains(&located)
                                && watcher.watch(&located, RecursiveMode::NonRecursive).is_ok()
                            {
                                watched.insert(located.clone());
                            }
                            if !moved && !replaced {
                                sync_watches(&self.db, &root, watcher, &mut watched)?;
                            }
                        } else {
                            marks = directory_marks(&self.db, &located)?;
                        }
                        let result = if moved {
                            self.move_root(&root, &located, cancel)
                        } else {
                            self.scan(&root, cancel)
                        };
                        if result.is_ok() {
                            root = located;
                            anchor = self.root_record(&root)?;
                            available = Some(true);
                            rescan_at = Instant::now() + Duration::from_secs(60);
                        }
                        report(result);
                        if let Some(watcher) = watcher.as_mut() {
                            sync_watches(&self.db, &root, watcher, &mut watched)?;
                        }
                    }
                } else if available != Some(false) {
                    if let Some(watcher) = watcher.as_mut() {
                        for path in std::mem::take(&mut watched) {
                            let _ = watcher.unwatch(&path);
                        }
                    }
                    let tx = Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)?;
                    let missing = forgotten(&tx, path_text(&root)?, false)?;
                    tx.commit()?;
                    available = Some(false);
                    report(Ok(ScanReport {
                        missing,
                        ..Default::default()
                    }));
                }
            }
            let Some(watcher) = watcher.as_mut() else {
                if available == Some(true) && !root.is_dir() {
                    probe = Instant::now();
                } else if available == Some(true) && Instant::now() >= mark_at {
                    mark_at = Instant::now() + Duration::from_millis(500);
                    if Instant::now() >= rescan_at {
                        report(self.scan(&root, cancel));
                        rescan_at = Instant::now() + Duration::from_secs(60);
                    } else {
                        let next = directory_marks(&self.db, &root)?;
                        let mut changed = BTreeSet::new();
                        for path in marks.keys().chain(next.keys()) {
                            if marks.get(path) != next.get(path) {
                                changed.insert(path.clone());
                            }
                        }
                        marks = next;
                        let mut targets = BTreeSet::<PathBuf>::new();
                        for path in changed {
                            if !path
                                .ancestors()
                                .skip(1)
                                .any(|parent| targets.contains(parent))
                            {
                                targets.insert(path);
                            }
                        }
                        self.refresh_changes(targets, cancel, &mut report);
                    }
                }
                std::thread::sleep(Duration::from_millis(250));
                continue;
            };
            if available != Some(true) {
                let _ = recv.recv_timeout(Duration::from_millis(250));
                continue;
            }
            let version = self
                .db
                .pragma_query_value(None, "data_version", |r| r.get::<_, i64>(0))?;
            if version != boundary_version {
                sync_watches(&self.db, &root, watcher, &mut watched)?;
                boundary_version = version;
            }
            let boundaries = crate::nested::boundaries(&self.db)?;
            let mut paths = BTreeSet::<PathBuf>::new();
            let mut rescan = overflow.swap(false, Ordering::Relaxed) || Instant::now() >= rescan_at;
            let first = recv.recv_timeout(Duration::from_millis(250));
            if let Ok(first) = first {
                let interval = Instant::now();
                for event in std::iter::once(first).chain(std::iter::from_fn(|| {
                    (interval.elapsed() < Duration::from_millis(200))
                        .then(|| recv.recv_timeout(Duration::from_millis(30)).ok())
                        .flatten()
                })) {
                    match event {
                        Ok(event) if !event.need_rescan() => {
                            if matches!(event.kind, notify::EventKind::Access(_)) {
                                continue;
                            }
                            for path in event.paths {
                                let Ok(relative) = path.strip_prefix(&root) else {
                                    continue;
                                };
                                if relative.as_os_str().is_empty() {
                                    continue;
                                }
                                if boundaries
                                    .iter()
                                    .any(|boundary| crate::nested::within(&path, boundary))
                                    || relative.components().any(|p| {
                                        p.as_os_str() == ".git"
                                            || p.as_os_str() == ".mosh-quarantine"
                                    })
                                    || skip(&path, &self.db_path, &storage)
                                {
                                    continue;
                                }
                                paths.insert(path);
                            }
                        }
                        _ => rescan = true,
                    }
                }
            }
            if !root.is_dir() {
                probe = Instant::now();
                continue;
            }
            if rescan {
                report(self.scan(&root, cancel));
                rescan_at =
                    Instant::now() + Duration::from_secs(if watched.is_empty() { 5 } else { 60 });
                sync_watches(&self.db, &root, watcher, &mut watched)?;
                continue;
            }
            if !paths.is_empty() {
                self.refresh_changes(paths, cancel, &mut report);
                sync_watches(&self.db, &root, watcher, &mut watched)?;
            }
        }
        Ok(())
    }
}
