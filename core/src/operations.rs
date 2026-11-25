use crate::{Mosh, SearchOptions, Sort, canonical, event, fsmeta, now, path_text, uuid};
use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    fs::{self, File, FileTimes},
    io,
    path::{Component, Path},
    sync::atomic::{AtomicBool, Ordering},
};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Copy,
    Move,
    Quarantine,
    Tag,
    Marriage,
    Manifest,
}

#[derive(Deserialize, Serialize)]
pub struct Manifest {
    pub id: String,
    pub action: Action,
    pub query: String,
    pub argument: Option<String>,
    pub entries: Vec<Entry>,
    pub total_bytes: u64,
    pub added_logical_bytes: u64,
    pub conflicts: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Deserialize, Serialize)]
pub struct Entry {
    pub object: String,
    pub source: String,
    pub destination: Option<String>,
    pub bytes: i64,
    pub revision: i64,
    native: Option<String>,
    modified: Option<i64>,
    created: Option<i64>,
}

impl Action {
    fn physical(self) -> bool {
        matches!(self, Self::Copy | Self::Move | Self::Quarantine)
    }
}

fn managed(path: &Path, roots: &[String]) -> Result<()> {
    ensure!(path.is_absolute(), "an absolute path is required");
    ensure!(
        roots.iter().any(|root| path.starts_with(root)),
        "path is outside the registered roots: {}",
        path.display()
    );
    for var in [
        "WINDIR",
        "ProgramFiles",
        "ProgramFiles(x86)",
        "ProgramW6432",
        "ProgramData",
    ] {
        if let Some(protected) = std::env::var_os(var).and_then(|p| canonical(Path::new(&p)).ok()) {
            let path = path.as_os_str().to_string_lossy().to_lowercase();
            let protected = protected.as_os_str().to_string_lossy().to_lowercase();
            ensure!(
                path != protected
                    && !path.starts_with(&(protected + std::path::MAIN_SEPARATOR_STR)),
                "system locations cannot be modified"
            );
        }
    }
    Ok(())
}

fn parent_safe(path: &Path, roots: &[String]) -> Result<()> {
    managed(path, roots)?;
    let parent = path.parent().context("path has no parent")?;
    ensure!(
        canonical(parent)? == parent,
        "parent directory changed: {}",
        parent.display()
    );
    for ancestor in parent.ancestors() {
        let meta = fs::symlink_metadata(ancestor)?;
        ensure!(
            meta.is_dir() && !fsmeta::reparse(&meta),
            "path contains a reparse point or non-directory"
        );
    }
    Ok(())
}

fn available(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
        Ok(_) => anyhow::bail!("destination already exists: {}", path.display()),
    }
}

#[cfg(windows)]
fn open_move(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ};
    File::options()
        .access_mode(0x80010000)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

#[cfg(windows)]
fn guard_destination(destination: &Path) -> Result<Vec<File>> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::*;
    let mut dirs = destination
        .parent()
        .context("destination requires a parent")?
        .ancestors()
        .filter(|p| p.is_absolute())
        .collect::<Vec<_>>();
    dirs.reverse();
    let mut handles = Vec::with_capacity(dirs.len());
    for dir in dirs {
        let handle = File::options()
            .access_mode(FILE_READ_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(dir)?;
        let meta = handle.metadata()?;
        ensure!(
            meta.is_dir() && !fsmeta::reparse(&meta),
            "destination directory changed"
        );
        handles.push(handle);
    }
    Ok(handles)
}

#[cfg(not(windows))]
fn guard_destination(_destination: &Path) -> Result<Vec<File>> {
    Ok(Vec::new())
}

#[cfg(windows)]
fn rename_open(file: &File, _source: &Path, destination: &Path) -> Result<()> {
    use std::{
        mem::{offset_of, size_of},
        os::windows::{ffi::OsStrExt, io::AsRawHandle},
    };
    use windows_sys::Win32::Storage::FileSystem::*;
    let _guards = guard_destination(destination)?;
    let name: Vec<u16> = destination.as_os_str().encode_wide().collect();
    let start = offset_of!(FILE_RENAME_INFO, FileName);
    let length = (start + (name.len() + 1) * 2).max(size_of::<FILE_RENAME_INFO>());
    let mut storage = vec![0u64; length.div_ceil(size_of::<u64>())];
    let info = storage.as_mut_ptr().cast::<FILE_RENAME_INFO>();
    // A zero flags field refuses replacement. The directory handles prevent path replacement.
    unsafe {
        (*info).FileNameLength = (name.len() * 2) as u32;
        std::ptr::copy_nonoverlapping(
            name.as_ptr(),
            storage.as_mut_ptr().cast::<u8>().add(start).cast(),
            name.len(),
        );
        if SetFileInformationByHandle(
            file.as_raw_handle().cast(),
            FileRenameInfo,
            info.cast(),
            length as u32,
        ) == 0
        {
            return Err(io::Error::last_os_error())
                .context("moving without replacing an existing file");
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn open_move(path: &Path) -> io::Result<File> {
    fsmeta::open_snapshot(path)
}

#[cfg(not(windows))]
fn rename_open(_file: &File, source: &Path, destination: &Path) -> Result<()> {
    fs::hard_link(source, destination)?;
    fs::remove_file(source)?;
    Ok(())
}

fn digest(path: &Path) -> Result<String> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_file() && !fsmeta::reparse(&metadata),
        "expected an ordinary file"
    );
    fsmeta::hash(&mut fsmeta::open_snapshot(path)?).map_err(Into::into)
}

fn same_path(a: &Path, b: &Path) -> bool {
    #[cfg(windows)]
    {
        a.as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(&b.as_os_str().to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        a == b
    }
}

fn original_matches(entry: &Entry, path: &Path) -> Result<()> {
    let (size, modified, created, id) = fsmeta::hard_stamp(path)?;
    ensure!(
        entry.bytes as u64 == size
            && entry.modified == modified
            && entry.created == created
            && entry
                .native
                .as_ref()
                .is_none_or(|old| id.as_ref() == Some(old)),
        "source changed since preview: {}",
        path.display()
    );
    Ok(())
}

fn copy_verified(source: &mut File, destination: &Path, expected: &str) -> Result<()> {
    use std::io::{Seek, SeekFrom};
    let _guards = guard_destination(destination)?;
    let mut temp = tempfile::Builder::new()
        .prefix(".mosh-copy-")
        .tempfile_in(destination.parent().unwrap())?;
    source.seek(SeekFrom::Start(0))?;
    io::copy(source, temp.as_file_mut())?;
    let metadata = source.metadata()?;
    let mut times = FileTimes::new();
    if let Ok(time) = metadata.modified() {
        times = times.set_modified(time);
    }
    if let Ok(time) = metadata.accessed() {
        times = times.set_accessed(time);
    }
    temp.as_file().set_times(times)?;
    temp.as_file().sync_all()?;
    ensure!(
        fsmeta::hash(temp.as_file_mut())? == expected,
        "copied bytes failed integrity check"
    );
    let output = temp.persist_noclobber(destination)?;
    output.set_permissions(metadata.permissions())?;
    Ok(())
}

impl Mosh {
    pub fn plan(
        &self,
        query: &str,
        action: Action,
        argument: Option<&str>,
        selected: &[String],
    ) -> Result<Manifest> {
        if action.physical() {
            self.admin()?;
        }
        ensure!(
            action == Action::Manifest || !query.trim().is_empty() || !selected.is_empty(),
            "provide a selection or a nonempty query"
        );
        let roots = self.roots()?;
        let id = uuid();
        let mut plan = Manifest {
            id,
            action,
            query: query.to_owned(),
            argument: argument.map(str::to_owned),
            entries: vec![],
            total_bytes: 0,
            added_logical_bytes: 0,
            conflicts: vec![],
            warnings: vec![],
        };
        let target = if matches!(action, Action::Copy | Action::Move) {
            let raw = Path::new(argument.context("choose a destination directory")?);
            ensure!(
                !fsmeta::reparse(&fs::symlink_metadata(raw)?),
                "destination cannot be a reparse point"
            );
            let directory = canonical(raw)?;
            let meta = fs::symlink_metadata(&directory)?;
            ensure!(
                meta.is_dir() && !fsmeta::reparse(&meta),
                "destination must be a physical directory"
            );
            managed(&directory, &roots)?;
            Some(directory)
        } else {
            None
        };
        if matches!(action, Action::Tag | Action::Marriage) {
            ensure!(
                argument.is_some_and(|arg| !arg.trim().is_empty()),
                "a tag or Marriage name is required"
            );
        }
        if action == Action::Tag {
            crate::organization::tag_name(argument.unwrap())?;
        }
        let mut destinations = HashSet::new();
        let mut included = HashSet::new();
        let mut offset = 0;
        loop {
            let page = if selected.is_empty() {
                self.search(
                    query,
                    &SearchOptions {
                        limit: 1000,
                        offset,
                        sort: Sort::Path,
                        ..Default::default()
                    },
                )?
            } else {
                selected
                    .iter()
                    .skip(offset)
                    .take(1000)
                    .map(|id| self.get(id))
                    .collect::<Result<Vec<_>>>()?
            };
            if page.is_empty() {
                break;
            }
            offset += page.len();
            for item in page {
                if !included.insert(item.id.clone()) {
                    continue;
                }
                ensure!(
                    plan.entries.len() < 25_000,
                    "a Cleave plan may contain up to 25,000 entries"
                );
                let source = item.path.clone().unwrap_or_else(|| item.name.clone());
                let path = Path::new(&source);
                let mut destination = None;
                if action.physical() {
                    if item.kind != "file" || item.reparse || item.path.is_none() {
                        plan.conflicts.push(format!(
                            "{source}: select ordinary files for this operation"
                        ));
                    } else if let Err(error) = parent_safe(path, &roots).and_then(|_| {
                        original_matches(
                            &Entry {
                                object: item.id.clone(),
                                source: source.clone(),
                                destination: None,
                                bytes: item.size,
                                revision: item.revision,
                                native: item.native_id.clone(),
                                modified: item.modified,
                                created: item.created,
                            },
                            path,
                        )
                    }) {
                        plan.conflicts.push(error.to_string());
                    }
                    if item.git_root.is_some() && action != Action::Copy {
                        plan.warnings.push(format!("{source}: moving a Git working-tree file can break references or change Git status"));
                    }
                    let dest = if let Some(target) = &target {
                        Some(target.join(path.file_name().context("missing file name")?))
                    } else {
                        let root = roots
                            .iter()
                            .filter(|root| path.starts_with(root))
                            .max_by_key(|root| root.len());
                        root.map(|root| {
                            Path::new(root)
                                .join(".mosh-quarantine")
                                .join(&plan.id)
                                .join(&item.id)
                                .join(path.file_name().unwrap_or_default())
                        })
                    };
                    if let Some(dest) = dest {
                        let key = path_text(&dest)?.to_owned();
                        if same_path(path, &dest)
                            || !destinations.insert(if cfg!(windows) {
                                key.to_lowercase()
                            } else {
                                key.clone()
                            })
                            || available(&dest).is_err()
                        {
                            plan.conflicts.push(format!(
                                "{source}: destination is occupied or repeated: {key}"
                            ));
                        }
                        destination = Some(key);
                    }
                } else if action == Action::Tag {
                    self.editable(&item)?;
                }
                plan.total_bytes = plan.total_bytes.saturating_add(item.size as u64);
                plan.entries.push(Entry {
                    object: item.id,
                    source,
                    destination,
                    bytes: item.size,
                    revision: item.revision,
                    native: item.native_id,
                    modified: item.modified,
                    created: item.created,
                });
            }
        }
        if action == Action::Copy {
            plan.added_logical_bytes = plan.total_bytes;
        }
        if action == Action::Move {
            plan.warnings
                .push("Moves require the source and destination to share a volume.".into());
        }
        if action == Action::Copy {
            plan.warnings.push("Copy verifies the main stream and preserves basic permissions and times. Extended metadata can differ.".into());
        }
        ensure!(!plan.entries.is_empty(), "the selection is empty");
        self.db.execute(
            "INSERT INTO plans VALUES(?,?,?,'planned',?,?)",
            params![
                plan.id,
                self.actor,
                serde_json::to_string(&action)?,
                now(),
                serde_json::to_string(&plan)?
            ],
        )?;
        Ok(plan)
    }

    fn load_plan(&self, id: &str) -> Result<(Manifest, String)> {
        let (manifest, state) = self
            .db
            .query_row(
                "SELECT manifest,state FROM plans WHERE id=? AND actor=?",
                params![id, self.actor],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?
            .context("plan not found")?;
        Ok((serde_json::from_str(&manifest)?, state))
    }

    pub fn plan_status(&self, id: &str) -> Result<Value> {
        let (manifest, state) = self.load_plan(id)?;
        let mut statement = self
            .db
            .prepare("SELECT step,state,details FROM plan_steps WHERE plan=? ORDER BY step")?;
        let steps=statement.query_map([id],|r| {
            let details=r.get::<_,Option<String>>(2)?.and_then(|s|serde_json::from_str::<Value>(&s).ok());
            Ok(json!({"step":r.get::<_,usize>(0)?,"state":r.get::<_,String>(1)?,"details":details}))
        })?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(json!({"state":state,"manifest":manifest,"steps":steps}))
    }

    pub fn execute_plan(&self, id: &str, confirmed: bool, cancel: &AtomicBool) -> Result<Value> {
        let (plan, state) = self.load_plan(id)?;
        ensure!(state == "planned", "plan has already started");
        ensure!(
            plan.conflicts.is_empty(),
            "resolve manifest conflicts before executing"
        );
        ensure!(
            !plan.action.physical() || confirmed,
            "explicit confirmation is required for physical changes"
        );
        if !plan.action.physical() {
            return self.metadata_plan(&plan);
        }
        self.admin()?;
        let roots = self.roots()?;
        ensure!(
            self.db.execute(
                "UPDATE plans SET state='running' WHERE id=? AND state='planned'",
                [id]
            )? == 1,
            "plan is already running"
        );
        let mut complete = true;
        for (index, entry) in plan.entries.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                complete = false;
                break;
            }
            let result = self.physical_step(&plan, index, entry, &roots, cancel);
            if let Err(error) = result {
                complete = false;
                self.db.execute("INSERT INTO plan_steps VALUES(?,?,'failed',?)
                    ON CONFLICT DO UPDATE SET state=CASE WHEN state='intent' THEN 'intent' ELSE 'failed' END,
                    details=CASE WHEN state='intent' THEN json_set(details,'$.error',?4) ELSE excluded.details END",
                    params![id,index,json!({"error":format!("{error:#}")}).to_string(),format!("{error:#}")])?;
                break;
            }
        }
        self.db.execute(
            "UPDATE plans SET state=? WHERE id=?",
            params![if complete { "done" } else { "partial" }, id],
        )?;
        self.plan_status(id)
    }

    fn metadata_plan(&self, plan: &Manifest) -> Result<Value> {
        let tx = Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)?;
        ensure!(
            tx.execute(
                "UPDATE plans SET state='done' WHERE id=? AND state='planned'",
                [&plan.id]
            )? == 1,
            "plan is already running"
        );
        let mut collection = None;
        if plan.action == Action::Marriage {
            let id = uuid();
            tx.execute(
                "INSERT INTO objects(id,kind,name) VALUES(?,'marriage',?)",
                params![id, plan.argument],
            )?;
            tx.execute("INSERT INTO owners VALUES(?,?)", params![id, self.actor])?;
            collection = Some(id);
        }
        for (index, entry) in plan.entries.iter().enumerate() {
            let item = self.get(&entry.object)?;
            if plan.action == Action::Tag {
                self.editable(&item)?;
                crate::organization::assign(
                    &tx,
                    &item.id,
                    plan.argument.as_deref().unwrap(),
                    "user",
                    1.0,
                    None,
                )?;
            } else if let Some(collection) = &collection {
                tx.execute(
                    "INSERT INTO edges(container,target,kind) VALUES(?,?,'reference')",
                    params![collection, item.id],
                )?;
            }
            tx.execute(
                "INSERT INTO plan_steps VALUES(?,?,'done',?)",
                params![plan.id, index, json!({"collection":collection}).to_string()],
            )?;
        }
        event(
            &tx,
            None,
            "cleave-metadata",
            Some(&self.actor),
            None,
            Some(&json!({"plan":plan.id,"collection":collection})),
            "ok",
        )?;
        tx.commit()?;
        self.plan_status(&plan.id)
    }

    fn physical_step(
        &self,
        plan: &Manifest,
        index: usize,
        entry: &Entry,
        roots: &[String],
        cancel: &AtomicBool,
    ) -> Result<()> {
        let source = Path::new(&entry.source);
        let destination = Path::new(entry.destination.as_deref().context("no destination")?);
        if plan.action == Action::Quarantine {
            self.quarantine_parents(destination, roots)?;
        }
        parent_safe(source, roots)?;
        parent_safe(destination, roots)?;
        available(destination)?;
        original_matches(entry, source)?;
        let mut file = if plan.action == Action::Copy {
            fsmeta::open_snapshot(source)?
        } else {
            open_move(source)?
        };
        ensure!(
            !fsmeta::reparse(&file.metadata()?) && file.metadata()?.is_file(),
            "source is no longer an ordinary file"
        );
        original_matches(entry, source)?;
        let expected = fsmeta::hash(&mut file)?;
        ensure!(
            !cancel.load(Ordering::Relaxed),
            "operation cancelled before changing the file"
        );
        self.db.execute(
            "INSERT INTO plan_steps VALUES(?,?,'intent',?)",
            params![plan.id, index, json!({"digest":expected}).to_string()],
        )?;
        if plan.action == Action::Copy {
            copy_verified(&mut file, destination, &expected)?;
        } else {
            rename_open(&file, source, destination)?;
        }
        drop(file);
        ensure!(
            digest(destination)? == expected,
            "destination bytes failed integrity verification"
        );
        if plan.action == Action::Move {
            self.relink(entry, destination)?;
        }
        if plan.action != Action::Quarantine {
            self.refresh_path(destination, &AtomicBool::new(false))?;
        }
        if plan.action != Action::Copy {
            self.refresh_path(source, &AtomicBool::new(false))?;
        }
        self.record_physical(plan, index, entry, &expected, "done", source, destination)
    }

    fn record_physical(
        &self,
        plan: &Manifest,
        index: usize,
        entry: &Entry,
        expected: &str,
        state: &str,
        source: &Path,
        destination: &Path,
    ) -> Result<()> {
        let tx = self.db.unchecked_transaction()?;
        tx.execute(
            "UPDATE plan_steps SET state=?,details=? WHERE plan=? AND step=?",
            params![
                state,
                json!({"digest":expected}).to_string(),
                plan.id,
                index
            ],
        )?;
        event(
            &tx,
            Some(&entry.object),
            &format!("cleave-{state}"),
            Some(&self.actor),
            Some(&json!({"path":source})),
            Some(&json!({"path":destination,"plan":plan.id,"digest":expected})),
            "verified",
        )?;
        tx.commit()?;
        Ok(())
    }

    fn relink(&self, entry: &Entry, path: &Path) -> Result<()> {
        let (size, _, created, id) = fsmeta::hard_stamp(path)?;
        ensure!(
            size == entry.bytes as u64
                && entry
                    .native
                    .as_ref()
                    .is_none_or(|old| id.as_ref() == Some(old)),
            "verified destination no longer matches the moved file"
        );
        // NTFS can tunnel creation timestamps when a name is reused. A verified move provides
        // stronger evidence than the birth-time check used for an external rename.
        ensure!(
            self.db.execute(
                "UPDATE objects SET path=?,created=?,missing=0 WHERE id=?",
                params![path_text(path)?, created, entry.object]
            )? == 1,
            "could not reconnect the file to its Ledger identity"
        );
        Ok(())
    }

    fn quarantine_parents(&self, destination: &Path, roots: &[String]) -> Result<()> {
        let mut pending = Vec::new();
        for path in destination
            .parent()
            .context("missing quarantine folder")?
            .ancestors()
        {
            if fs::symlink_metadata(path).is_ok() {
                break;
            }
            pending.push(path);
        }
        for path in pending.into_iter().rev() {
            parent_safe(path, roots)?;
            match fs::create_dir(path) {
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                result => result?,
            }
        }
        Ok(())
    }

    pub fn undo_plan(&self, id: &str, confirmed: bool) -> Result<Value> {
        self.admin()?;
        ensure!(
            confirmed,
            "explicit confirmation is required to undo physical changes"
        );
        let (plan, state) = self.load_plan(id)?;
        ensure!(
            plan.action.physical() && matches!(state.as_str(), "done" | "partial" | "undo-partial"),
            "choose a completed physical plan"
        );
        ensure!(
            self.db.execute(
                "UPDATE plans SET state='undo-running' WHERE id=? AND state=?",
                params![id, state]
            )? == 1,
            "plan changed before undo"
        );
        let result = self.undo_steps(&plan);
        self.db.execute(
            "UPDATE plans SET state=? WHERE id=?",
            params![
                if result.is_ok() {
                    "undone"
                } else {
                    "undo-partial"
                },
                id
            ],
        )?;
        result?;
        self.plan_status(id)
    }

    fn undo_steps(&self, plan: &Manifest) -> Result<()> {
        let roots = self.roots()?;
        for (index, entry) in plan.entries.iter().enumerate().rev() {
            let step = self
                .db
                .query_row(
                    "SELECT state,details FROM plan_steps WHERE plan=? AND step=?",
                    params![plan.id, index],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
                )
                .optional()?;
            let Some((state, details)) = step else {
                continue;
            };
            if matches!(state.as_str(), "failed" | "undone") {
                continue;
            }
            ensure!(
                state == "done",
                "an interrupted step needs manual recovery before undo"
            );
            let details: Value = serde_json::from_str(&details.context("missing journal digest")?)?;
            let expected = details["digest"]
                .as_str()
                .context("missing journal digest")?;
            let destination = Path::new(
                entry
                    .destination
                    .as_deref()
                    .context("missing journal destination")?,
            );
            let source = Path::new(&entry.source);
            let recovery = if plan.action == Action::Copy {
                let root = roots
                    .iter()
                    .filter(|r| destination.starts_with(r))
                    .max_by_key(|r| r.len())
                    .context("destination outside roots")?;
                let recovery = Path::new(root)
                    .join(".mosh-quarantine")
                    .join(&plan.id)
                    .join(format!("undo-{index}"))
                    .join(destination.file_name().unwrap());
                self.quarantine_parents(&recovery, &roots)?;
                recovery
            } else {
                source.to_owned()
            };
            parent_safe(destination, &roots)?;
            parent_safe(&recovery, &roots)?;
            available(&recovery)?;
            let mut file = open_move(destination)?;
            ensure!(
                !fsmeta::reparse(&file.metadata()?)
                    && file.metadata()?.is_file()
                    && fsmeta::hash(&mut file)? == expected,
                "destination changed after Cleave: {}",
                destination.display()
            );
            self.db.execute(
                "UPDATE plan_steps SET state='undo-intent',details=? WHERE plan=? AND step=?",
                params![
                    json!({"digest":expected,"recovery":recovery}).to_string(),
                    plan.id,
                    index
                ],
            )?;
            rename_open(&file, destination, &recovery)?;
            drop(file);
            ensure!(
                digest(&recovery)? == expected,
                "restored bytes failed integrity verification"
            );
            if plan.action != Action::Copy {
                self.relink(entry, &recovery)?;
                self.refresh_path(&recovery, &AtomicBool::new(false))?;
            }
            self.refresh_path(destination, &AtomicBool::new(false))?;
            self.record_physical(
                plan,
                index,
                entry,
                expected,
                "undone",
                destination,
                &recovery,
            )?;
        }
        Ok(())
    }

    pub fn create_folder(&self, parent: &str, name: &str) -> Result<crate::Object> {
        self.admin()?;
        let item = self.get(parent)?;
        let path = Path::new(
            item.path
                .as_deref()
                .context("choose a physical directory")?,
        );
        let name = leaf(name)?;
        let target = path.join(name);
        parent_safe(&target, &self.roots()?)?;
        let _guards = guard_destination(&target)?;
        fs::create_dir(&target)?;
        self.refresh_path(&target, &AtomicBool::new(false))?;
        let item = self.get(path_text(&target)?)?;
        event(
            &self.db,
            Some(&item.id),
            "create-directory",
            Some(&self.actor),
            None,
            Some(&json!({"path":target})),
            "ok",
        )?;
        Ok(item)
    }
}

fn leaf(name: &str) -> Result<&str> {
    let mut parts = Path::new(name).components();
    let lowercase = name.to_ascii_lowercase();
    ensure!(
        !name.is_empty()
            && !name.ends_with(['.', ' '])
            && !name.contains(['\\', '/', ':', '*', '?', '"', '<', '>', '|'])
            && !name.chars().any(char::is_control)
            && !matches!(lowercase.as_str(), ".git" | ".mosh-quarantine")
            && !lowercase.starts_with(".mosh-copy-")
            && matches!(parts.next(), Some(Component::Normal(_)))
            && parts.next().is_none(),
        "enter one valid name"
    );
    Ok(name)
}
