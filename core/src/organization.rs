use crate::{Mosh, Object, event, object, uuid};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

pub(crate) const AUTHORITY: &str = "WITH RECURSIVE authority(person) AS (
    SELECT ?1 UNION ALL SELECT p.id FROM people p JOIN authority a ON p.superior=a.person
) ";

pub(crate) const VISIBLE: &str = "(?1='local' OR NOT EXISTS(
    SELECT 1 FROM objects s WHERE s.secret=1 AND s.missing=0
    AND (s.id=o.id OR (s.path IS NOT NULL AND o.path IS NOT NULL AND
        substr(o.path,1,length(rtrim(s.path,?2))+1)=rtrim(s.path,?2)||?2))
    AND NOT EXISTS(SELECT 1 FROM owners w JOIN authority a ON a.person=w.person WHERE w.object=s.id)
    AND NOT EXISTS(SELECT 1 FROM guests g WHERE g.object=s.id AND g.person=?1)
))";

pub(crate) fn tag_name(name: &str) -> Result<String> {
    let normalized = name.trim().to_lowercase();
    ensure!(
        !normalized.is_empty()
            && normalized.chars().count() <= 120
            && !normalized.chars().any(char::is_control),
        "tag names must contain between 1 and 120 printable characters"
    );
    Ok(normalized)
}

fn tag_id(db: &Connection, name: &str) -> Result<Option<i64>> {
    Ok(db
        .query_row(
            "SELECT id FROM tag_dictionary WHERE name=?1
        UNION ALL SELECT tag FROM tag_aliases WHERE alias=?1 LIMIT 1",
            [name],
            |r| r.get(0),
        )
        .optional()?)
}

pub(crate) fn assign(
    db: &Connection,
    object_id: &str,
    name: &str,
    origin: &str,
    confidence: f64,
    evidence: Option<&str>,
) -> Result<()> {
    ensure!(
        (0.0..=1.0).contains(&confidence),
        "confidence must be between zero and one"
    );
    let name = tag_name(name)?;
    let id = match tag_id(db, &name)? {
        Some(id) => id,
        None => {
            db.execute("INSERT INTO tag_dictionary(name) VALUES(?)", [name])?;
            db.last_insert_rowid()
        }
    };
    let existing = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM tag_assignments WHERE object=? AND tag=?)",
        params![object_id, id],
        |r| r.get::<_, bool>(0),
    )?;
    if !existing {
        let count: i64 = db.query_row(
            "SELECT count(DISTINCT tag) FROM tag_assignments WHERE object=?",
            [object_id],
            |r| r.get(0),
        )?;
        ensure!(count < 1000, "this object already has 1,000 direct tags");
    }
    db.execute("INSERT INTO tag_assignments(object,tag,origin,confidence,evidence) VALUES(?,?,?,?,?)
        ON CONFLICT(object,tag,origin) DO UPDATE SET confidence=excluded.confidence,evidence=excluded.evidence
        WHERE confidence<>excluded.confidence OR evidence IS NOT excluded.evidence",
        params![object_id,id,origin,confidence,evidence])?;
    Ok(())
}

impl Mosh {
    pub(crate) fn can_read(&self, selected: &Object) -> Result<bool> {
        if self.actor == "local" {
            return Ok(true);
        }
        let sql = format!("{AUTHORITY} SELECT {VISIBLE} FROM objects o WHERE id=?3");
        Ok(self.db.query_row(
            &sql,
            params![self.actor, std::path::MAIN_SEPARATOR_STR, selected.id],
            |row| row.get(0),
        )?)
    }

    pub(crate) fn owns(&self, id: &str) -> Result<bool> {
        if self.actor == "local" {
            return Ok(true);
        }
        let sql = format!(
            "{AUTHORITY} SELECT EXISTS(SELECT 1 FROM owners w JOIN authority a ON a.person=w.person WHERE w.object=?2)"
        );
        Ok(self
            .db
            .query_row(&sql, params![self.actor, id], |r| r.get(0))?)
    }

    pub(crate) fn editable(&self, item: &Object) -> Result<()> {
        let guest: bool = self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM guests WHERE object=? AND person=? AND permission='edit')",
            params![item.id, self.actor],
            |r| r.get(0),
        )?;
        if self.owns(&item.id)? || guest {
            return Ok(());
        }
        if let Some(parent) = &item.parent {
            let guest: bool = self.db.query_row("SELECT EXISTS(SELECT 1 FROM guests WHERE object=? AND person=? AND permission='edit')",
                params![parent,self.actor],|r|r.get(0))?;
            if self.owns(parent)? || guest {
                return Ok(());
            }
        }
        anyhow::bail!("edit permission required")
    }

    pub fn person(&self, id: &str, superior: Option<&str>) -> Result<()> {
        self.admin()?;
        ensure!(
            !id.is_empty() && id.len() <= 100 && id != "local",
            "invalid person identifier"
        );
        let cycle: bool = self.db.query_row("WITH RECURSIVE managers(id) AS (
            SELECT ?1 UNION SELECT p.superior FROM people p JOIN managers m ON p.id=m.id WHERE p.superior IS NOT NULL
        ) SELECT EXISTS(SELECT 1 FROM managers WHERE id=?2)",params![superior,id],|r|r.get(0))?;
        ensure!(!cycle, "this would create a hierarchy cycle");
        self.db.execute("INSERT INTO people VALUES(?,?) ON CONFLICT(id) DO UPDATE SET superior=excluded.superior",params![id,superior])?;
        Ok(())
    }

    pub fn access(&self, reference: &str, person: &str, permission: &str) -> Result<()> {
        let item = self.get(reference)?;
        ensure!(
            item.kind != "file" && self.owns(&item.id)?,
            "Coll owner authority required"
        );
        let transaction = self.db.unchecked_transaction()?;
        match permission {
            "owner" => {
                transaction.execute(
                    "INSERT OR IGNORE INTO owners VALUES(?,?)",
                    params![item.id, person],
                )?;
            }
            "read" | "edit" => {
                transaction.execute("INSERT INTO guests VALUES(?,?,?) ON CONFLICT DO UPDATE SET permission=excluded.permission",
                params![item.id,person,permission])?;
            }
            "revoke" => {
                transaction.execute(
                    "DELETE FROM guests WHERE object=? AND person=?",
                    params![item.id, person],
                )?;
                transaction.execute(
                    "DELETE FROM owners WHERE object=?1 AND person=?2
                    AND EXISTS(SELECT 1 FROM owners WHERE object=?1 AND person<>?2)",
                    params![item.id, person],
                )?;
            }
            _ => anyhow::bail!("permission must be owner, read, edit, or revoke"),
        }
        event(
            &transaction,
            Some(&item.id),
            "access",
            Some(&self.actor),
            None,
            Some(&json!({"person":person,"permission":permission})),
            "ok",
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn secret(&self, reference: &str, enabled: bool) -> Result<()> {
        let item = self.get(reference)?;
        ensure!(
            item.kind != "file" && self.owns(&item.id)?,
            "Coll owner authority required"
        );
        self.db.execute(
            "UPDATE objects SET secret=? WHERE id=?",
            params![enabled, item.id],
        )?;
        Ok(())
    }

    pub fn tags(&self, reference: &str) -> Result<Vec<Value>> {
        let selected = self.get(reference)?;
        let sql = format!("{AUTHORITY}, ancestors(id,depth) AS (
            SELECT ?3,0 UNION ALL SELECT o.parent,depth+1 FROM ancestors a JOIN objects o ON o.id=a.id WHERE o.parent IS NOT NULL
        )
        SELECT t.name,a.origin,a.confidence,a.evidence,a.object,ancestors.depth
        FROM ancestors JOIN objects o ON o.id=ancestors.id JOIN tag_assignments a ON a.object=o.id
        JOIN tag_dictionary t ON t.id=a.tag
        WHERE (ancestors.depth=0 OR a.origin<>'suggested') AND {VISIBLE}
        ORDER BY ancestors.depth,t.name,a.origin");
        let mut query = self.db.prepare(&sql)?;
        let rows = query.query_map(
            params![self.actor, std::path::MAIN_SEPARATOR_STR, selected.id],
            |r| {
                Ok(
                    json!({"name":r.get::<_,String>(0)?,"origin":r.get::<_,String>(1)?,
                "confidence":r.get::<_,f64>(2)?,"evidence":r.get::<_,Option<String>>(3)?,
                "object":r.get::<_,String>(4)?,"inherited":r.get::<_,i64>(5)?>0}),
                )
            },
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn tag(&self, reference: &str, name: &str, add: bool, confidence: f64) -> Result<()> {
        let selected = self.get(reference)?;
        self.editable(&selected)?;
        let tx = self.db.unchecked_transaction()?;
        let old = tx.query_row("SELECT confidence,evidence FROM tag_assignments WHERE object=?1 AND origin='user' AND tag=(
            SELECT id FROM tag_dictionary WHERE name=?2 UNION ALL SELECT tag FROM tag_aliases WHERE alias=?2 LIMIT 1)",
            params![selected.id,tag_name(name)?],|r|Ok(json!({"confidence":r.get::<_,f64>(0)?,"evidence":r.get::<_,Option<String>>(1)?}))).optional()?;
        if add {
            assign(&tx, &selected.id, name, "user", confidence, None)?;
        } else if let Some(id) = tag_id(&tx, &tag_name(name)?)? {
            tx.execute(
                "DELETE FROM tag_assignments WHERE object=? AND tag=? AND origin='user'",
                params![selected.id, id],
            )?;
        }
        event(
            &tx,
            Some(&selected.id),
            if add { "tag-add" } else { "tag-remove" },
            Some(&self.actor),
            old.as_ref(),
            Some(&json!({"tag":name,"confidence":confidence})),
            "ok",
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn tag_catalog(&self, limit: usize) -> Result<Vec<Value>> {
        let sql = format!(
            "{AUTHORITY} SELECT t.name,count(DISTINCT a.object) FROM tag_assignments a
            JOIN objects o ON o.id=a.object JOIN tag_dictionary t ON t.id=a.tag
            WHERE o.missing=0 AND a.origin<>'suggested' AND {VISIBLE}
            GROUP BY t.id ORDER BY 2 DESC,t.name LIMIT ?3"
        );
        let mut query = self.db.prepare(&sql)?;
        let rows = query.query_map(
            params![
                self.actor,
                std::path::MAIN_SEPARATOR_STR,
                limit.clamp(1, 1000)
            ],
            |r| Ok(json!({"name":r.get::<_,String>(0)?,"count":r.get::<_,i64>(1)?})),
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn tag_coverage(&self) -> Result<Value> {
        let sql = format!("{AUTHORITY} SELECT count(*),coalesce(sum(n>=10),0),coalesce(sum(n),0)
            FROM (SELECT (SELECT count(DISTINCT tag) FROM tag_assignments a WHERE a.object=o.id AND origin<>'suggested') n
            FROM objects o WHERE o.missing=0 AND {VISIBLE})");
        Ok(self.db.query_row(&sql,params![self.actor,std::path::MAIN_SEPARATOR_STR],|r| {
            let total = r.get::<_,i64>(0)?;
            let covered = r.get::<_,i64>(1)?;
            Ok(json!({"objects":total,"covered":covered,"incomplete":total-covered,"direct_assignments":r.get::<_,i64>(2)?}))
        })?)
    }

    pub fn tag_dictionary(&self, action: &str, from: &str, to: Option<&str>) -> Result<()> {
        self.admin()?;
        let from = tag_name(from)?;
        let to = to.map(tag_name).transpose()?;
        let tx = self.db.unchecked_transaction()?;
        let id = tag_id(&tx, &from)?.context("unknown tag")?;
        match action {
            "delete" => {
                tx.execute("DELETE FROM tag_dictionary WHERE id=?", [id])?;
            }
            "alias" | "rename" => {
                let to = to.context("new name required")?;
                ensure!(
                    tag_id(&tx, &to)?.is_none(),
                    "the new name is already in use"
                );
                if action == "alias" {
                    tx.execute("INSERT INTO tag_aliases VALUES(?,?)", params![to, id])?;
                } else {
                    let old: String =
                        tx.query_row("SELECT name FROM tag_dictionary WHERE id=?", [id], |r| {
                            r.get(0)
                        })?;
                    tx.execute(
                        "UPDATE tag_dictionary SET name=? WHERE id=?",
                        params![to, id],
                    )?;
                    tx.execute("INSERT INTO tag_aliases VALUES(?,?)", params![old, id])?;
                }
            }
            "merge" => {
                let target = tag_id(&tx, &to.context("destination tag required")?)?
                    .context("unknown destination tag")?;
                ensure!(id != target, "choose two distinct tags");
                tx.execute("INSERT INTO tag_assignments SELECT object,?2,origin,confidence,evidence FROM tag_assignments WHERE tag=?1
                    ON CONFLICT DO UPDATE SET confidence=excluded.confidence,evidence=excluded.evidence WHERE excluded.confidence>confidence",params![id,target])?;
                tx.execute(
                    "UPDATE tag_aliases SET tag=?2 WHERE tag=?1",
                    params![id, target],
                )?;
                tx.execute(
                    "INSERT INTO tag_aliases SELECT name,?2 FROM tag_dictionary WHERE id=?1",
                    params![id, target],
                )?;
                tx.execute("DELETE FROM tag_dictionary WHERE id=?", [id])?;
            }
            _ => anyhow::bail!("use delete, alias, rename, or merge"),
        }
        event(
            &tx,
            None,
            &format!("tag-{action}"),
            Some(&self.actor),
            Some(&json!({"name":from,"tag":id})),
            None,
            "ok",
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn rule(&self, query: &str, tag: &str) -> Result<()> {
        self.admin()?;
        crate::query::validate(query)?;
        self.db.execute(
            "INSERT OR IGNORE INTO rules VALUES(?,?)",
            params![tag_name(tag)?, query],
        )?;
        Ok(())
    }

    pub fn paint(&self, reference: &str) -> Result<Option<Value>> {
        let selected = self.get(reference)?;
        Ok(self.db.query_row("SELECT level,body,revision,locked,origin FROM paint WHERE object=?",[selected.id],|r| {
            let revision = r.get::<_,i64>(2)?;
            Ok(json!({"level":r.get::<_,String>(0)?,"body":r.get::<_,String>(1)?,"revision":revision,
                "stale":revision!=selected.revision,"locked":r.get::<_,bool>(3)?,"origin":r.get::<_,String>(4)?}))
        }).optional()?)
    }

    pub fn set_paint(&self, reference: &str, level: &str, body: &str, locked: bool) -> Result<()> {
        let selected = self.get(reference)?;
        self.editable(&selected)?;
        ensure!(
            body.len() <= 32768 && matches!(level, "none" | "minimal" | "standard" | "detailed"),
            "invalid Paint level or body length"
        );
        let before = self.paint(&selected.id)?;
        let tx = self.db.unchecked_transaction()?;
        if level == "none" {
            tx.execute("DELETE FROM paint WHERE object=?", [&selected.id])?;
        } else {
            tx.execute("INSERT INTO paint VALUES(?,?,?,?,?, 'user') ON CONFLICT(object) DO UPDATE SET
                level=excluded.level,body=excluded.body,revision=excluded.revision,locked=excluded.locked,origin='user'",
                params![selected.id,level,body,selected.revision,locked])?;
        }
        event(
            &tx,
            Some(&selected.id),
            "paint",
            Some(&self.actor),
            before.as_ref(),
            Some(&json!({"level":level,"body":body,"locked":locked})),
            "ok",
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn collection(&self, name: &str, marriage: bool) -> Result<String> {
        ensure!(
            !name.trim().is_empty() && name.len() <= 240,
            "a collection name is required"
        );
        let tx = self.db.unchecked_transaction()?;
        let id = uuid();
        tx.execute(
            "INSERT INTO objects(id,kind,name) VALUES(?,?,?)",
            params![id, if marriage { "marriage" } else { "coll" }, name.trim()],
        )?;
        tx.execute("INSERT INTO owners VALUES(?,?)", params![id, self.actor])?;
        event(
            &tx,
            Some(&id),
            "collection-create",
            Some(&self.actor),
            None,
            Some(&json!({"name":name})),
            "ok",
        )?;
        tx.commit()?;
        Ok(id)
    }

    pub fn connect(
        &self,
        container: &str,
        target: &str,
        kind: &str,
        selector: Option<&str>,
    ) -> Result<()> {
        let container = self.get(container)?;
        let target = self.get(target)?;
        self.editable(&container)?;
        ensure!(
            matches!(kind, "parent" | "reference" | "related")
                && selector.unwrap_or("").len() <= 1024,
            "invalid relationship"
        );
        ensure!(
            kind == "related" || container.kind != "file",
            "a file cannot contain virtual entries"
        );
        ensure!(
            kind != "reference" || container.kind == "marriage",
            "references belong to Marriages"
        );
        let tx = rusqlite::Transaction::new_unchecked(
            &self.db,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let cycle: bool = tx.query_row("WITH RECURSIVE reachable(id) AS (
            SELECT ?1 UNION SELECT e.target FROM edges e JOIN reachable r ON e.container=r.id WHERE e.kind<>'related'
            UNION SELECT o.id FROM objects o JOIN reachable r ON o.parent=r.id WHERE o.missing=0
        ) SELECT EXISTS(SELECT 1 FROM reachable WHERE id=?2)",params![target.id,container.id],|r|r.get(0))?;
        ensure!(
            kind == "related" || !cycle,
            "this relationship would create a cycle"
        );
        tx.execute(
            "INSERT OR IGNORE INTO edges VALUES(?,?,?,?)",
            params![container.id, target.id, kind, selector.unwrap_or("")],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn collection_query(&self, container: &str, query: &str) -> Result<()> {
        let selected = self.get(container)?;
        self.editable(&selected)?;
        ensure!(
            selected.kind == "marriage",
            "saved references belong to Marriages"
        );
        crate::query::validate(query)?;
        self.db.execute(
            "INSERT OR IGNORE INTO collection_queries VALUES(?,?)",
            params![selected.id, query],
        )?;
        Ok(())
    }

    pub fn graph(&self, reference: Option<&str>) -> Result<Vec<Value>> {
        let id = reference
            .map(|reference| self.get(reference).map(|object| object.id))
            .transpose()?;
        let sql = format!(
            "{AUTHORITY} SELECT o.*,e.kind AS edge_kind,e.selector,e.container AS source
            FROM edges e JOIN objects o ON o.id=e.target
            WHERE o.missing=0 AND (?3 IS NULL OR e.container=?3 OR e.target=?3) AND {VISIBLE}
            ORDER BY e.container,e.target LIMIT 1000"
        );
        let mut query = self.db.prepare(&sql)?;
        let records = query.query_map(
            params![self.actor, std::path::MAIN_SEPARATOR_STR, id],
            |r| {
                Ok((
                    object(r)?,
                    r.get::<_, String>("source")?,
                    r.get::<_, String>("edge_kind")?,
                    r.get::<_, String>("selector")?,
                ))
            },
        )?;
        let mut result = Vec::new();
        for record in records {
            let (target, source, kind, selector) = record?;
            if let Ok(source) = self.get(&source) {
                result
                    .push(json!({"source":source,"target":target,"kind":kind,"selector":selector}));
            }
        }
        Ok(result)
    }

    pub fn saved_search(&self, name: &str, query: &str) -> Result<()> {
        crate::query::validate(query)?;
        ensure!(
            !name.trim().is_empty() && name.len() <= 240,
            "invalid saved search name"
        );
        self.db.execute("INSERT INTO saved_searches VALUES(?,?,?) ON CONFLICT DO UPDATE SET query=excluded.query",params![self.actor,name.trim(),query])?;
        Ok(())
    }

    pub fn saved_searches(&self) -> Result<Vec<Value>> {
        let mut query = self
            .db
            .prepare("SELECT name,query FROM saved_searches WHERE actor=? ORDER BY name")?;
        let rows = query.query_map([&self.actor], |r| {
            Ok(json!({"name":r.get::<_,String>(0)?,"query":r.get::<_,String>(1)?}))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}
