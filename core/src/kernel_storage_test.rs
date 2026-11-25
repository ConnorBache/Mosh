use super::*;
use crate::{Mosh, SearchOptions};
use rusqlite::{Transaction, TransactionBehavior};
use serde_json::json;
use std::{path::PathBuf, sync::atomic::AtomicBool, time::Instant};

// The system's own scan and the same writer used by commands persist the full workload.
// Synthetic names model shared project tags and variable per-file tags without claiming
// that every input file really has a thousand distinct, meaningful annotations.
#[test]
#[ignore = "set MOSH_KERNEL_ROOT to an extracted kernel snapshot"]
fn kernel_storage_budget() -> Result<()> {
    let root = PathBuf::from(std::env::var_os("MOSH_KERNEL_ROOT").context("set MOSH_KERNEL_ROOT")?);
    let temporary = tempfile::tempdir()?;
    let ledger = temporary.path().join("ledger.sqlite3");
    let mosh = Mosh::open(&ledger)?;
    let scan = mosh.scan(&root, &AtomicBool::new(false))?;
    ensure!(scan.errors == 0, "scan warnings: {:?}", scan.warnings);
    let (files, source_bytes): (i64, i64) = mosh.db.query_row(
        "SELECT count(*),sum(size) FROM objects WHERE missing=0 AND kind='file' AND reparse=0",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    assert!(files >= 95_000);
    mosh.compact()?;
    println!(
        "{}",
        json!({"phase":"scan","files":files,"source_bytes":source_bytes,
        "elapsed_ms":scan.elapsed_ms,"ledger_bytes":std::fs::metadata(&ledger)?.len()})
    );

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
        let bytes = std::fs::metadata(&ledger)?.len();
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
        assert!(bytes as i64 * 4 < source_bytes);
    }
    Ok(())
}
