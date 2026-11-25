use crate::{Mosh, Object, now, packed_tags};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Tier {
    pub level: u8,
    pub minimum_tags: u32,
    pub minimum_score: u8,
    pub passes: u8,
    pub paint: Option<String>,
    pub embed: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Weights {
    pub importance: u16,
    pub information: u16,
    pub hotness: u16,
    pub promote: bool,
    pub demote: bool,
    pub hysteresis: u8,
}

impl Default for Weights {
    fn default() -> Self {
        Self {
            importance: 50,
            information: 30,
            hotness: 20,
            promote: true,
            demote: true,
            hysteresis: 3,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Scores {
    pub importance: u8,
    pub information: u8,
    pub hotness: u8,
}

pub(crate) fn key(db: &Connection, id: &str) -> Result<i64> {
    Ok(db
        .prepare_cached("SELECT key FROM object_keys WHERE object=?")?
        .query_row([id], |r| r.get(0))?)
}

pub(crate) fn setting<T: for<'a> Deserialize<'a> + Default>(
    db: &Connection,
    name: &str,
) -> Result<T> {
    let value: Option<String> = db
        .prepare_cached("SELECT value FROM metadata WHERE key=?")?
        .query_row([name], |r| r.get(0))
        .optional()?;
    value
        .map(|text| serde_json::from_str(&text).map_err(Into::into))
        .transpose()
        .map(|value| value.unwrap_or_default())
}

pub(crate) fn set_setting(db: &Connection, name: &str, value: &impl Serialize) -> Result<()> {
    db.execute(
        "INSERT INTO metadata VALUES(?,?) ON CONFLICT DO UPDATE SET value=excluded.value",
        params![name, serde_json::to_string(value)?],
    )?;
    Ok(())
}

pub(crate) fn policies(db: &Connection) -> Result<Vec<Tier>> {
    let mut statement = db.prepare_cached(
        "SELECT level,minimum_tags,minimum_score,passes,paint,embed FROM tiers ORDER BY level",
    )?;
    Ok(statement
        .query_map([], |r| {
            Ok(Tier {
                level: r.get(0)?,
                minimum_tags: r.get(1)?,
                minimum_score: r.get(2)?,
                passes: r.get(3)?,
                paint: r.get(4)?,
                embed: r.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

fn predicted(path: &str, extension: &str) -> Scores {
    let generated = path.split(['\\', '/']).any(|part| {
        ["target", "node_modules", ".cache", "dist", "obj"]
            .iter()
            .any(|name| part.eq_ignore_ascii_case(name))
    });
    if generated {
        return Scores {
            importance: 5,
            information: 10,
            hotness: 4,
        };
    }
    let document = matches!(
        extension,
        "md" | "txt" | "eml" | "docx" | "pdf" | "rtf" | "jsonl"
    );
    let code = matches!(
        extension,
        "rs" | "py" | "ts" | "js" | "go" | "cpp" | "c" | "java"
    );
    Scores {
        importance: if document {
            46
        } else if code {
            34
        } else {
            22
        },
        information: if document {
            53
        } else if code {
            46
        } else {
            21
        },
        hotness: if code {
            43
        } else if document {
            36
        } else {
            18
        },
    }
}

pub(crate) fn observed_hotness(predicted: u8, accesses: u32, last: Option<i64>, time: i64) -> u8 {
    let days = last
        .map(|last| (time.saturating_sub(last).max(0) / 86_400_000).min(64))
        .unwrap_or(64);
    let signal = (accesses.min(12) * 8) / (1 + (days as u32 / 7));
    (u32::from(predicted) + signal).min(100) as u8
}

pub(crate) fn weighted(scores: &Scores, hotness: u8, weights: &Weights) -> u8 {
    let weights = [weights.importance, weights.information, weights.hotness].map(u32::from);
    let values = [scores.importance, scores.information, hotness].map(u32::from);
    let numerator = weights
        .into_iter()
        .zip(values)
        .map(|(weight, value)| weight * value)
        .sum::<u32>();
    (numerator / weights.iter().sum::<u32>().max(1)) as u8
}

pub(crate) fn recompute(db: &Connection, key: i64, fresh: bool) -> Result<()> {
    let stored = db
        .prepare_cached(
            "SELECT importance,information,predicted_hotness,human_accesses,last_access,
        target,requested,locked FROM file_tiers WHERE object_key=?",
        )?
        .query_row([key], |r| {
            Ok((
                Scores {
                    importance: r.get(0)?,
                    information: r.get(1)?,
                    hotness: r.get(2)?,
                },
                r.get::<_, u32>(3)?,
                r.get::<_, Option<i64>>(4)?,
                r.get::<_, u8>(5)?,
                r.get::<_, Option<u8>>(6)?,
                r.get::<_, bool>(7)?,
            ))
        })
        .optional()?;
    let Some((scores, accesses, last, previous, requested, locked)) = stored else {
        return Ok(());
    };
    let tiers = policies(db)?;
    let weights: Weights = setting(db, "tier_weights")?;
    let score = weighted(
        &scores,
        observed_hotness(scores.hotness, accesses, last, now()),
        &weights,
    );
    let previous_active = tiers.iter().any(|t| t.level == previous);
    let applicable = tiers
        .iter()
        .filter(|t| t.minimum_score <= score)
        .map(|t| t.level)
        .max()
        .unwrap_or(1);
    let use_previous = previous_active
        && applicable != previous
        && !fresh
        && ((applicable > previous
            && (!weights.promote
                || !tiers.iter().any(|t| {
                    t.level == applicable
                        && score >= t.minimum_score.saturating_add(weights.hysteresis).min(100)
                })))
            || (applicable < previous
                && (!weights.demote
                    || tiers.iter().any(|t| {
                        t.level == previous
                            && score.saturating_add(weights.hysteresis) >= t.minimum_score
                    }))));
    let target = requested
        .filter(|level| tiers.iter().any(|t| t.level == *level))
        .unwrap_or(if (locked && previous_active) || use_previous {
            previous
        } else {
            applicable
        });
    db.prepare_cached("UPDATE file_tiers SET target=?2,effective=(
        SELECT max(level) FROM tiers WHERE level<=?2 AND minimum_tags<=coalesce(
            (SELECT s.confirmed FROM object_keys k JOIN tag_sets s ON k.tag_set=s.id WHERE k.key=?1),0))
        WHERE object_key=?1")?.execute(params![key,target])?;
    Ok(())
}

pub(crate) fn assess(
    db: &Connection,
    key: i64,
    revision: i64,
    scores: &Scores,
    assessor: &str,
    note: Option<&str>,
) -> Result<()> {
    ensure!(
        [scores.importance, scores.information, scores.hotness]
            .iter()
            .all(|n| *n <= 100),
        "tier scores must be between 0 and 100"
    );
    let is_new = db
        .prepare_cached("SELECT NOT EXISTS(SELECT 1 FROM file_tiers WHERE object_key=?)")?
        .query_row([key], |r| r.get::<_, bool>(0))?;
    db.prepare_cached("INSERT INTO file_tiers(object_key,revision,importance,information,predicted_hotness,assessor,assessment,assessed)
        VALUES(?,?,?,?,?,?,?,?) ON CONFLICT(object_key) DO UPDATE SET
        revision=excluded.revision,importance=excluded.importance,information=excluded.information,
        predicted_hotness=excluded.predicted_hotness,assessor=excluded.assessor,assessment=excluded.assessment,assessed=excluded.assessed")?
        .execute(params![key,revision,scores.importance,scores.information,scores.hotness,assessor,note,now()])?;
    recompute(db, key, is_new)
}

pub(crate) fn indexed(db: &Connection, id: &str) -> Result<()> {
    let (key, revision, path, extension): (i64, i64, String, String) = db
        .prepare_cached(
            "SELECT k.key,o.revision,coalesce(o.path,''),coalesce(o.extension,'')
        FROM objects o JOIN object_keys k ON o.id=k.object WHERE o.id=?",
        )?
        .query_row([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
    assess(
        db,
        key,
        revision,
        &predicted(&path, &extension),
        "filesystem",
        None,
    )?;
    crate::intelligence::index_changed(db, id, revision)?;
    Ok(())
}

pub(crate) fn ensure_minimum(db: &Connection) -> Result<()> {
    let mut statement=db.prepare("SELECT o.id FROM objects o JOIN object_keys k ON k.object=o.id LEFT JOIN tag_sets s ON s.id=k.tag_set
        WHERE o.kind='file' AND coalesce(s.confirmed,0)=0")?;
    let rows = statement
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if !rows.is_empty() {
        let id = crate::organization::dictionary_id(db, "file")?;
        let mut writer = packed_tags::Writer::new(db);
        for object in rows {
            writer.assign(
                &object,
                id,
                packed_tags::FILESYSTEM,
                Some((1.0, Some("file metadata"))),
            )?;
        }
        writer.flush()?;
    }
    Ok(())
}

pub(crate) fn migrate(db: &Connection) -> Result<()> {
    db.execute_batch(include_str!("intelligent.sql"))?;
    ensure_minimum(db)?;
    db.execute("INSERT INTO file_tiers(object_key,revision,importance,information,predicted_hotness,target,effective,assessor,assessed)
        SELECT k.key,o.revision,22,21,18,1,1,'filesystem',? FROM objects o JOIN object_keys k ON o.id=k.object WHERE o.kind='file'",[now()])?;
    Ok(())
}

impl Mosh {
    pub fn tier_settings(&self) -> Result<Value> {
        self.admin()?;
        Ok(
            json!({"tiers":policies(&self.db)?,"weights":setting::<Weights>(&self.db,"tier_weights")?}),
        )
    }

    pub fn configure_tiers(&self, tiers: &[Tier], weights: &Weights) -> Result<Value> {
        self.admin()?;
        ensure!(
            tiers.len() >= 4 && tiers.len() <= 10,
            "configure T1 through T4 and up to six additional tiers"
        );
        ensure!(
            (1..=4).all(|level| tiers.iter().any(|t| t.level == level)),
            "T1 through T4 must have definitions"
        );
        ensure!(
            weights.importance != 0 || weights.information != 0 || weights.hotness != 0,
            "at least one score weight must be positive"
        );
        ensure!(weights.hysteresis <= 25, "hysteresis must be at most 25");
        let tx = Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)?;
        tx.execute("DELETE FROM tiers", [])?;
        for tier in tiers {
            ensure!(
                (1..=10).contains(&tier.level)
                    && tier.minimum_tags >= 1
                    && tier.minimum_score <= 100
                    && (1..=8).contains(&tier.passes),
                "invalid tier definition"
            );
            tx.execute(
                "INSERT INTO tiers VALUES(?,?,?,?,?,?)",
                params![
                    tier.level,
                    tier.minimum_tags,
                    tier.minimum_score,
                    tier.passes,
                    tier.paint,
                    tier.embed
                ],
            )?;
        }
        set_setting(&tx, "tier_weights", weights)?;
        let mut after = 0;
        loop {
            let keys=tx.prepare_cached("SELECT object_key FROM file_tiers WHERE object_key>? ORDER BY object_key LIMIT 1024")?
                .query_map([after],|r|r.get::<_,i64>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            if keys.is_empty() {
                break;
            }
            for key in keys {
                recompute(&tx, key, true)?;
                after = key;
            }
        }
        crate::event(
            &tx,
            None,
            "tier-settings",
            Some(&self.actor),
            None,
            None,
            "ok",
        )?;
        tx.commit()?;
        self.tier_settings()
    }

    pub fn file_tier(&self, reference: &str) -> Result<Value> {
        let obj = self.get(reference)?;
        if !self.permits(&obj, "paint")? {
            return Ok(Value::Null);
        }
        self.file_tier_for(&obj)
    }

    pub(crate) fn file_tier_for(&self, obj: &Object) -> Result<Value> {
        let value=self.db.prepare_cached("SELECT f.importance,f.information,f.predicted_hotness,f.human_accesses,f.last_access,
            f.target,f.effective,f.requested,f.locked,f.assessor,f.assessment,f.revision,
            coalesce(s.confirmed,0),t.minimum_tags FROM object_keys k JOIN file_tiers f ON f.object_key=k.key
            LEFT JOIN tag_sets s ON k.tag_set=s.id JOIN tiers t ON t.level=f.target WHERE k.object=?")?
            .query_row([&obj.id],|r| {
                let target=r.get::<_,u8>(5)?;
                let effective=r.get::<_,Option<u8>>(6)?;
                let count=r.get::<_,u32>(12)?;
                let required=r.get::<_,u32>(13)?;
                let forecast=r.get::<_,u8>(2)?;
                let accesses=r.get::<_,u32>(3)?;
                Ok(json!({"importance":r.get::<_,u8>(0)?,"information":r.get::<_,u8>(1)?,
                    "predicted_hotness":forecast,"hotness":observed_hotness(forecast,accesses,r.get(4)?,now()),
                    "human_accesses":accesses,"target":target,"effective":effective,"requested":r.get::<_,Option<u8>>(7)?,
                    "locked":r.get::<_,bool>(8)?,"assessor":r.get::<_,String>(9)?,"assessment":r.get::<_,Option<String>>(10)?,
                    "revision":r.get::<_,i64>(11)?,"direct_tags":count,"minimum_tags":required,
                    "downgrade":(effective!=Some(target)).then(||format!("T{target} requires {required} distinct confirmed direct tags; this file has {count}."))}))
            }).optional()?;
        Ok(value.unwrap_or(Value::Null))
    }

    pub fn override_tier(
        &self,
        reference: &str,
        requested: Option<u8>,
        locked: bool,
    ) -> Result<Value> {
        let object = self.get(reference)?;
        self.editable(&object)?;
        ensure!(object.kind == "file", "tier overrides apply to files");
        let tx = Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)?;
        if let Some(requested) = requested {
            ensure!(
                policies(&tx)?.iter().any(|t| t.level == requested),
                "tier is inactive"
            );
        }
        let key = key(&tx, &object.id)?;
        tx.execute(
            "UPDATE file_tiers SET requested=?,locked=? WHERE object_key=?",
            params![requested, locked, key],
        )?;
        recompute(&tx, key, true)?;
        crate::event(
            &tx,
            Some(&object.id),
            "tier-override",
            Some(&self.actor),
            None,
            Some(&json!({"requested":requested,"locked":locked})),
            "ok",
        )?;
        tx.commit()?;
        self.file_tier_for(&object)
    }

    pub(crate) fn human_access(&self, object: &Object) -> Result<()> {
        if object.kind != "file" {
            return Ok(());
        }
        let tx = Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)?;
        let key = key(&tx, &object.id)?;
        tx.execute("UPDATE file_tiers SET human_accesses=min(human_accesses+1,2147483647),last_access=? WHERE object_key=?",params![now(),key])?;
        recompute(&tx, key, false)?;
        tx.commit()?;
        Ok(())
    }

    pub fn reassess_tier(&self, reference: &str) -> Result<Value> {
        let obj = self.get(reference)?;
        self.editable(&obj)?;
        ensure!(obj.kind == "file", "tier reassessment applies to files");
        let tx = Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)?;
        indexed(&tx, &obj.id).context("assessing current file metadata")?;
        tx.commit()?;
        self.file_tier_for(&obj)
    }
}

#[cfg(test)]
mod scale_test {
    use super::*;
    use crate::SearchOptions;
    use std::{fs, time::Instant};

    #[test]
    #[ignore = "creates a temporary million-row metadata Ledger without physical files"]
    fn million_record_metadata() -> Result<()> {
        let temporary = tempfile::Builder::new()
            .prefix("mosh-metadata-scale-")
            .tempdir()?;
        let path = temporary.path().join("scale.sqlite3");
        let baseline_started = Instant::now();
        let mut baseline = Connection::open(&path)?;
        packed_tags::register(&baseline)?;
        baseline.execute_batch(
            "PRAGMA foreign_keys=ON; PRAGMA synchronous=FULL; PRAGMA journal_mode=WAL;",
        )?;
        let tx = baseline.transaction()?;
        tx.execute_batch(include_str!("schema.sql"))?;
        packed_tags::migrate(&tx)?;
        tx.execute_batch(include_str!("ledger_v2.sql"))?;
        for start in (1..=1_000_000).step_by(100_000) {
            tx.execute("WITH RECURSIVE series(n) AS (SELECT ?1 UNION ALL SELECT n+1 FROM series WHERE n<?2)
                INSERT INTO objects(id,kind,path,name,extension,size,modified)
                SELECT printf('file-%07d',n),'file',printf('simulated/repo/file%07d.txt',n),
                printf('file%07d.txt',n),'txt',1000+n,n FROM series",
                params![start,start+99_999])?;
        }
        tx.execute(
            "INSERT INTO tag_dictionary(id,name) VALUES(1,'file'),(2,'rare')",
            [],
        )?;
        let file = 1;
        let mut writer = packed_tags::Writer::new(&tx);
        for n in 1..=1_000_000 {
            writer.assign(
                &format!("file-{n:07}"),
                file,
                packed_tags::FILESYSTEM,
                Some((1.0, None)),
            )?;
        }
        writer.assign(
            "file-1000000",
            2,
            packed_tags::FILESYSTEM,
            Some((1.0, None)),
        )?;
        writer.flush()?;
        tx.pragma_update(None, "user_version", 2)?;
        tx.commit()?;
        baseline.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        let baseline_time = baseline_started.elapsed();
        let baseline_bytes = fs::metadata(&path)?.len();
        let indexed = "direct-tag:file AND direct-tag:rare AND ext:txt";
        let mut query = baseline.prepare(
            "SELECT k.object FROM ledger JOIN object_keys k ON ledger.rowid=k.key
            WHERE ledger MATCH 'name:file1000000'",
        )?;
        let start = Instant::now();
        for _ in 0..30 {
            let id: String = query.query_row([], |r| r.get(0))?;
            assert_eq!(id, "file-1000000");
        }
        let v2_mean_ms = start.elapsed().as_secs_f64() * 1000.0 / 30.0;
        drop(query);
        drop(baseline);

        let migration = Instant::now();
        let ledger = Mosh::open(&path)?;
        ledger
            .db
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        let migration_time = migration.elapsed();
        let current_bytes = fs::metadata(&path)?.len();
        let current_id = ledger.get("file-1000000")?.id;
        let count: u64 = ledger.db.query_row(
            "SELECT count(*) FROM file_tiers WHERE effective=1",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(count, 1_000_000);
        let mut query = ledger.db.prepare(
            "SELECT k.object FROM ledger JOIN object_keys k ON ledger.rowid=k.key
            WHERE ledger MATCH 'name:file1000000'",
        )?;
        let start = Instant::now();
        for _ in 0..30 {
            let id: String = query.query_row([], |r| r.get(0))?;
            assert_eq!(id, current_id);
        }
        let current_mean_ms = start.elapsed().as_secs_f64() * 1000.0 / 30.0;
        let intersect = Instant::now();
        let result = ledger.search(
            indexed,
            &SearchOptions {
                limit: 10,
                ..Default::default()
            },
        )?;
        assert_eq!(result[0].id, current_id);
        let intersection_ms = intersect.elapsed().as_secs_f64() * 1000.0;
        println!(
            "million metadata rows: base {baseline_time:.2?}, migration {migration_time:.2?}, bytes {baseline_bytes} -> {current_bytes}; warm FTS (30) {v2_mean_ms:.4} -> {current_mean_ms:.4} ms; two-tag intersection {intersection_ms:.2} ms"
        );
        Ok(())
    }
}
