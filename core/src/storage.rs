use crate::{Mosh, canonical, fsmeta, object, path_text, prefix};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, Transaction, TransactionBehavior, params};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{BufReader, Read},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

fn page_group(name: &str) -> &'static str {
    match name {
        "objects"
        | "object_keys"
        | "legacy_object_ids"
        | "sqlite_autoindex_objects_1"
        | "sqlite_autoindex_object_keys_1" => "file_records",
        "audit" | "object_audit" | "tag_audit_members" => "audit_history",
        "tag_history" | "tag_history_tag" | "tag_history_object" => "vocabulary_history",
        "file_tiers" | "file_tier_rank" | "tiers" => "tier_records",
        "native_volumes" | "sqlite_autoindex_native_volumes_1" => "native_volume_dictionary",
        "legacy_public_id" => "legacy_object_aliases",
        "path_folders" | "sqlite_autoindex_path_folders_1" => "path_directory_dictionary",
        "live_path" | "live_path_key" | "object_path" => "file_indexes",
        "text_cached" => "source_text_cache",
        "paint" => "paint",
        "embeddings"
        | "vectors_model"
        | "retrievals"
        | "retrieval_feedback"
        | "passages"
        | "passage_relation"
        | "passage_tags"
        | "tag_passages"
        | "sqlite_autoindex_passages_1"
        | "sqlite_autoindex_retrieval_feedback_1"
        | "recommendations"
        | "coll_ai_policy"
        | "coverage" => "optional_intelligence",
        _ if name.starts_with("object_") => "file_indexes",
        _ if name.starts_with("ledger_") => "name_path_search",
        _ if name.starts_with("directory_fts_") => "directory_path_search",
        _ if name.starts_with("tag_") || name.starts_with("sqlite_autoindex_tag_") => {
            "packed_tags_and_registry"
        }
        _ if name.starts_with("text_terms_") => "source_text_cache",
        _ if name.starts_with("ai_")
            || name.starts_with("sqlite_autoindex_ai_")
            || name.starts_with("retrieval_")
            || name.starts_with("mosh_evaluation_") =>
        {
            "optional_intelligence"
        }
        _ => "other",
    }
}

fn sidecar_bytes(path: &Path, suffix: &str) -> u64 {
    let mut sidecar = path.as_os_str().to_os_string();
    sidecar.push(suffix);
    fs::metadata(sidecar).map_or(0, |metadata| metadata.len())
}

fn equals(a: &mut File, b: &mut File) -> std::io::Result<bool> {
    use std::io::{Seek, SeekFrom};
    a.seek(SeekFrom::Start(0))?;
    b.seek(SeekFrom::Start(0))?;
    let mut a = BufReader::with_capacity(64 * 1024, a);
    let mut b = BufReader::with_capacity(64 * 1024, b);
    let mut buf1 = [0; 64 * 1024];
    let mut buf2 = [0; 64 * 1024];
    loop {
        let n = a.read(&mut buf1)?;
        if n == 0 {
            return Ok(b.read(&mut buf2[..1])? == 0);
        }
        b.read_exact(&mut buf2[..n])?;
        if buf1[..n] != buf2[..n] {
            return Ok(false);
        }
    }
}

fn checked_snapshot(item: &crate::Object) -> Result<File> {
    let path = Path::new(item.path.as_deref().context("expected a file path")?);
    ensure!(
        fsmeta::matches_indexed(item, path),
        "file changed since indexing: {}",
        path.display()
    );
    Ok(fsmeta::open_snapshot(path)?)
}

impl Mosh {
    pub fn storage_usage(&self) -> Result<Value> {
        self.admin()?;
        let read = self.db.unchecked_transaction()?;
        let pragma = |name| read.pragma_query_value(None, name, |r| r.get::<_, i64>(0));
        let page_size = pragma("page_size")?;
        let database = page_size * pragma("page_count")?;
        let reusable = page_size * pragma("freelist_count")?;
        let mut groups = BTreeMap::new();
        let mut tables = BTreeMap::new();
        let mut occupied = 0;
        {
            let mut query = read.prepare("SELECT name,sum(pgsize) FROM dbstat GROUP BY name")?;
            for result in
                query.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
            {
                let (name, bytes) = result?;
                *groups.entry(page_group(&name)).or_insert(0) += bytes;
                occupied += bytes;
                tables.insert(name, bytes);
            }
        }
        let (files, source, scan, durable):(i64,i64,i64,i64) = read.query_row("SELECT
            (SELECT count(*) FROM objects WHERE missing=0 AND kind='file' AND reparse=0),
            (SELECT coalesce(sum(mosh_live_stat(path,native_id,created,reparse,'size')),0) FROM objects WHERE missing=0 AND kind='file' AND reparse=0),
            (SELECT count(*) FROM audit WHERE action IN ('discovered','external-modified','external-move','external-missing')),
            (SELECT count(*) FROM audit WHERE action NOT IN ('discovered','external-modified','external-move','external-missing'))",
            [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
        read.commit()?;
        Ok(
            json!({"database_bytes":database,"occupied_bytes":occupied,"reusable_bytes":reusable,
            "structural_bytes":database-occupied-reusable,"groups":groups,"tables":tables,
            "files":files,"source_bytes":source,"routine_scan_events":scan,"durable_events":durable,
            "physical_bytes":fs::metadata(&self.db_path)?.len(),
            "wal_bytes":sidecar_bytes(&self.db_path,"-wal"),"shm_bytes":sidecar_bytes(&self.db_path,"-shm"),
            "rollback_journal_bytes":sidecar_bytes(&self.db_path,"-journal")}),
        )
    }

    pub fn duplicates(&self, root: Option<&Path>, cancel: &AtomicBool) -> Result<Value> {
        self.admin()?;
        let start = Instant::now();
        let scope = root.map(canonical).transpose()?;
        if let Some(path) = &scope {
            self.outside_children(path)?;
        }
        let range = scope.as_deref().map(path_text).transpose()?.map(prefix);
        let filter = "o.kind='file' AND o.missing=0 AND o.reparse=0 AND o.path IS NOT NULL
            AND (?1 IS NULL OR (o.path>=?1 AND o.path<?2))";
        let args = params![range.as_ref().map(|r| &r.0), range.as_ref().map(|r| &r.1)];
        let tx = Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)?;
        tx.execute_batch(
            "CREATE TEMP TABLE duplicate_sizes(id INTEGER PRIMARY KEY,size INTEGER);
            CREATE INDEX duplicate_size ON duplicate_sizes(size);
            CREATE TEMP TABLE samples(id INTEGER PRIMARY KEY,partial TEXT NOT NULL);
            CREATE INDEX sample_partial ON samples(partial);",
        )?;
        tx.execute(&format!("INSERT INTO duplicate_sizes
            SELECT key,mosh_live_stat(path,native_id,created,reparse,'size') FROM objects o WHERE {filter}"),args)?;
        let mut sizes = tx.prepare(
            "SELECT size FROM duplicate_sizes WHERE size IS NOT NULL
            GROUP BY size HAVING count(*)>1 ORDER BY size",
        )?;
        let results = sizes.query_map([], |r| r.get::<_, i64>(0))?;
        let mut inspected = 0;
        let mut full_bytes = 0u64;
        let mut errors = 0;
        for size in results {
            ensure!(!cancel.load(Ordering::Relaxed), "duplicate scan cancelled");
            let size = size?;
            let mut files = tx.prepare_cached(
                "SELECT o.* FROM duplicate_sizes s JOIN objects o ON o.key=s.id WHERE s.size=?",
            )?;
            let files = files.query_map([size], object)?;
            for file in files {
                ensure!(!cancel.load(Ordering::Relaxed), "duplicate scan cancelled");
                let item = file?;
                if let Ok(mut file) = checked_snapshot(&item) {
                    if let Ok(partial) = fsmeta::partial(&mut file, size as u64) {
                        tx.prepare_cached("INSERT INTO samples VALUES(?,?)")?
                            .execute(params![item.key, partial])?;
                        inspected += 1;
                        continue;
                    }
                }
                errors += 1;
            }
            let mut candidates=tx.prepare_cached("SELECT o.* FROM samples s JOIN objects o ON o.key=s.id
                WHERE (SELECT count(*) FROM (SELECT 1 FROM samples s2 WHERE s2.partial=s.partial LIMIT 2))=2")?;
            for item in candidates.query_map([], object)? {
                ensure!(!cancel.load(Ordering::Relaxed), "duplicate scan cancelled");
                let item = item?;
                let digest = (|| -> Result<String> {
                    let mut file = checked_snapshot(&item)?;
                    let hash = fsmeta::hash(&mut file)?;
                    checked_snapshot(&item)?;
                    Ok(hash)
                })();
                if let Ok(digest) = digest {
                    full_bytes = full_bytes.saturating_add(size as u64);
                    tx.execute(
                        "UPDATE objects SET hash=? WHERE key=?",
                        params![digest, item.key],
                    )?;
                } else {
                    errors += 1;
                }
            }
            tx.execute("DELETE FROM samples", [])?;
        }
        drop(sizes);
        let mut groups = tx.prepare(
            "SELECT min(s.size),o.hash,count(*) FROM duplicate_sizes s JOIN objects o ON o.key=s.id
            WHERE s.size IS NOT NULL AND o.hash IS NOT NULL GROUP BY o.hash
            HAVING count(*)>1 ORDER BY min(s.size)*(count(*)-1) DESC LIMIT 500",
        )?;
        let group_rows = groups
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(groups);
        let mut report = Vec::new();
        for group in group_rows {
            let (size, hash, total) = group;
            let mut members = self.db.prepare(&format!(
                "SELECT o.* FROM objects o WHERE {filter} AND hash=?3 ORDER BY path LIMIT 1000"
            ))?;
            let mut members = members.query_map(
                params![
                    range.as_ref().map(|r| &r.0),
                    range.as_ref().map(|r| &r.1),
                    hash
                ],
                object,
            )?;
            let mut output = Vec::new();
            let mut physical = std::collections::HashSet::new();
            let mut representative = None;
            for item in members.by_ref() {
                let mut item = item?;
                let Ok(mut reader) = checked_snapshot(&item) else {
                    errors += 1;
                    continue;
                };
                if let Some(first) = &mut representative {
                    match equals(first, &mut reader) {
                        Ok(true) => {}
                        _ => {
                            errors += 1;
                            continue;
                        }
                    }
                } else {
                    representative = Some(reader);
                }
                physical.insert(item.native_id.clone().unwrap_or_else(|| item.id.clone()));
                fsmeta::populate(&mut item);
                output.push(item);
            }
            if output.len() > 1 {
                let reclaimable = if total == output.len() as i64
                    && output
                        .iter()
                        .all(|m| m.hardlinks == Some(1) && m.allocated.is_some())
                {
                    let sum = output.iter().filter_map(|m| m.allocated).sum::<i64>();
                    Some(sum - output.iter().filter_map(|m| m.allocated).max().unwrap_or(0))
                } else {
                    None
                };
                report.push(
                    json!({"hash":hash,"size":size,"members":output,"total_candidates":total,
                    "physical_files":physical.len(),"reclaimable_allocated":reclaimable}),
                );
            }
        }
        tx.execute_batch("DROP TABLE samples; DROP TABLE duplicate_sizes;")?;
        tx.commit()?;
        Ok(
            json!({"groups":report,"sampled":inspected,"hashed_bytes":full_bytes,"errors":errors,"elapsed_ms":start.elapsed().as_millis() as u64}),
        )
    }

    pub fn space(&self, root: &Path) -> Result<Value> {
        self.admin()?;
        let root = canonical(root)?;
        self.outside_children(&root)?;
        let root = path_text(&root)?;
        let (low, high) = prefix(root);
        let scope = "kind='file' AND reparse=0 AND missing=0 AND path>=?1 AND path<?2";
        let cte = format!(
            "WITH sizes AS MATERIALIZED (
            SELECT key,path,extension,mosh_live_stat(path,native_id,created,reparse,'size') size
            FROM objects WHERE {scope})"
        );
        let totals = self.db.query_row(
            &format!(
                "WITH live AS MATERIALIZED (
            SELECT id,mosh_live_stat(path,native_id,created,reparse,'size') size,native_id,mosh_live_allocated(path,native_id,created,reparse) allocated
            FROM objects WHERE {scope}
          ), physical AS (
            SELECT max(allocated) bytes FROM live GROUP BY coalesce(native_id,id)
          ) SELECT count(*),coalesce(sum(size),0),count(*)-count(allocated),
             (SELECT coalesce(sum(bytes),0) FROM physical) FROM live"
            ),
            params![low, high],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            },
        )?;
        let mut distribution = self.db.prepare(&format!(
            "{cte} SELECT
            CASE WHEN size<1024 THEN 'Under 1 KiB' WHEN size<1048576 THEN '1 KiB–1 MiB'
                 WHEN size<1073741824 THEN '1 MiB–1 GiB' ELSE '1 GiB or larger' END,
            count(*),sum(size),min(size) FROM sizes WHERE size IS NOT NULL GROUP BY 1 ORDER BY 4"
        ))?;
        let distribution = distribution
            .query_map(params![low, high], |r| {
                Ok(json!({
            "label":r.get::<_,String>(0)?,"files":r.get::<_,i64>(1)?,"logical":r.get::<_,i64>(2)?}))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut kinds = self.db.prepare(&format!(
            "{cte} SELECT extension,count(*),sum(size) FROM sizes WHERE size IS NOT NULL
            GROUP BY extension ORDER BY sum(size) DESC LIMIT 20"
        ))?;
        let kinds = kinds
            .query_map(params![low, high], |r| {
                Ok(json!({"extension":r.get::<_,Option<String>>(0)?,
            "files":r.get::<_,i64>(1)?,"logical":r.get::<_,i64>(2)?}))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut largest = self.db.prepare(&format!(
            "{cte} SELECT o.* FROM sizes s JOIN objects o ON o.key=s.key WHERE s.size IS NOT NULL ORDER BY s.size DESC LIMIT 20"
        ))?;
        let mut largest = largest
            .query_map(params![low, high], object)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for item in &mut largest {
            fsmeta::populate(item);
        }
        let mut tree=self.db.prepare(&format!("{cte} SELECT CASE WHEN instr(substr(path,length(?1)+1),?3)=0 THEN substr(path,length(?1)+1)
            ELSE substr(path,length(?1)+1,instr(substr(path,length(?1)+1),?3)-1) END name,
            count(*),sum(size) FROM sizes WHERE size IS NOT NULL GROUP BY name ORDER BY sum(size) DESC LIMIT 100"))?;
        let tree=tree.query_map(params![low,high,std::path::MAIN_SEPARATOR_STR],|r|Ok(json!({
            "name":r.get::<_,String>(0)?,"files":r.get::<_,i64>(1)?,"logical":r.get::<_,i64>(2)?
        })))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(
            json!({"root":root,"files":totals.0,"logical":totals.1,"known_allocated":totals.3,
            "unknown_allocation":totals.2,"largest":largest,"types":kinds,"distribution":distribution,"tree":tree}),
        )
    }

    pub fn doctor(&self) -> Result<Value> {
        self.admin()?;
        let mut statement = self.db.prepare("PRAGMA integrity_check")?;
        let integrity = statement
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let foreign_violations =
            self.db
                .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
                    r.get::<_, i64>(0)
                })?;
        let uncertain = self.db.query_row(
            "SELECT count(*) FROM plan_steps WHERE state IN ('intent','undo-intent')",
            [],
            |r| r.get::<_, i64>(0),
        )?;
        let active =
            self.db
                .query_row("SELECT count(*) FROM objects WHERE missing=0", [], |r| {
                    r.get::<_, i64>(0)
                })?;
        let page_count = self
            .db
            .pragma_query_value(None, "page_count", |r| r.get::<_, i64>(0))?;
        let page_size = self
            .db
            .pragma_query_value(None, "page_size", |r| r.get::<_, i64>(0))?;
        let reusable = self
            .db
            .pragma_query_value(None, "freelist_count", |r| r.get::<_, i64>(0))?;
        let (forward, reverse):(i64,i64)=self.db.query_row("SELECT
            coalesce((SELECT sum(s.confirmed) FROM object_keys k JOIN tag_sets s ON s.id=k.tag_set),0),
            coalesce((SELECT sum(mosh_block_size(packed)) FROM tag_blocks WHERE suggested=0),0)",
            [],|r|Ok((r.get(0)?,r.get(1)?)))?;
        let tag_pages: i64 = self.db.query_row(
            "SELECT coalesce(sum(pgsize),0) FROM dbstat WHERE name GLOB 'tag_*'
            OR name GLOB 'sqlite_autoindex_tag_*'",
            [],
            |r| r.get(0),
        )?;
        let storage = self.storage_usage()?;
        Ok(
            json!({"ok":integrity==["ok"]&&foreign_violations==0&&forward==reverse,"integrity":integrity,
            "foreign_key_violations":foreign_violations,"objects":active,
            "pending_recovery_steps":uncertain,"database_bytes":page_count*page_size,"reusable_bytes":reusable*page_size,
            "tag_storage_bytes":tag_pages,"confirmed_assignments":forward,"confirmed_postings":reverse,"storage":storage}),
        )
    }

    pub fn rebuild(&self) -> Result<()> {
        self.admin()?;
        let tx = self.db.unchecked_transaction()?;
        crate::packed_tags::rebuild(&tx)?;
        tx.commit()?;
        Ok(())
    }

    pub fn compact(&self) -> Result<()> {
        self.admin()?;
        self.db.execute_batch(
            "DELETE FROM path_folders WHERE id NOT IN
                (SELECT path_folder FROM objects WHERE path_folder IS NOT NULL);
             VACUUM; PRAGMA wal_checkpoint(TRUNCATE);",
        )?;
        Ok(())
    }

    pub fn backup(&self, destination: &Path) -> Result<()> {
        self.admin()?;
        let parent = destination
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let temp = tempfile::NamedTempFile::new_in(parent)?;
        {
            let mut backup = Connection::open(temp.path())?;
            rusqlite::backup::Backup::new(&self.db, &mut backup)?.run_to_completion(
                512,
                Duration::from_millis(15),
                None,
            )?;
        }
        temp.persist_noclobber(destination)?;
        Ok(())
    }

    pub fn restore(source: &Path, destination: &Path) -> Result<PathBuf> {
        ensure!(source.is_file(), "backup file is missing");
        let source =
            Connection::open_with_flags(source, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        crate::lean::register(&source)?;
        crate::fsmeta::register(&source)?;
        crate::metadata_search::register(&source)?;
        let version: i64 = source.pragma_query_value(None, "user_version", |r| r.get(0))?;
        ensure!(
            (1..=crate::LEDGER_VERSION).contains(&version),
            "unsupported Ledger backup format"
        );
        let status: String = source.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
        ensure!(status == "ok", "backup integrity check failed");
        if let Some(parent) = destination.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        let parent = destination
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let copy = tempfile::NamedTempFile::new_in(parent)?;
        {
            let mut restored = Connection::open(copy.path())?;
            rusqlite::backup::Backup::new(&source, &mut restored)?.run_to_completion(
                512,
                Duration::from_millis(15),
                None,
            )?;
        }
        copy.persist_noclobber(destination)?;
        canonical(destination)
    }
}
