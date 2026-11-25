use anyhow::Result;
use mosh_core::{Action, Mosh, SearchOptions};
use std::{
    fs,
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

struct Fixture {
    temporary: tempfile::TempDir,
    root: PathBuf,
    db: PathBuf,
    mosh: Mosh,
}

impl Fixture {
    fn new() -> Result<Self> {
        let temporary = tempfile::tempdir()?;
        let root = temporary.path().join("fixtures");
        fs::create_dir(&root)?;
        let db = temporary.path().join("ledger.sqlite3");
        let mosh = Mosh::open(&db)?;
        Ok(Self {
            temporary,
            root,
            db,
            mosh,
        })
    }
    fn scan(&self) -> Result<()> {
        let report = self.mosh.scan(&self.root, &AtomicBool::new(false))?;
        assert_eq!(report.errors, 0, "{:?}", report.warnings);
        Ok(())
    }
    fn search(&self, query: &str) -> Result<Vec<String>> {
        Ok(self
            .mosh
            .search(query, &SearchOptions::default())?
            .into_iter()
            .map(|item| item.name)
            .collect())
    }
}

#[test]
fn search_inheritance_paint_and_identity_survive_rename() -> Result<()> {
    let fixture = Fixture::new()?;
    let folder = fixture.root.join("Projet été");
    fs::create_dir(&folder)?;
    let file = folder.join("résumé.rs");
    fs::write(&file, "Submarine routes and pathfinding in Rust")?;
    fixture.scan()?;
    let coll = fixture.mosh.get(folder.to_str().unwrap())?;
    let id = fixture.mosh.get(file.to_str().unwrap())?.id;
    fixture.mosh.tag(&coll.id, "Game Archive", true, 1.0)?;
    fixture.mosh.tag(&id, "combat", true, 0.9)?;
    fixture
        .mosh
        .tag_dictionary("alias", "combat", Some("battle"))?;
    fixture
        .mosh
        .set_paint(&id, "minimal", "Primary mechanics research", true)?;
    assert_eq!(
        fixture.search(r#"tag:"game archive" AND direct-tag:battle AND content:submarine"#)?,
        ["résumé.rs"]
    );
    assert_eq!(fixture.search("paint:mechanics")?, ["résumé.rs"]);
    assert_eq!(fixture.search("ext:rs NOT tag:obsolete")?, ["résumé.rs"]);
    assert!(
        fixture
            .search("content:submarine AND (ext:txt OR ext:md)")?
            .is_empty()
    );
    assert!(fixture.search("tag:test AND (").is_err());
    assert!(fixture.search("size:>NaN").is_err());
    assert!(
        fixture
            .mosh
            .tags(&id)?
            .iter()
            .any(|tag| tag["inherited"] == true && tag["name"] == "game archive")
    );
    let renamed = folder.join("archive.rs");
    fs::rename(&file, &renamed)?;
    fixture.scan()?;
    assert_eq!(fixture.mosh.get(renamed.to_str().unwrap())?.id, id);
    fs::write(&renamed, "Replacement content and modification")?;
    fixture.scan()?;
    assert_eq!(fixture.search("content:replacement")?, ["archive.rs"]);
    assert!(fixture.search("content:submarine")?.is_empty());
    assert_eq!(fixture.mosh.paint(&id)?.unwrap()["stale"], true);
    assert_eq!(fixture.mosh.doctor()?["ok"], true);
    drop(fixture.mosh);
    assert_eq!(Mosh::open(&fixture.db)?.get(&id)?.name, "archive.rs");
    Ok(())
}

#[test]
fn virtual_members_and_secret_sources_use_authority() -> Result<()> {
    let fixture = Fixture::new()?;
    let vault = fixture.root.join("vault");
    fs::create_dir(&vault)?;
    fs::write(vault.join("hidden.txt"), "unlisted flame")?;
    fs::write(fixture.root.join("visible.txt"), "unlisted flame")?;
    fixture.scan()?;
    let vault = fixture.mosh.get(vault.to_str().unwrap())?;
    let target = fixture.mosh.get(
        PathBuf::from(vault.path.as_ref().unwrap())
            .join("hidden.txt")
            .to_str()
            .unwrap(),
    )?;
    fixture.mosh.person("manager", None)?;
    fixture.mosh.person("owner", Some("manager"))?;
    fixture.mosh.person("guest", None)?;
    fixture.mosh.person("team-lead", None)?;
    fixture.mosh.person("visitor", Some("team-lead"))?;
    assert!(fixture.mosh.person("team-lead", Some("visitor")).is_err());
    fixture.mosh.access(&vault.id, "owner", "owner")?;
    fixture.mosh.access(&vault.id, "visitor", "read")?;
    fixture.mosh.secret(&vault.id, true)?;
    let first = fixture.mosh.collection("Reading list", true)?;
    let second = fixture.mosh.collection("Archive lens", true)?;
    fixture.mosh.connect(&first, &second, "reference", None)?;
    assert!(
        fixture
            .mosh
            .connect(&second, &first, "reference", None)
            .is_err()
    );
    fixture
        .mosh
        .connect(&first, &target.id, "reference", Some("section:notes"))?;
    fixture.mosh.collection_query(&first, "content:flame")?;
    let actor = Mosh::as_actor(&fixture.db, "guest")?;
    assert!(actor.inspect(&target.id).is_err());
    assert!(actor.history(None, 100).is_err());
    assert_eq!(
        actor
            .search("content:flame", &SearchOptions::default())?
            .len(),
        1
    );
    assert_eq!(
        actor
            .search(
                "",
                &SearchOptions {
                    parent: Some(first.clone()),
                    ..Default::default()
                }
            )?
            .len(),
        2
    );
    assert!(
        actor
            .graph(None)?
            .iter()
            .all(|edge| edge["target"]["id"] != target.id)
    );
    assert!(
        Mosh::as_actor(&fixture.db, "manager")?
            .get(&target.id)
            .is_ok()
    );
    assert!(
        Mosh::as_actor(&fixture.db, "visitor")?
            .get(&target.id)
            .is_ok()
    );
    assert!(
        Mosh::as_actor(&fixture.db, "team-lead")?
            .get(&target.id)
            .is_err()
    );
    Ok(())
}

#[test]
fn physical_plans_require_confirmation_and_preserve_original_identity() -> Result<()> {
    let fixture = Fixture::new()?;
    let source = fixture.root.join("note.txt");
    let copy_folder = fixture.root.join("exports");
    fs::create_dir(&copy_folder)?;
    fs::write(&source, "keep this safe")?;
    fixture.scan()?;
    let original = fixture.mosh.get(source.to_str().unwrap())?;
    fixture.mosh.tag(&original.id, "durable", true, 1.0)?;
    let list = vec![original.id.clone()];
    let cancel = AtomicBool::new(false);
    let copy = fixture
        .mosh
        .plan("", Action::Copy, copy_folder.to_str(), &list)?;
    assert!(copy.conflicts.is_empty(), "{:?}", copy.conflicts);
    let copy_result = fixture.mosh.execute_plan(&copy.id, true, &cancel)?;
    assert_eq!(copy_result["state"], "done", "{copy_result}");
    assert_eq!(fs::read(copy_folder.join("note.txt"))?, b"keep this safe");
    let conflict = fixture
        .mosh
        .plan("", Action::Move, copy_folder.to_str(), &list)?;
    assert!(!conflict.conflicts.is_empty());
    let undone = fixture.mosh.undo_plan(&copy.id, true)?;
    assert_eq!(undone["state"], "undone");
    assert!(!copy_folder.join("note.txt").exists());
    let quarantine = fixture.mosh.plan("", Action::Quarantine, None, &list)?;
    assert!(
        quarantine.conflicts.is_empty(),
        "{:?}",
        quarantine.conflicts
    );
    assert!(
        fixture
            .mosh
            .execute_plan(&quarantine.id, false, &cancel)
            .is_err()
    );
    assert!(source.exists());
    let result = fixture.mosh.execute_plan(&quarantine.id, true, &cancel)?;
    assert_eq!(result["state"], "done", "{result}");
    let hidden = PathBuf::from(quarantine.entries[0].destination.as_ref().unwrap());
    assert_eq!(fs::read(&hidden)?, b"keep this safe");
    assert!(!source.exists());
    assert!(fixture.search("direct-tag:durable")?.is_empty());
    let undone = fixture.mosh.undo_plan(&quarantine.id, true)?;
    assert_eq!(undone["state"], "undone");
    assert_eq!(fixture.mosh.get(source.to_str().unwrap())?.id, original.id);
    assert_eq!(fixture.search("direct-tag:durable")?, ["note.txt"]);
    let move_plan = fixture
        .mosh
        .plan("", Action::Move, copy_folder.to_str(), &list)?;
    let result = fixture.mosh.execute_plan(&move_plan.id, true, &cancel)?;
    assert_eq!(result["state"], "done", "{result}");
    let moved = fixture
        .mosh
        .get(copy_folder.join("note.txt").to_str().unwrap())?;
    assert_eq!(moved.id, original.id);
    fixture.mosh.undo_plan(&move_plan.id, true)?;
    assert_eq!(fs::read(&source)?, b"keep this safe");
    assert_eq!(fixture.mosh.doctor()?["ok"], true);
    Ok(())
}

#[test]
fn duplicates_account_for_hardlinks_and_backup_round_trips() -> Result<()> {
    let fixture = Fixture::new()?;
    let file = fixture.root.join("one.txt");
    fs::write(&file, vec![b'a'; 5040])?;
    fs::hard_link(&file, fixture.root.join("linked.txt"))?;
    fs::copy(&file, fixture.root.join("other.txt"))?;
    fs::write(fixture.root.join("different.txt"), vec![b'b'; 5040])?;
    fixture.scan()?;
    let report = fixture
        .mosh
        .duplicates(Some(&fixture.root), &AtomicBool::new(false))?;
    assert_eq!(report["errors"], 0, "{report}");
    let groups = report["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 1, "{report}");
    assert_eq!(groups[0]["members"].as_array().unwrap().len(), 3);
    assert_eq!(groups[0]["physical_files"], 2);
    assert_eq!(fixture.search("duplicate:true")?.len(), 3);
    let space = fixture.mosh.space(&fixture.root)?;
    assert_eq!(space["files"], 4);
    assert_eq!(space["logical"], 20160);
    let backup = fixture.temporary.path().join("backup.sqlite3");
    fixture.mosh.backup(&backup)?;
    assert!(fixture.mosh.backup(&backup).is_err());
    let destination = fixture.temporary.path().join("restored.sqlite3");
    Mosh::restore(&backup, &destination)?;
    let restored = Mosh::open(&destination)?;
    assert_eq!(restored.doctor()?["ok"], true);
    restored.rebuild()?;
    assert_eq!(
        restored
            .search("duplicate:true", &SearchOptions::default())?
            .len(),
        3
    );
    Ok(())
}

#[test]
fn cancellation_leaves_no_partial_scan_and_watch_indexes_an_external_edit() -> Result<()> {
    let fixture = Fixture::new()?;
    let file = fixture.root.join("watch.txt");
    fs::write(&file, "before")?;
    assert!(
        fixture
            .mosh
            .scan(&fixture.root, &AtomicBool::new(true))
            .is_err()
    );
    assert!(fixture.mosh.roots()?.is_empty());
    let cancel = Arc::new(AtomicBool::new(false));
    let (send, recv) = mpsc::channel();
    let watcher = {
        let db = fixture.db.clone();
        let root = fixture.root.clone();
        let cancel = cancel.clone();
        std::thread::spawn(move || -> Result<()> {
            Mosh::open(&db)?.watch(&root, &cancel, |report| {
                let _ = send.send(report);
            })
        })
    };
    let wait = (|| -> Result<()> {
        recv.recv_timeout(Duration::from_secs(10))??;
        fs::write(&file, "watcher success sentinel")?;
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(10) {
            let _ = recv.recv_timeout(Duration::from_millis(500));
            if fixture.search("content:sentinel")? == ["watch.txt"] {
                return Ok(());
            }
        }
        anyhow::bail!("the ledger never picked up the external write");
    })();
    cancel.store(true, Ordering::Relaxed);
    watcher.join().expect("watcher panicked")?;
    wait
}

#[test]
fn reads_git_history_without_changing_the_worktree() -> Result<()> {
    let fixture = Fixture::new()?;
    let git = |args: &[&str]| -> Result<()> {
        let status = Command::new("git")
            .current_dir(&fixture.root)
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        anyhow::ensure!(status.success(), "git {args:?} failed");
        Ok(())
    };
    git(&["init", "-b", "test"])?;
    git(&["config", "user.name", "Fixture Author"])?;
    git(&["config", "user.email", "fixture@example.invalid"])?;
    let file = fixture.root.join("tracked.md");
    fs::write(&file, "First revision")?;
    git(&["add", "--", "tracked.md"])?;
    git(&["commit", "-m", "Fixture commit"])?;
    fs::write(&file, "First revision\nsecond revision")?;
    fixture.scan()?;
    let selected = fixture.mosh.get(file.to_str().unwrap())?;
    assert_eq!(selected.git_status.as_deref(), Some("modified"));
    assert_eq!(
        fixture.search(r#"branch:test AND git-author:"Fixture Author""#)?,
        ["tracked.md"]
    );
    let commits = fixture.mosh.git_history(&selected.id)?;
    assert_eq!(commits[0]["author"], "Fixture Author");
    assert!(
        fixture
            .mosh
            .git_diff(&selected.id, None)?
            .contains("+second revision")
    );
    assert_eq!(
        fs::read_to_string(&file)?,
        "First revision\nsecond revision"
    );
    Ok(())
}

#[cfg(windows)]
#[test]
fn records_authentic_windows_file_metadata() -> Result<()> {
    let fixture = Fixture::new()?;
    let file = fixture.root.join("win.txt");
    fs::write(&file, "windows")?;
    fixture.scan()?;
    let object = fixture.mosh.get(file.to_str().unwrap())?;
    assert!(object.native_id.is_some());
    assert!(object.volume.is_some());
    assert!(object.allocated.is_some());
    assert!(object.attributes.is_some());
    assert_eq!(object.hardlinks, Some(1));
    assert!(fixture.mosh.inspect(&object.id)?["windows_security"]["sddl"].is_string());
    Ok(())
}
