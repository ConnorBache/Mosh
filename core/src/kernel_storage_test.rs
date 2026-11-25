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
            let batch = tx.prepare_cached("SELECT k.key FROM object_keys k JOIN objects o ON o.id=k.object
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
        "SELECT count(*),sum(size) FROM objects WHERE missing=0 AND kind='file' AND reparse=0",
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
