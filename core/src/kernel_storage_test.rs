use super::*;
use crate::{Mosh, SearchOptions};
use rusqlite::{Transaction, TransactionBehavior};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::AtomicBool,
    time::Instant,
};

// Use the production writer with shared project tags and variable per-file tags.
fn measure_densities(mosh: &Mosh, files: i64, source_bytes: i64) -> Result<()> {
    let vocabulary = (0..2048)
        .map(|index| {
            crate::organization::dictionary_id(&mosh.db, &format!("fixture-topic-{index:04}"))
        })
        .collect::<Result<Vec<_>>>()?;
    for density in [100, 1000] {
        let start = Instant::now();
        let mut last = 0;
        loop {
            let tx = Transaction::new_unchecked(&mosh.db, TransactionBehavior::Immediate)?;
            let batch = tx.prepare_cached("SELECT k.key FROM object_keys k JOIN objects o ON o.key=k.key
                WHERE o.missing=0 AND o.kind='file' AND o.reparse=0 AND k.key>? ORDER BY k.key LIMIT 4096")?
                .query_map([last], |r| r.get::<_, i64>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            if batch.is_empty() {
                break;
            }
            let mut writer = Writer::new(&tx);
            for key in batch {
                let (old_set, old) = read_set(&tx, key)?;
                let mut tags = old
                    .iter()
                    .filter(|(tag, _)| **tag < vocabulary[0])
                    .map(|(&tag, &origins)| (tag, origins))
                    .collect::<Tags>();
                // 75% are shared with a locality cohort. The others vary per file.
                let start_index = ((key as usize / 257) % 8) * 151;
                for &id in &vocabulary[start_index..start_index + density * 3 / 4] {
                    tags.entry(id).or_insert(USER);
                }
                let mut entropy = key as u64;
                while tags.len() < density {
                    entropy = entropy.wrapping_mul(6364136223846793005).wrapping_add(1);
                    tags.entry(vocabulary[(entropy >> 32) as usize % vocabulary.len()])
                        .or_insert(USER);
                }
                writer.replace(key, old_set, &old, &tags)?;
                last = key;
            }
            writer.flush()?;
            tx.commit()?;
        }
        let write_ms = start.elapsed().as_millis();
        mosh.compact()?;
        let bytes = fs::metadata(mosh.db_path())?.len();
        let tags_bytes:i64=mosh.db.query_row("SELECT sum(pgsize) FROM dbstat WHERE
            name IN ('tag_sets','tag_set_owners','tag_dictionary','tag_aliases','tag_details','tag_blocks','sqlite_autoindex_tag_sets_1','sqlite_autoindex_tag_dictionary_1')",[],|r|r.get(0))?;
        let coverage = mosh.tag_coverage()?;
        assert!(coverage["direct_assignments"].as_i64().unwrap() >= files * density as i64);
        let start = Instant::now();
        let hits = mosh.search(
            "direct-tag:fixture-topic-0000 direct-tag:fixture-topic-0001",
            &SearchOptions::default(),
        )?;
        let intersection_ms = start.elapsed().as_millis();
        assert_eq!(hits.len(), SearchOptions::default().limit);
        assert_eq!(mosh.doctor()?["ok"], true);
        println!(
            "{}",
            json!({"tags_per_file":density,"tag_bytes":tags_bytes,"ledger_bytes":bytes,
            "added_percent":100.0*bytes as f64 / source_bytes as f64,"write_ms":write_ms,
            "intersection_ms":intersection_ms})
        );
        if files >= 95_000 {
            assert!(bytes as i64 * 4 < source_bytes);
        }
    }
    Ok(())
}

fn source_size(mosh: &Mosh) -> Result<(i64, i64)> {
    Ok(mosh.db.query_row(
        "SELECT count(*),sum(mosh_live_stat(path,native_id,created,reparse,'size')) FROM objects WHERE missing=0 AND kind='file' AND reparse=0",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?)
}

#[test]
#[ignore = "set MOSH_KERNEL_ROOT to an extracted kernel snapshot"]
fn kernel_storage_budget() -> Result<()> {
    let root = PathBuf::from(std::env::var_os("MOSH_KERNEL_ROOT").context("set MOSH_KERNEL_ROOT")?);
    let temporary = tempfile::tempdir()?;
    let mosh = Mosh::open(&temporary.path().join("ledger.sqlite3"))?;
    let scan = mosh.scan(&root, &AtomicBool::new(false))?;
    ensure!(scan.errors == 0, "scan warnings: {:?}", scan.warnings);
    let (files, source_bytes) = source_size(&mosh)?;
    assert!(files >= 95_000);
    mosh.compact()?;
    println!(
        "{}",
        json!({"phase":"scan","files":files,"source_bytes":source_bytes,
        "elapsed_ms":scan.elapsed_ms,"ledger_bytes":fs::metadata(mosh.db_path())?.len()})
    );
    measure_densities(&mosh, files, source_bytes)
}

fn footprint(path: &Path) -> Result<Value> {
    let db = Connection::open(path)?;
    let groups = db.query_row(
        "SELECT sum(CASE WHEN name IN ('audit','object_audit') THEN pgsize ELSE 0 END),
                sum(CASE WHEN name LIKE 'ledger_%' THEN pgsize ELSE 0 END),
                sum(CASE WHEN name IN ('objects','sqlite_autoindex_objects_1','object_keys',
                      'sqlite_autoindex_object_keys_1','live_path','object_path')
                      OR (name LIKE 'object_%' AND name<>'object_audit') THEN pgsize ELSE 0 END),
                sum(CASE WHEN name='legacy_object_ids' THEN pgsize ELSE 0 END) FROM dbstat",
        [],
        |r| {
            Ok(
                json!({"audit":r.get::<_,i64>(0)?,"name_and_path_fts":r.get::<_,i64>(1)?,
            "objects_and_lookup":r.get::<_,i64>(2)?,"legacy_aliases":r.get::<_,i64>(3)?}),
            )
        },
    )?;
    Ok(json!({"bytes":fs::metadata(path)?.len(),"groups":groups}))
}

#[test]
#[ignore = "set MOSH_FORMAT_3_CLI to the previous release CLI"]
fn supplemental_storage_budget() -> Result<()> {
    let baseline = std::env::var_os("MOSH_FORMAT_3_CLI").context("set MOSH_FORMAT_3_CLI")?;
    let temporary = tempfile::tempdir()?;
    let root = temporary.path().join("source");
    fs::create_dir(&root)?;
    for group in 0..100 {
        let folder = root.join(format!("subsystem-{group:03}/src/development"));
        fs::create_dir_all(&folder)?;
        for file in 0..100 {
            let path = folder.join(format!("module-{group:03}-{file:03}.c"));
            fs::write(
                path,
                "/* supplemental Ledger workload */\nint mosh_example(void) { return 42; }\n",
            )?;
        }
    }
    let older = temporary.path().join("old.sqlite3");
    let baseline_call = |args: &[&std::ffi::OsStr]| -> Result<Value> {
        let output = Command::new(&baseline)
            .arg("--db")
            .arg(&older)
            .args(args)
            .output()?;
        ensure!(
            output.status.success(),
            "baseline CLI: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(serde_json::from_slice(&output.stdout)?)
    };
    let old_scan = baseline_call(&["scan".as_ref(), root.as_os_str()])?;
    assert_eq!(old_scan["errors"], 0);
    baseline_call(&["ledger".as_ref(), "compact".as_ref()])?;
    let before = footprint(&older)?;
    let example = root.join("subsystem-000/src/development/module-000-000.c");
    let legacy = baseline_call(&["inspect".as_ref(), example.as_os_str()])?["object"]["id"].clone();
    baseline_call(&[
        "tag".as_ref(),
        "add".as_ref(),
        example.as_os_str(),
        "keep-on-upgrade".as_ref(),
    ])?;

    let fresh = Mosh::open(&temporary.path().join("new.sqlite3"))?;
    let scan = fresh.scan(&root, &AtomicBool::new(false))?;
    assert_eq!(scan.errors, 0, "{:?}", scan.warnings);
    let (files, _) = source_size(&fresh)?;
    assert_eq!(files, 10_000);
    fresh.compact()?;
    let after = footprint(fresh.db_path())?;
    assert!(after["bytes"].as_u64().unwrap() < before["bytes"].as_u64().unwrap() * 3 / 4);
    assert_eq!(fresh.doctor()?["ok"], true);

    let upgraded = Mosh::open(&older)?;
    let legacy = legacy.as_str().context("original file ID")?;
    let actual = upgraded.get(legacy)?;
    assert!(actual.id.starts_with('@') && actual.id.len() < 6);
    assert!(
        upgraded
            .tags(legacy)?
            .iter()
            .any(|tag| tag["name"] == "keep-on-upgrade")
    );
    assert!(
        upgraded
            .history(Some(legacy), 10)?
            .iter()
            .any(|row| row["action"] == "discovered")
    );
    assert_eq!(
        upgraded.search(&format!("id:{legacy}"), &SearchOptions::default())?[0].id,
        actual.id
    );
    assert_eq!(upgraded.doctor()?["ok"], true);
    upgraded.compact()?;
    println!(
        "{}",
        json!({"files": files, "format_3":before,"format_4_fresh":after,
        "format_4_upgraded":footprint(upgraded.db_path())?,"baseline_scan_ms":old_scan["elapsed_ms"],
        "fresh_scan_ms":scan.elapsed_ms})
    );

    // The tiny synthetic source contents only exercise the index. The comparison
    // above measures Ledger bytes across identical paths, independently of source size.
    let (_, source) = source_size(&fresh)?;
    measure_densities(&fresh, files, source)
}

#[test]
#[ignore = "measures compacted pages, journals and churn on generated files"]
fn storage_churn_workload() -> Result<()> {
    use std::{sync::Arc, thread, time::Duration};
    let fixture = tempfile::Builder::new()
        .prefix("mosh-storage-churn-")
        .tempdir()?;
    let scratch = fixture.path().join("scratch");
    fs::create_dir(&scratch)?;
    let mut records = Vec::new();
    for (kind, mode) in [("repeated", 0), ("overlap", 1), ("diverse", 2)] {
        let root = fixture.path().join(format!("source-{mode}"));
        for folder in 0..24 {
            fs::create_dir_all(root.join(format!("component-{folder:02}/subsystem")))?;
        }
        let path_for =
            |n: usize| root.join(format!("component-{:02}/subsystem/unit-{n:04}.rs", n % 24));
        let count = 1200;
        for n in 0..count {
            fs::write(
                path_for(n),
                format!(
                    "// generated unit {n}\n{}",
                    "const COUNT: usize = 1;\n".repeat(170)
                ),
            )?;
        }
        let mosh = Mosh::open(&fixture.path().join(format!("mode-{mode}.sqlite3")))?;
        let quote = scratch.to_str().unwrap().replace('\'', "''");
        mosh.db
            .execute_batch(&format!("PRAGMA temp_store_directory='{quote}';"))?;
        let stop = Arc::new(AtomicBool::new(false));
        let reader = stop.clone();
        let wal = mosh.db_path().with_extension("sqlite3-wal");
        let journal = mosh.db_path().with_extension("sqlite3-journal");
        let temp = scratch.clone();
        let peak = thread::spawn(move || {
            let mut journals = 0;
            let mut temporary = 0;
            while !reader.load(std::sync::atomic::Ordering::Relaxed) {
                let bytes = |path: &Path| fs::metadata(path).map_or(0, |m| m.len());
                journals = journals.max(bytes(&wal) + bytes(&journal));
                temporary = temporary.max(
                    fs::read_dir(&temp)
                        .into_iter()
                        .flatten()
                        .flatten()
                        .map(|entry| bytes(&entry.path()))
                        .sum::<u64>(),
                );
                thread::sleep(Duration::from_millis(4));
            }
            (journals, temporary)
        });
        let measured = (|| -> Result<Value> {
            let cancel = AtomicBool::new(false);
            let scan = mosh.scan(&root, &cancel)?;
            assert_eq!(scan.errors, 0);
            let first = mosh.find(path_for(100).to_str().unwrap())?;
            mosh.set_paint(&first.id, "minimal", "Keep this annotation", true)?;
            mosh.tag(&first.id, "preserved-note", true, 1.0)?;
            mosh.compact()?;
            let fresh = mosh.storage_usage()?;
            let lookup_paths = (0..250)
                .map(|n| dunce::canonicalize(path_for((n * 7) % count)))
                .collect::<std::io::Result<Vec<_>>>()?;
            let index_read = |hash: bool| -> Result<f64> {
                let sql = if hash {
                    "SELECT id FROM objects WHERE missing=0 AND path IS NOT NULL AND
                    mosh_path_key(path)=mosh_path_key(?1) AND path=?1"
                } else {
                    "SELECT id FROM objects WHERE missing=0 AND path=?1"
                };
                let mut query = mosh.db.prepare(sql)?;
                let time = Instant::now();
                for path in &lookup_paths {
                    assert!(
                        !query
                            .query_row([path.to_str().unwrap()], |r| r.get::<_, String>(0))?
                            .is_empty()
                    );
                }
                Ok(time.elapsed().as_secs_f64() * 4.0)
            };
            let compact_lookup = index_read(true)?;
            let mut legacy = Value::Null;
            if mode == 0 {
                let path_update: String = mosh.db.query_row(
                    "SELECT sql FROM sqlite_schema WHERE name='unique_live_path_update'",
                    [],
                    |row| row.get(0),
                )?;
                mosh.db.execute_batch("DROP TRIGGER unique_live_path_insert; DROP TRIGGER unique_live_path_update;
                    DROP INDEX live_path_key; CREATE UNIQUE INDEX live_path ON objects(path) WHERE missing=0 AND path IS NOT NULL;
                    DROP INDEX object_git; CREATE INDEX object_git ON objects(git_root,git_status);
                    DROP INDEX object_parent_key;
                    CREATE INDEX object_parent ON objects(parent_key,missing,name COLLATE NOCASE);
                    UPDATE file_tiers SET assessor='filesystem' WHERE assessor='f';")?;
                mosh.compact()?;
                legacy = json!({"storage":mosh.storage_usage()?,"exact_path_mean_ms":index_read(false)?});
                mosh.db.execute_batch(
                    &include_str!("ledger_v6.sql").replace("DROP INDEX file_tier_rank;", "")
                        .replace("CREATE INDEX object_parent ON objects(parent,missing,name COLLATE NOCASE);",
                                 "CREATE INDEX object_parent_key ON objects(parent_key) WHERE missing=0;"),
                )?;
                mosh.db
                    .execute_batch("DROP TRIGGER unique_live_path_update;")?;
                mosh.db.execute_batch(&path_update)?;
            }
            let vocabulary = (0..2048)
                .map(|n| crate::organization::dictionary_id(&mosh.db, &format!("load:{n:04}")))
                .collect::<Result<Vec<_>>>()?;
            let fill = |density: usize| -> Result<Value> {
                let time = Instant::now();
                let tx = Transaction::new_unchecked(&mosh.db, TransactionBehavior::Immediate)?;
                let mut statement=tx.prepare("SELECT k.key FROM object_keys k JOIN objects o ON o.key=k.key WHERE o.kind='file' AND o.missing=0 ORDER BY k.key")?;
                let keys = statement
                    .query_map([], |r| r.get::<_, i64>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                drop(statement);
                let mut writer = Writer::new(&tx);
                for (i, key) in keys.into_iter().enumerate() {
                    let (id, before) = read_set(&tx, key)?;
                    let mut tags: Tags = before
                        .iter()
                        .filter(|(tag, _)| **tag < vocabulary[0])
                        .map(|(&tag, &bits)| (tag, bits))
                        .collect();
                    let shared = match mode {
                        0 => density,
                        1 => density * 3 / 4,
                        _ => 0,
                    };
                    let seed = if mode == 1 { (i / 40 % 8) * 151 } else { 0 };
                    for &tag in &vocabulary[seed..seed + shared] {
                        if tags.len() == density {
                            break;
                        }
                        tags.insert(tag, USER);
                    }
                    let mut rng = i as u64 + 1;
                    while tags.len() < density {
                        rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
                        tags.entry(vocabulary[(rng >> 32) as usize % vocabulary.len()])
                            .or_insert(USER);
                    }
                    writer.replace(key, id, &before, &tags)?;
                }
                writer.flush()?;
                tx.commit()?;
                let index_ms = time.elapsed().as_millis();
                mosh.compact()?;
                let storage = mosh.storage_usage()?;
                let time = Instant::now();
                for _ in 0..10 {
                    mosh.search(
                        "direct-tag:load:0000 direct-tag:load:0001",
                        &SearchOptions {
                            limit: 25,
                            ..Default::default()
                        },
                    )?;
                }
                Ok(
                    json!({"tags":density,"storage":storage,"write_ms":index_ms,"intersection_mean_ms":time.elapsed().as_secs_f64()*100.0}),
                )
            };
            let before = [fill(100)?, fill(1000)?];
            for n in 0..60 {
                fs::write(
                    path_for(n),
                    "// source updated after initial import".repeat(130),
                )?;
                let renamed = path_for(n + 60).with_file_name(format!("renamed-{n:04}.rs"));
                fs::rename(path_for(n + 60), renamed)?;
                fs::remove_file(path_for(n + 120))?;
            }
            let churn = mosh.scan(&root, &cancel)?;
            let repeat = mosh.scan(&root, &cancel)?;
            assert_eq!(churn.errors + repeat.errors, 0);
            assert_eq!(repeat.changed, 0);
            assert_eq!(
                mosh.paint(&first.id)?
                    .context("renamed file lost its Paint")?["body"],
                "Keep this annotation"
            );
            let after = [fill(100)?, fill(1000)?];
            Ok(
                json!({"workload":kind,"scan_ms":scan.elapsed_ms,"churn_scan_ms":churn.elapsed_ms,
                "recheck_ms":repeat.elapsed_ms,"legacy_layout":legacy,"fresh":fresh,"exact_path_mean_ms":compact_lookup,
                "before_churn":before,"after_churn":after}),
            )
        })();
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let (journal, temp) = peak.join().expect("storage sampler");
        let mut record = measured?;
        record["sampled_peak_journal_bytes"] = json!(journal);
        record["sampled_peak_temp_bytes"] = json!(temp);
        records.push(record);
    }
    for mut record in records {
        // The doctor output still exposes per-table totals on demand.
        let trim = |v: &mut Value| {
            if let Some(map) = v.as_object_mut() {
                map.remove("tables");
            }
        };
        trim(&mut record["fresh"]);
        trim(&mut record["legacy_layout"]["storage"]);
        for phase in ["before_churn", "after_churn"] {
            for row in record[phase].as_array_mut().unwrap() {
                trim(&mut row["storage"]);
            }
        }
        println!("{record}");
    }
    Ok(())
}
