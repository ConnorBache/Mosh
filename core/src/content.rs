use crate::{Mosh, Object, canonical, fsmeta};
use anyhow::{Result, ensure};
use rusqlite::{Connection, Transaction, TransactionBehavior, functions::FunctionFlags, params};
use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

pub(crate) fn with_fts_sample<T>(
    scratch: &Mutex<Option<Connection>>,
    text: &str,
    read: impl FnOnce(&Connection) -> rusqlite::Result<T>,
) -> rusqlite::Result<T> {
    let mut scratch = scratch.lock().map_err(|_| rusqlite::Error::InvalidQuery)?;
    if scratch.is_none() {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("CREATE VIRTUAL TABLE sample USING fts5(body,content='',contentless_delete=1,tokenize='unicode61');
            CREATE VIRTUAL TABLE sample_tokens USING fts5vocab(sample,instance);")?;
        *scratch = Some(conn);
    }
    let conn = scratch.as_ref().unwrap();
    conn.prepare_cached("INSERT INTO sample(sample) VALUES('delete-all')")?
        .execute([])?;
    conn.prepare_cached("INSERT INTO sample(rowid,body) VALUES(1,?)")?
        .execute([text])?;
    read(conn)
}

fn extract(
    path: &str,
    extension: &str,
    native_id: Option<&str>,
    created: Option<i64>,
) -> Option<String> {
    if !fsmeta::text_supported(extension) {
        return None;
    }
    let path = Path::new(path);
    let metadata = fs::symlink_metadata(path).ok()?;
    if !metadata.is_file() || fsmeta::reparse(&metadata) || canonical(path).ok()?.as_path() != path
    {
        return None;
    }
    if let Some(id) = native_id {
        if fsmeta::native(path, &metadata).id.as_deref() != Some(id) {
            return None;
        }
    }
    if let Some(created) = created {
        if metadata.created().ok().map(crate::millis) != Some(created) {
            return None;
        }
    }
    fsmeta::text(path, extension).ok()
}

pub fn preview(object: &Object) -> String {
    if object.kind != "file" || object.reparse {
        return String::new();
    }
    object
        .path
        .as_deref()
        .zip(object.extension.as_deref())
        .and_then(|(path, extension)| {
            extract(path, extension, object.native_id.as_deref(), object.created)
        })
        .unwrap_or_default()
}

pub fn register(db: &Connection) -> Result<()> {
    db.create_scalar_function(
        "mosh_text_supported",
        1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| {
            Ok(ctx
                .get::<Option<String>>(0)?
                .is_some_and(|extension| fsmeta::text_supported(&extension)))
        },
    )?;
    // A private in-memory FTS document uses the same tokenizer and phrase semantics as the cache.
    let scratch = Mutex::new(None::<Connection>);
    db.create_scalar_function(
        "mosh_live_match",
        5,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DIRECTONLY,
        move |ctx| {
            let path = ctx.get::<String>(0)?;
            let extension = ctx.get::<String>(1)?;
            let native = fsmeta::unpack_id(ctx.get_raw(2))?;
            let created = ctx.get::<Option<i64>>(3)?;
            let term = ctx.get::<String>(4)?;
            let Some(text) = extract(&path, &extension, native.as_deref(), created) else {
                return Ok(false);
            };
            if text.is_empty() {
                return Ok(false);
            }
            with_fts_sample(&scratch, &text, |conn| {
                conn.prepare_cached("SELECT EXISTS(SELECT 1 FROM sample WHERE sample MATCH ?)")?
                    .query_row([term], |r| r.get(0))
            })
        },
    )?;
    Ok(())
}

fn allocated(db: &Connection) -> Result<i64> {
    Ok(db.query_row("SELECT coalesce(sum(pgsize),0) FROM dbstat WHERE name GLOB 'text_terms_*' OR name='text_cached'",
        [],|r|r.get(0))?)
}

fn reset(db: &Connection) -> Result<()> {
    db.execute_batch(
        "INSERT INTO text_terms(text_terms) VALUES('delete-all'); DELETE FROM text_cached;",
    )?;
    Ok(())
}

fn budget(db: &Connection) -> Result<i64> {
    Ok(db.query_row(
        "SELECT coalesce((SELECT cast(value AS INTEGER) FROM metadata WHERE key='text_budget'),0)",
        [],
        |r| r.get(0),
    )?)
}

fn flush_segments(db: &Connection) -> Result<()> {
    // FTS5's savepoint callback flushes pending terms and deletions to shadow pages.
    db.execute_batch("SAVEPOINT measure_text; RELEASE measure_text;")?;
    Ok(())
}

pub fn trim_after_invalidation(db: &Connection) -> Result<()> {
    let budget = budget(db)?;
    if budget > 0 {
        flush_segments(db)?;
        if allocated(db)? > budget {
            reset(db)?;
        }
    }
    Ok(())
}

impl Mosh {
    pub fn text_status(&self) -> Result<Value> {
        self.admin()?;
        let budget = budget(&self.db)?;
        let count: i64 = self
            .db
            .query_row("SELECT count(*) FROM text_cached", [], |r| r.get(0))?;
        Ok(
            json!({"budget_bytes":budget,"cached_files":count,"cache_bytes":if count==0 {0} else {allocated(&self.db)?}}),
        )
    }

    pub fn index_text(&self, budget: i64, cancel: &AtomicBool) -> Result<Value> {
        self.admin()?;
        ensure!(budget >= 0, "text budget must be nonnegative");
        let tx = Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)?;
        reset(&tx)?;
        tx.execute("INSERT INTO metadata VALUES('text_budget',?) ON CONFLICT DO UPDATE SET value=excluded.value",[budget])?;
        let mut batch_limit = 32;
        let mut after = 0;
        // Write at most one batch inside a savepoint before checking allocated FTS pages.
        while budget > 0 && batch_limit > 0 {
            ensure!(!cancel.load(Ordering::Relaxed), "text indexing cancelled");
            let mut statement=tx.prepare_cached("SELECT k.key,o.path,o.extension,o.native_id,o.created
                FROM object_keys k JOIN objects o ON o.id=k.object
                WHERE k.key>? AND o.missing=0 AND o.kind='file' AND o.reparse=0 AND mosh_text_supported(o.extension)
                ORDER BY k.key LIMIT ?2")?;
            let batch = statement
                .query_map(params![after, batch_limit], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        fsmeta::unpack_id(r.get_ref(3)?)?,
                        r.get::<_, Option<i64>>(4)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            if batch.is_empty() {
                break;
            }
            let last = batch.last().unwrap().0;
            tx.execute_batch("SAVEPOINT text_batch;")?;
            for (key, path, extension, native, created) in batch {
                ensure!(!cancel.load(Ordering::Relaxed), "text indexing cancelled");
                if let Some(text) = extract(&path, &extension, native.as_deref(), created) {
                    tx.prepare_cached("INSERT INTO text_terms(rowid,body) VALUES(?,?)")?
                        .execute(params![key, text])?;
                    tx.prepare_cached("INSERT INTO text_cached VALUES(?)")?
                        .execute([key])?;
                }
            }
            flush_segments(&tx)?;
            if allocated(&tx)? > budget {
                tx.execute_batch("ROLLBACK TO text_batch;")?;
                batch_limit /= 2;
            } else {
                after = last;
            }
            tx.execute_batch("RELEASE text_batch;")?;
        }
        tx.commit()?;
        let mut status = self.text_status()?;
        status["budget_reached"] = json!(batch_limit == 0);
        Ok(status)
    }
}
