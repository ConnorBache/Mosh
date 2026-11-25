use anyhow::{Context, Result, ensure};
use mosh_core::{Mosh, ScanReport};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Clone, Serialize, Deserialize)]
pub struct Workspace {
    pub id: String,
    pub root: PathBuf,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Catalog {
    pub active: Option<String>,
    pub entries: Vec<Workspace>,
}

pub struct Workspaces {
    default_db: PathBuf,
    directory: PathBuf,
    catalog: Mutex<Catalog>,
}

impl Workspaces {
    pub fn open(default_db: PathBuf) -> Result<Self> {
        let directory = default_db.with_extension("moshes");
        let mut catalog: Catalog = match fs::read(directory.join("catalog.json")) {
            Ok(bytes) => serde_json::from_slice(&bytes).context("reading the Mosh catalog")?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Catalog::default(),
            Err(error) => return Err(error.into()),
        };
        ensure!(
            catalog.entries.iter().all(|entry| valid_id(&entry.id)),
            "invalid Mosh catalog ID"
        );
        if catalog
            .active
            .as_ref()
            .is_some_and(|id| !directory.join(format!("{id}.sqlite3")).is_file())
        {
            catalog.active = None;
        }
        Ok(Self {
            default_db,
            directory,
            catalog: Mutex::new(catalog),
        })
    }

    pub fn catalog(&self) -> Catalog {
        self.catalog.lock().unwrap().clone()
    }

    pub fn active(&self) -> (Option<String>, PathBuf) {
        let id = self.catalog.lock().unwrap().active.clone();
        let db = id
            .as_ref()
            .map_or_else(|| self.default_db.clone(), |id| self.ledger_path(id));
        (id, db)
    }

    pub fn select(&self, id: Option<&str>) -> Result<Catalog> {
        let mut current = self.catalog.lock().unwrap();
        if let Some(id) = id {
            ensure!(
                current.entries.iter().any(|entry| entry.id == id),
                "unknown Mosh"
            );
            ensure!(
                self.ledger_path(id).is_file(),
                "the Mosh Ledger is unavailable"
            );
        }
        let mut next = current.clone();
        next.active = id.map(str::to_owned);
        self.save(&next)?;
        *current = next;
        Ok(current.clone())
    }

    pub fn build(&self, selected: &Path, cancel: &AtomicBool) -> Result<(Catalog, ScanReport)> {
        ensure!(!selected.as_os_str().is_empty(), "select a directory");
        let root = dunce::canonicalize(selected).context("opening the selected directory")?;
        ensure!(root.is_dir(), "select a directory");
        fs::read_dir(&root).context("reading the selected directory")?;
        fs::create_dir_all(&self.directory)?;
        let directory = dunce::canonicalize(&self.directory)?;
        ensure!(
            !root.starts_with(&directory),
            "select a directory outside the Mosh data folder"
        );
        let prior = self
            .catalog()
            .entries
            .into_iter()
            .find(|entry| entry.root == root);
        if let Some(prior) = prior {
            let ledger = self.ledger_path(&prior.id);
            ensure!(ledger.is_file(), "the Mosh Ledger is unavailable");
            let report = Mosh::open(&ledger)?.scan(&root, cancel)?;
            return Ok((self.select(Some(&prior.id))?, report));
        }
        let staging = tempfile::Builder::new()
            .prefix(".build-")
            .tempdir_in(&directory)?;
        // The database is closed and checkpointed before it becomes selectable.
        let temporary = staging.path().join("ledger.sqlite3");
        let mosh = Mosh::open(&temporary)?;
        mosh.set_scan_storage(&directory, &self.default_db)?;
        let report = mosh.scan(&root, cancel)?;
        ensure!(
            mosh.roots()?.iter().any(|path| Path::new(path) == root),
            "the selected directory is unavailable"
        );
        ensure!(!cancel.load(Ordering::Relaxed), "build cancelled");
        let id = mosh.mosh_id()?;
        ensure!(valid_id(&id), "invalid generated Mosh ID");
        let path = self.ledger_path(&id);
        mosh.backup(&path)?;

        let mut current = self.catalog.lock().unwrap();
        let mut next = current.clone();
        next.entries.push(Workspace {
            id: id.clone(),
            root,
        });
        next.active = Some(id);
        if let Err(error) = self.save(&next) {
            let _ = fs::remove_file(&path);
            return Err(error);
        }
        *current = next;
        Ok((current.clone(), report))
    }

    pub fn configure_monitor(&self, mosh: &Mosh) -> Result<()> {
        if self.directory.is_dir() {
            mosh.set_scan_storage(&self.directory, &self.default_db)?;
        }
        Ok(())
    }

    pub fn note_root(&self, id: &str, path: &Path) -> Result<()> {
        let mut catalog = self.catalog.lock().unwrap();
        let mut next = catalog.clone();
        if let Some(entry) = next
            .entries
            .iter_mut()
            .find(|entry| entry.id == id && entry.root != path)
        {
            entry.root = path.to_owned();
            self.save(&next)?;
            *catalog = next;
        }
        Ok(())
    }

    fn ledger_path(&self, id: &str) -> PathBuf {
        self.directory.join(format!("{id}.sqlite3"))
    }

    fn save(&self, catalog: &Catalog) -> Result<()> {
        fs::create_dir_all(&self.directory)?;
        let mut temporary = tempfile::NamedTempFile::new_in(&self.directory)?;
        temporary.write_all(&serde_json::to_vec(catalog)?)?;
        temporary.as_file().sync_all()?;
        temporary.persist(self.directory.join("catalog.json"))?;
        Ok(())
    }
}

fn valid_id(id: &str) -> bool {
    id.len() == 36
        && id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;
    use mosh_core::SearchOptions;

    #[test]
    fn rebuilding_a_selection_reuses_its_private_ledger_and_catalog() -> Result<()> {
        let fixture = tempfile::tempdir()?;
        let default_db = fixture.path().join("default.sqlite3");
        let source = fixture.path().join("Source chosen in wizard");
        fs::create_dir(&source)?;
        let file = source.join("one.txt");
        fs::write(&file, "original contents")?;
        let workspaces = Workspaces::open(default_db.clone())?;
        let cancelled = AtomicBool::new(true);
        assert!(workspaces.build(&source, &cancelled).is_err());
        assert!(workspaces.catalog().entries.is_empty());
        let cancel = AtomicBool::new(false);
        let (catalog, _) = workspaces.build(&source, &cancel)?;
        let id = catalog.active.unwrap();
        let (_, path) = workspaces.active();
        let ledger = Mosh::open(&path)?;
        let object = ledger.get(file.to_str().unwrap())?;
        ledger.tag(&object.id, "wizard-label", true, 1.0)?;
        assert!(
            Mosh::open(&default_db)?
                .search("name:one.txt", &SearchOptions::default())?
                .is_empty()
        );
        assert_eq!(
            workspaces.build(&source, &cancel)?.0.active.as_deref(),
            Some(id.as_str())
        );
        assert_eq!(workspaces.catalog().entries.len(), 1);
        let reopened = Workspaces::open(default_db)?;
        assert_eq!(reopened.active().0, Some(id));
        assert_eq!(reopened.active().1, path);
        assert_eq!(
            ledger.search("direct-tag:wizard-label", &SearchOptions::default())?[0].id,
            object.id
        );
        assert_eq!(fs::read_to_string(file)?, "original contents");
        assert_eq!(fs::read_dir(source)?.count(), 1);
        Ok(())
    }
}
