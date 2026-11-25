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
    assert_eq!(fixture.search("kind:file path:projet")?, ["résumé.rs"]);
    assert_eq!(fixture.search("kind:file path:résumé")?, ["résumé.rs"]);
    let across = format!("kind:file path:été{}résumé.rs", std::path::MAIN_SEPARATOR);
    assert_eq!(fixture.search(&across)?, ["résumé.rs"]);
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
fn v1_migration_preserves_provenance_aliases_and_search_across_vacuum() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let database = temporary.path().join("old.sqlite3");
    let file = temporary.path().join("original.txt");
    fs::write(&file, "a résumé on waterways")?;
    fs::create_dir(temporary.path().join("alias"))?;
    let stored = temporary.path().join("alias/../original.txt");
    {
        let db = rusqlite::Connection::open(&database)?;
        db.execute_batch(include_str!("../src/schema.sql"))?;
        db.pragma_update(None, "user_version", 1)?;
        db.execute(
            "INSERT INTO objects(id,kind,path,name,extension,content) VALUES
            ('old-id','file',?,'original.txt','txt','a stale copy of the original')",
            [stored.to_str().unwrap()],
        )?;
        db.execute(
            "INSERT INTO objects(id,kind,name) VALUES('untagged','file','untagged')",
            [],
        )?;
        db.execute_batch("INSERT INTO tag_dictionary VALUES(1,'topic'),(2,'candidate'),(3,'subject');
            INSERT INTO tag_aliases VALUES('topic-alias',1);
            INSERT INTO tag_assignments VALUES
            ('old-id',1,'user',0.9,NULL),('old-id',1,'automatic',0.4,'rule detail'),
            ('old-id',2,'suggested',0.7,'assistant detail'),('old-id',3,'automatic',0.1,'weaker rule');
            INSERT INTO paint VALUES('old-id','standard','A useful note',1,1,'user');")?;
    }
    let mosh = Mosh::open(&database)?;
    assert!(
        mosh.tags("untagged")?
            .iter()
            .any(|tag| tag["name"] == "file")
    );
    let check = |query| {
        mosh.search(query, &SearchOptions::default())
            .map(|found| found.len())
    };
    assert_eq!(check("direct-tag:topic-alias AND direct-tag:subject")?, 1);
    assert_eq!(check("suggested-tag:candidate")?, 1);
    assert_eq!(check("direct-tag:candidate")?, 0);
    assert_eq!(check("paint:useful")?, 1);
    assert_eq!(check("content:resume")?, 1);
    assert_eq!(check("content:stale")?, 0);
    assert_eq!(
        mosh.get("old-id")?.path.as_deref(),
        dunce::canonicalize(&file)?.to_str()
    );
    mosh.tag("old-id", "topic", false, 1.0)?;
    assert_eq!(check("direct-tag:topic-alias")?, 1);
    mosh.tag_dictionary("merge", "topic", Some("subject"))?;
    let tags = mosh.tags("old-id")?;
    let migrated = tags.iter().find(|tag| tag["name"] == "subject").unwrap();
    assert_eq!(migrated["confidence"], 0.4);
    assert_eq!(migrated["evidence"], "rule detail");
    assert_eq!(migrated["origin"], "automatic");
    assert_eq!(
        check("direct-tag:topic-alias AND direct-tag:topic AND NOT direct-tag:candidate")?,
        1
    );
    assert!(
        mosh.tag_catalog(20)?
            .iter()
            .all(|entry| entry["name"] != "candidate")
    );
    mosh.tag_dictionary("delete", "candidate", None)?;
    assert_eq!(check("suggested-tag:candidate")?, 0);
    mosh.compact()?;
    mosh.rebuild()?;
    assert_eq!(
        check("name:original.txt AND paint:useful AND direct-tag:subject")?,
        1
    );
    assert_eq!(mosh.doctor()?["ok"], true);
    assert_eq!(check("content:resume")?, 1);
    assert_eq!(check("content:stale")?, 0);
    let snapshot = temporary.path().join("v1-upgraded-backup.sqlite3");
    let restored = temporary.path().join("v1-restored.sqlite3");
    mosh.backup(&snapshot)?;
    let copy = Mosh::open(&Mosh::restore(&snapshot, &restored)?)?;
    assert_eq!(copy.get("old-id")?.id, mosh.get("old-id")?.id);
    assert_eq!(
        copy.search("content:resume", &SearchOptions::default())?
            .len(),
        1
    );
    assert!(
        copy.search("content:stale", &SearchOptions::default())?
            .is_empty()
    );
    assert_eq!(
        rusqlite::Connection::open(&database)?.query_row(
            "SELECT count(*) FROM pragma_table_info('objects') WHERE name='content'",
            [],
            |r| r.get::<_, i64>(0)
        )?,
        0
    );
    Ok(())
}

#[test]
fn content_uses_current_source_without_a_persistent_text_index() -> Result<()> {
    let fixture = Fixture::new()?;
    let file = fixture.root.join("report.md");
    fs::write(&file, "Résumé on lunar navigation")?;
    fixture.scan()?;
    assert_eq!(fixture.search(r#"content:"resume on""#)?, ["report.md"]);
    assert_eq!(
        fixture.search(r#"content:"lunar navigation""#)?,
        ["report.md"]
    );
    let connection = rusqlite::Connection::open(&fixture.db)?;
    assert_eq!(connection.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE name IN ('ledger','directory_fts','text_terms','text_cached')",
        [], |row| row.get::<_, i64>(0)
    )?, 0);
    fs::write(&file, "Replacement from another revision")?;
    assert_eq!(fixture.search("content:replacement")?, ["report.md"]);
    assert!(fixture.search("content:lunar")?.is_empty());
    assert!(
        fixture
            .mosh
            .preview(file.to_str().unwrap())?
            .contains("another revision")
    );
    Ok(())
}

#[test]
fn cleave_tag_records_one_packed_membership_event() -> Result<()> {
    let fixture = Fixture::new()?;
    let one = fixture.root.join("one.md");
    let two = fixture.root.join("two.md");
    fs::write(&one, "first")?;
    fs::write(&two, "second")?;
    fixture.scan()?;
    fixture
        .mosh
        .tag(one.to_str().unwrap(), "only-one", true, 1.0)?;
    let plan = fixture
        .mosh
        .plan("ext:md", Action::Tag, Some("marked"), &[])?;
    let status = fixture
        .mosh
        .execute_plan(&plan.id, false, &AtomicBool::new(false))?;
    assert_eq!(status["steps"].as_array().unwrap().len(), 2);
    assert_eq!(
        fixture.search("direct-tag:marked AND direct-tag:only-one")?,
        ["one.md"]
    );
    assert_eq!(
        fixture
            .search("direct-tag:marked AND direct-tag:unknown")?
            .len(),
        0
    );
    for file in [one, two] {
        let history = fixture.mosh.history(file.to_str(), 20)?;
        assert_eq!(
            history
                .iter()
                .filter(|row| row["action"] == "cleave-tag")
                .count(),
            1
        );
    }
    let db = rusqlite::Connection::open(&fixture.db)?;
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM plan_steps WHERE plan=?",
            [plan.id],
            |r| r.get::<_, i64>(0)
        )?,
        0
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM tag_audit_members", [], |r| r
            .get::<_, i64>(0))?,
        1
    );
    Ok(())
}

#[test]
fn live_directory_search_uses_relative_phrases_and_permissions() -> Result<()> {
    let fixture = Fixture::new()?;
    let root = fixture.root.join("machineancestor").join("projectroot");
    let src = root.join("src");
    let parser = src.join("parser");
    let second = fixture.root.join("otherroot");
    let other_parser = second.join("src").join("parser");
    fs::create_dir_all(&parser)?;
    fs::create_dir_all(root.join("unused").join("emptydir"))?;
    fs::create_dir_all(&other_parser)?;
    let file = parser.join("léxer.bin");
    let other = other_parser.join("index.bin");
    fs::write(&file, [0u8])?;
    fs::write(&other, [0u8])?;
    let cancel = AtomicBool::new(false);
    for path in [&root, &second] {
        assert_eq!(fixture.mosh.scan(path, &cancel)?.errors, 0);
    }
    assert_eq!(
        fixture.search("kind:file text:parser")?,
        ["index.bin", "léxer.bin"]
    );
    assert_eq!(
        fixture.search(r#"kind:file text:"src parser""#)?,
        ["index.bin", "léxer.bin"]
    );
    assert_eq!(
        fixture.search(r#"kind:file text:"parser lexer""#)?,
        ["léxer.bin"]
    );
    assert!(fixture.search(r#"kind:file text:"src lexer""#)?.is_empty());
    assert!(fixture.search("kind:file text:machineancestor")?.is_empty());
    assert!(fixture.search("kind:file text:projectroot")?.is_empty());

    let raw = rusqlite::Connection::open(&fixture.db)?;
    let directory_count = || {
        raw.query_row("SELECT count(*) FROM path_folders", [], |row| {
            row.get::<_, i64>(0)
        })
    };
    let count = directory_count()?;
    fs::write(parser.join("old-léxer.bin"), [0u8])?;
    assert_eq!(fixture.mosh.scan(&root, &cancel)?.errors, 0);
    assert_eq!(directory_count()?, count);

    let restricted = fixture.mosh.get(parser.to_str().unwrap())?;
    fixture.mosh.person("guest", None)?;
    fixture.mosh.secret(&restricted.id, true)?;
    let guest = Mosh::as_actor(&fixture.db, "guest")?;
    let visible = guest.search("kind:file text:parser", &SearchOptions::default())?;
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].name, "index.bin");

    assert_eq!(fixture.mosh.scan(&src, &cancel)?.errors, 0);
    assert_eq!(fixture.search("kind:file text:src")?, ["index.bin"]);
    fixture.mosh.rebuild()?;
    fixture.mosh.compact()?;
    assert_eq!(fixture.mosh.doctor()?["ok"], true);
    assert_eq!(
        fixture.search(r#"kind:file text:"parser lexer""#)?,
        ["léxer.bin"]
    );
    assert_eq!(
        fixture.search(r#"kind:folder text:"unused emptydir""#)?,
        ["emptydir"]
    );
    let id = fixture.mosh.get(file.to_str().unwrap())?.id;
    let renamed = src.join("analyzer");
    fs::rename(&parser, &renamed)?;
    assert_eq!(fixture.mosh.scan(&src, &cancel)?.errors, 0);
    assert_eq!(
        fixture
            .mosh
            .get(renamed.join("léxer.bin").to_str().unwrap())?
            .id,
        id
    );
    assert!(
        fixture
            .search(r#"kind:file text:"parser lexer""#)?
            .is_empty()
    );
    assert_eq!(
        fixture.search(r#"kind:file text:"analyzer lexer""#)?,
        ["léxer.bin"]
    );
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
    assert!(object.id.starts_with('@') && object.id.len() < 6);
    assert_eq!(fixture.search("allocated:>=0 ext:txt")?, ["win.txt"]);
    assert_eq!(fixture.search("mime:text/plain")?, ["win.txt"]);
    let raw = rusqlite::Connection::open(&fixture.db)?;
    let stored: (String, i64, bool, i64) = raw.query_row(
        "SELECT typeof(native_ref), length(native_ref),
            path_folder IS NOT NULL AND path_literal IS NULL AND mime IS NULL,
            native_volume FROM objects WHERE name='win.txt'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    assert_eq!(stored.0, "blob");
    assert!((1..=16).contains(&stored.1));
    assert!(stored.2);
    assert!(stored.3 > 0);
    let stored_live_fields: i64 = raw.query_row(
        "SELECT count(*) FROM pragma_table_xinfo('objects') WHERE hidden=0
         AND name IN ('path','extension','allocated','accessed','attributes','volume','filesystem','hardlinks','size','modified','changed','native_id','id')",
        [], |row| row.get(0),
    )?;
    assert_eq!(stored_live_fields, 0);
    assert!(fixture.mosh.history(Some(&object.id), 20)?.is_empty());
    Ok(())
}

#[cfg(windows)]
#[test]
fn resolves_an_external_rename_by_native_id_with_destination_permissions() -> Result<()> {
    let fixture = Fixture::new()?;
    let original = fixture.root.join("original.txt");
    let restricted = fixture.root.join("restricted");
    fs::create_dir(&restricted)?;
    fs::write(&original, "a unique file on disk")?;
    fixture.scan()?;
    let id = fixture.mosh.get(original.to_str().unwrap())?.id;
    fixture.mosh.tag(&id, "keep-my-tag", true, 1.0)?;
    fixture.mosh.person("visitor", None)?;
    fixture.mosh.secret(restricted.to_str().unwrap(), true)?;
    let visitor = Mosh::as_actor(&fixture.db, "visitor")?;
    let destination = restricted.join("moved.txt");
    fs::rename(&original, &destination)?;
    let recovered = fixture.mosh.get(&id)?;
    assert_eq!(recovered.path.as_deref(), destination.to_str());
    assert_eq!(fixture.mosh.preview(&id)?, "a unique file on disk");
    assert_ne!(visitor.get(&id)?.path.as_deref(), destination.to_str());
    assert!(visitor.preview(&id)?.is_empty());
    fixture.scan()?;
    assert!(visitor.get(&id).is_err());
    assert_eq!(fixture.search("direct-tag:keep-my-tag")?, ["moved.txt"]);
    Ok(())
}
