use crate::{Mosh, SearchOptions, canonical, event, path_text};
use anyhow::{Context, Result, ensure};
use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior,
    functions::FunctionFlags, params,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

pub(crate) fn within(path: &Path, root: &Path) -> bool {
    #[cfg(windows)]
    {
        let fold = |p: &Path| p.to_string_lossy().replace('/', "\\").to_lowercase();
        Path::new(&fold(path)).starts_with(fold(root))
    }
    #[cfg(not(windows))]
    path.starts_with(root)
}

pub(crate) fn register(db: &Connection) -> Result<()> {
    db.create_scalar_function(
        "mosh_within_path",
        2,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| {
            let path: Option<String> = ctx.get(0)?;
            let root: String = ctx.get(1)?;
            Ok(path.is_some_and(|path| within(Path::new(&path), Path::new(&root))))
        },
    )?;
    Ok(())
}

pub(crate) fn boundaries(db: &Connection) -> Result<Vec<PathBuf>> {
    Ok(db
        .prepare_cached("SELECT root FROM nested_moshes")?
        .query_map([], |r| r.get::<_, String>(0).map(PathBuf::from))?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

pub(crate) fn boundary_ids(db: &Connection) -> Result<Vec<(String, Option<i64>)>> {
    Ok(db
        .prepare_cached(
            "SELECT native_id,native_created FROM nested_moshes WHERE native_id IS NOT NULL",
        )?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?)
}

pub(crate) fn moved_boundary(
    path: &Path,
    meta: &fs::Metadata,
    ids: &[(String, Option<i64>)],
) -> bool {
    if ids.is_empty() || !meta.is_dir() || crate::fsmeta::reparse(meta) {
        return false;
    }
    let native = crate::fsmeta::native(path, meta).id;
    let created = meta.created().ok().map(crate::millis);
    ids.iter()
        .any(|(id, birth)| native.as_ref() == Some(id) && *birth == created)
}

pub(crate) fn id(db: &Connection) -> Result<String> {
    db.query_row("SELECT value FROM metadata WHERE key='mosh_id'", [], |r| {
        r.get(0)
    })
    .context("attachment is unavailable or has not been initialized as a Mosh")
}

fn read_attachment(path: &Path) -> Result<Connection> {
    ensure!(
        fs::symlink_metadata(path).is_ok_and(|m| m.is_file() && !crate::fsmeta::reparse(&m))
            && canonical(path)? == path,
        "attachment is unavailable"
    );
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    db.busy_timeout(Duration::from_secs(3))?;
    Ok(db)
}

fn check_cycle(path: &Path, first: &str, parent: &str) -> Result<()> {
    let mut visited = BTreeSet::new();
    let mut work = vec![(path.to_owned(), first.to_owned())];
    while let Some((path, expected)) = work.pop() {
        ensure!(expected != parent, "a nested Mosh would create a cycle");
        if !visited.insert(expected.clone()) {
            continue;
        }
        ensure!(
            visited.len() <= 128,
            "too many attachments to verify safely"
        );
        let db = read_attachment(&path)?;
        ensure!(id(&db)? == expected, "attachment identity changed");
        let mut statement = db.prepare("SELECT attachment,child FROM nested_moshes")?;
        for entry in statement.query_map([], |r| {
            Ok((
                PathBuf::from(r.get::<_, String>(0)?),
                r.get::<_, String>(1)?,
            ))
        })? {
            work.push(entry?);
        }
    }
    Ok(())
}

impl Mosh {
    pub fn mosh_id(&self) -> Result<String> {
        id(&self.db)
    }

    pub(crate) fn outside_children(&self, path: &Path) -> Result<()> {
        ensure!(
            !boundaries(&self.db)?.iter().any(|root| within(path, root)),
            "path is outside this Mosh"
        );
        let ids = boundary_ids(&self.db)?;
        if !ids.is_empty() {
            ensure!(
                !path
                    .ancestors()
                    .any(|ancestor| fs::symlink_metadata(ancestor)
                        .is_ok_and(|meta| moved_boundary(ancestor, &meta, &ids))),
                "path is outside this Mosh"
            );
        }
        Ok(())
    }

    pub fn register_child(
        &self,
        attachment: &Path,
        root: &Path,
        name: &str,
        paint: &str,
        tags: &[String],
    ) -> Result<Value> {
        self.admin()?;
        ensure!(
            !name.trim().is_empty() && name.len() <= 128 && paint.len() <= 2048 && tags.len() <= 32,
            "invalid routing metadata"
        );
        let tags = tags
            .iter()
            .map(|tag| crate::organization::tag_name(tag))
            .collect::<Result<BTreeSet<_>>>()?;
        let attachment = canonical(attachment)?;
        let root = canonical(root)?;
        ensure!(
            fs::symlink_metadata(&root)
                .is_ok_and(|meta| meta.is_dir() && !crate::fsmeta::reparse(&meta)),
            "choose an ordinary directory"
        );
        let child_db = read_attachment(&attachment)?;
        let child = id(&child_db)?;
        let parent = self.mosh_id()?;
        check_cycle(&attachment, &child, &parent)?;
        let child_roots = child_db
            .prepare("SELECT path FROM roots")?
            .query_map([], |r| r.get::<_, String>(0).map(PathBuf::from))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ensure!(
            child_roots.iter().any(|candidate| *candidate == root)
                && child_roots
                    .iter()
                    .filter(|candidate| candidate.is_dir())
                    .all(|candidate| within(candidate, &root)),
            "child roots must lie inside its declared boundary"
        );
        let tx = Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)?;
        let roots = tx
            .prepare("SELECT path FROM roots")?
            .query_map([], |r| r.get::<_, String>(0).map(PathBuf::from))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ensure!(
            !roots.iter().any(|parent| within(parent, &root)),
            "child boundary contains a registered parent root"
        );
        let siblings = tx
            .prepare("SELECT root FROM nested_moshes WHERE child<>?")?
            .query_map([&child], |r| r.get::<_, String>(0).map(PathBuf::from))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ensure!(
            !siblings
                .iter()
                .any(|path| within(path, &root) || within(&root, path)),
            "child boundaries overlap"
        );
        ensure!(
            !within(&self.db_path, &root),
            "parent Ledger lies inside the child boundary"
        );
        tx.execute(
            "UPDATE nested_suspended SET child=NULL WHERE child=?1
            AND EXISTS(SELECT 1 FROM nested_moshes WHERE child=?1 AND root<>?2)",
            params![child, path_text(&root)?],
        )?;
        let meta = fs::symlink_metadata(&root)?;
        tx.execute("INSERT INTO nested_moshes VALUES(?,?,?,?,?,?,?,?) ON CONFLICT(child) DO UPDATE SET
            name=excluded.name,paint=excluded.paint,tags=excluded.tags,attachment=excluded.attachment,root=excluded.root,
            native_id=excluded.native_id,native_created=excluded.native_created",
            params![child,name.trim(),paint,serde_json::to_string(&tags)?,path_text(&attachment)?,path_text(&root)?,
                crate::fsmeta::native(&root,&meta).id,meta.created().ok().map(crate::millis)])?;
        tx.execute("INSERT OR IGNORE INTO nested_suspended SELECT ?1,k.key FROM objects o JOIN object_keys k ON k.object=o.id
            WHERE o.missing=0 AND mosh_within_path(o.path,?2)",params![child,path_text(&root)?])?;
        tx.execute(
            "UPDATE objects SET missing=1 WHERE missing=0 AND mosh_within_path(path,?)",
            [path_text(&root)?],
        )?;
        crate::metadata_search::rebuild_directories(&tx)?;
        event(
            &tx,
            None,
            "nested-attach",
            Some(&self.actor),
            None,
            Some(&json!({"child":child,"root":root})),
            "ok",
        )?;
        tx.commit()?;
        Ok(json!({"mosh":parent,"child":child}))
    }

    pub fn child_discovery(&self, child: &str, actor: &str, enabled: bool) -> Result<Value> {
        self.admin()?;
        if enabled {
            self.db.execute(
                "INSERT OR IGNORE INTO nested_discovery VALUES(?,?)",
                params![child, actor],
            )?;
        } else {
            self.db.execute(
                "DELETE FROM nested_discovery WHERE child=? AND actor=?",
                params![child, actor],
            )?;
        }
        Ok(json!({"child":child,"actor":actor,"discover":enabled}))
    }

    pub fn grant_handoff(&self, parent: &str, actor: &str, enabled: bool) -> Result<Value> {
        self.admin()?;
        ensure!(
            uuid::Uuid::parse_str(parent).is_ok() && parent != self.mosh_id()?,
            "choose the parent Mosh ID"
        );
        if enabled {
            self.db.execute(
                "INSERT OR IGNORE INTO handoff_grants VALUES(?,?)",
                params![parent, actor],
            )?;
        } else {
            self.db.execute(
                "DELETE FROM handoff_grants WHERE parent=? AND actor=?",
                params![parent, actor],
            )?;
        }
        Ok(json!({"parent":parent,"actor":actor,"search":enabled}))
    }

    pub fn detach_child(&self, child: &str) -> Result<Value> {
        self.admin()?;
        let tx = Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)?;
        ensure!(
            tx.execute("DELETE FROM nested_moshes WHERE child=?", [child])? == 1,
            "attachment is unavailable"
        );
        crate::metadata_search::rebuild_directories(&tx)?;
        event(
            &tx,
            None,
            "nested-detach",
            Some(&self.actor),
            None,
            Some(&json!({"child":child})),
            "ok",
        )?;
        tx.commit()?;
        Ok(json!({"detached":child}))
    }

    pub fn child_pointers(&self, query: &str, selected: Option<&str>) -> Result<Vec<Value>> {
        let mut statement=self.db.prepare("SELECT child,name,paint,tags,attachment,root FROM nested_moshes n
            WHERE (?2 IS NULL OR n.child=?2) AND (?1='local' OR EXISTS(SELECT 1 FROM nested_discovery g
                  WHERE g.child=n.child AND g.actor=?1)) ORDER BY name,child")?;
        let words = query
            .split(|ch: char| !ch.is_alphanumeric())
            .filter(|w| w.len() >= 3)
            .map(str::to_lowercase)
            .collect::<Vec<_>>();
        let entries = statement
            .query_map(params![self.actor, selected], |r| {
                let tag_filter: String = r.get(3)?;
                let child: String = r.get(0)?;
                let name: String = r.get(1)?;
                let paint: String = r.get(2)?;
                let tags: Value = serde_json::from_str(&tag_filter).unwrap_or_default();
                let visible = format!("{name} {paint} {tag_filter}").to_lowercase();
                let relevance = words
                    .iter()
                    .filter(|word| visible.contains(word.as_str()))
                    .count();
                let mut row = json!({"child":child,"name":name,"paint":paint,"tags":tags});
                if self.actor == "local" {
                    row["attachment"] = json!(r.get::<_, String>(4)?);
                    row["root"] = json!(r.get::<_, String>(5)?);
                }
                Ok((relevance, row))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut entries = entries
            .into_iter()
            .filter(|(score, _)| query.trim().is_empty() || *score > 0)
            .collect::<Vec<_>>();
        entries.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
        Ok(entries
            .into_iter()
            .take(if query.trim().is_empty() {
                usize::MAX
            } else {
                10
            })
            .map(|(_, row)| row)
            .collect())
    }

    pub fn child_handoff(&self, child: &str, query: &str, natural: bool) -> Result<Value> {
        let path=self.db.query_row("SELECT attachment FROM nested_moshes n WHERE child=?2
            AND (?1='local' OR EXISTS(SELECT 1 FROM nested_discovery g WHERE g.child=n.child AND g.actor=?1))",
            params![self.actor,child],|r|r.get::<_,String>(0)).optional()?.context("attachment is unavailable")?;
        let path = Path::new(&path);
        let authorized = || -> Result<bool> {
            let db = read_attachment(path)?;
            Ok(id(&db)? == child
                && db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM handoff_grants WHERE parent=? AND actor=?)",
                    params![self.mosh_id()?, self.actor],
                    |r| r.get(0),
                )?)
        };
        ensure!(
            authorized().unwrap_or(false),
            "attachment search is unavailable"
        );
        let mosh = Mosh::as_actor(path, &self.actor).context("attachment search is unavailable")?;
        ensure!(
            mosh.mosh_id()? == child && authorized().unwrap_or(false),
            "attachment search is unavailable"
        );
        let mut result = if natural {
            mosh.ask(query, false)?
        } else {
            json!({"results":mosh.search(query,&SearchOptions::default())?.into_iter().map(|object|json!({"object":object})).collect::<Vec<_>>()})
        };
        result["mosh"] = json!(child);
        if let Some(rows) = result["results"].as_array_mut() {
            for row in rows {
                if let Some(id) = row["object"]["id"].as_str() {
                    row["qualified_id"] = json!(format!("{child}:{id}"));
                }
            }
        }
        Ok(result)
    }
}
