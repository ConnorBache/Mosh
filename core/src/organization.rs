use crate::{Mosh, Object, event, object, packed_tags};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

pub(crate) const AUTHORITY: &str = "WITH RECURSIVE authority(person) AS (
    SELECT ?1 UNION ALL SELECT p.id FROM people p JOIN authority a ON p.superior=a.person
) ";

pub(crate) const VISIBLE: &str = "((NOT EXISTS(SELECT 1 FROM nested_moshes) OR
    NOT EXISTS(SELECT 1 FROM nested_moshes boundary
        WHERE o.path IS NOT NULL AND mosh_within_path(o.path,boundary.root))) AND (?1='local' OR NOT EXISTS(
    SELECT 1 FROM objects s WHERE s.secret=1 AND s.missing=0
    AND (s.key=o.key OR (s.path IS NOT NULL AND o.path IS NOT NULL AND
        substr(o.path,1,length(rtrim(s.path,?2))+1)=rtrim(s.path,?2)||?2))
    AND NOT EXISTS(SELECT 1 FROM owners w JOIN authority a ON a.person=w.person WHERE w.object=s.key)
    AND NOT EXISTS(SELECT 1 FROM guests g WHERE g.object=s.key AND g.person=?1)
    AND NOT EXISTS(SELECT 1 FROM guest_rights g WHERE g.object=s.key AND g.person=?1 AND g.right='discover')
)))";

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

pub(crate) fn tag_id(db: &Connection, name: &str) -> Result<Option<i64>> {
    Ok(db
        .query_row(
            "SELECT id FROM tag_dictionary WHERE name=?1
        UNION ALL SELECT tag FROM tag_aliases WHERE alias=?1 LIMIT 1",
            [name],
            |r| r.get(0),
        )
        .optional()?)
}

pub(crate) fn dictionary_id(db: &Connection, name: &str) -> Result<i64> {
    let name = tag_name(name)?;
    Ok(match tag_id(db, &name)? {
        Some(id) => id,
        None => {
            db.execute("INSERT INTO tag_dictionary(id,name) SELECT coalesce(max(id),0)+1,? FROM tag_registry", [name])?;
            db.last_insert_rowid()
        }
    })
}

impl Mosh {
    pub(crate) fn can_read(&self, selected: &Object) -> Result<bool> {
        let sql = format!("{AUTHORITY} SELECT {VISIBLE} FROM objects o WHERE key=?3");
        Ok(self.db.query_row(
            &sql,
            params![self.actor, std::path::MAIN_SEPARATOR_STR, selected.key],
            |row| row.get(0),
        )?)
    }

    pub(crate) fn owns(&self, id: &str) -> Result<bool> {
        if self.actor == "local" {
            return Ok(true);
        }
        let sql = format!(
            "{AUTHORITY} SELECT EXISTS(SELECT 1 FROM owners w JOIN authority a ON a.person=w.person WHERE w.object=mosh_key(?2))"
        );
        Ok(self
            .db
            .query_row(&sql, params![self.actor, id], |r| r.get(0))?)
    }

    pub(crate) fn editable(&self, item: &Object) -> Result<()> {
        ensure!(self.permits(item, "edit")?, "edit permission required");
        let guest: bool = self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM guests WHERE object=? AND person=? AND permission='edit')",
            params![item.key, self.actor],
            |r| r.get(0),
        )?;
        if self.owns(&item.id)? || guest || self.explicit_right(item, "edit")? {
            return Ok(());
        }
        if let Some(parent) = &item.parent {
            let guest: bool = self.db.query_row("SELECT EXISTS(SELECT 1 FROM guests WHERE object=? AND person=? AND permission='edit')",
                params![parent,self.actor],|r|r.get(0))?;
            if self.owns(parent)? || guest {
                return Ok(());
            }
            if self.db.query_row("SELECT EXISTS(SELECT 1 FROM guest_rights WHERE object=? AND person=? AND right='edit')",
                params![parent,self.actor],|r|r.get::<_,bool>(0))? {return Ok(());}
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
        let item = self.find(reference)?;
        ensure!(
            item.kind != "file" && self.can_manage(&item)?,
            "Coll owner authority required"
        );
        let transaction = self.db.unchecked_transaction()?;
        match permission {
            "owner" => {
                transaction.execute(
                    "INSERT OR IGNORE INTO owners VALUES(?,?)",
                    params![item.key, person],
                )?;
            }
            "read" | "edit" => {
                transaction.execute("INSERT INTO guests VALUES(?,?,?) ON CONFLICT DO UPDATE SET permission=excluded.permission",
                params![item.key,person,permission])?;
            }
            "revoke" => {
                transaction.execute(
                    "DELETE FROM guests WHERE object=? AND person=?",
                    params![item.key, person],
                )?;
                transaction.execute(
                    "DELETE FROM owners WHERE object=?1 AND person=?2
                    AND EXISTS(SELECT 1 FROM owners WHERE object=?1 AND person<>?2)",
                    params![item.key, person],
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
        let item = self.find(reference)?;
        ensure!(
            item.kind != "file" && self.owns(&item.id)?,
            "Coll owner authority required"
        );
        self.db.execute(
            "UPDATE objects SET secret=? WHERE key=?",
            params![enabled, item.key],
        )?;
        Ok(())
    }

    pub fn tags(&self, reference: &str) -> Result<Vec<Value>> {
        let selected = self.find(reference)?;
        if !self.permits(&selected, "paint")? {
            return Ok(vec![]);
        }
        let readable = crate::security::allowed("paint");
        let sql = format!("{AUTHORITY}, ancestors(id,depth) AS (
            SELECT ?3,0 UNION ALL SELECT o.parent_key,depth+1 FROM ancestors a JOIN objects o ON o.key=a.id WHERE o.parent_key IS NOT NULL
        ), origins(bit,name) AS (VALUES(1,'user'),(2,'automatic'),(4,'filesystem'),(8,'git'),(16,'suggested'),(32,'ai'),(64,'contextual'))
        SELECT t.name,g.name,coalesce(d.confidence,1),CASE WHEN d.origin IS NOT NULL THEN d.evidence
            WHEN g.bit=4 THEN 'file metadata' ELSE NULL END,o.id,ancestors.depth
        FROM ancestors JOIN objects o ON o.key=ancestors.id
        JOIN tag_sets s ON s.id=o.tag_set JOIN json_each(mosh_set_tags(s.packed)) a
        JOIN origins g ON (a.value->>1) & g.bit <> 0 JOIN tag_dictionary t ON t.id=(a.value->>0)
        LEFT JOIN tag_details d ON d.object_key=o.key AND d.tag=t.id AND d.origin=g.bit
        WHERE (ancestors.depth=0 OR g.bit<>16) AND {VISIBLE} AND {readable}
        ORDER BY ancestors.depth,t.name,g.name");
        let mut query = self.db.prepare(&sql)?;
        let rows = query.query_map(
            params![self.actor, std::path::MAIN_SEPARATOR_STR, selected.key],
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
        let selected = self.find(reference)?;
        self.editable(&selected)?;
        let tx = rusqlite::Transaction::new_unchecked(
            &self.db,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let id = if add {
            Some(dictionary_id(&tx, name)?)
        } else {
            tag_id(&tx, &tag_name(name)?)?
        };
        let old = if let Some(id) = id {
            let old = packed_tags::detail(&tx, &selected.id, id, packed_tags::USER)?;
            let mut writer = packed_tags::Writer::new(&tx);
            writer.assign(
                &selected.id,
                id,
                packed_tags::USER,
                add.then_some((confidence, None)),
            )?;
            writer.flush()?;
            if add || old.is_some() {
                crate::registry::record(
                    &tx,
                    id,
                    Some(crate::tiers::key(&tx, &selected.id)?),
                    Some("user"),
                    if add { "assign" } else { "remove" },
                    &self.actor,
                    Some(selected.revision),
                    Some(&json!({"confidence":confidence,"previous":old})),
                )?;
            }
            old.map(|(confidence, evidence)| json!({"confidence":confidence,"evidence":evidence}))
        } else {
            None
        };
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
        let readable = crate::security::allowed("paint");
        let sql = format!(
            "{AUTHORITY}, visible_blocks AS MATERIALIZED (
                SELECT k.key/4096 block,mosh_make_block(json_group_array(k.key%4096)) packed
                FROM object_keys k JOIN objects o ON o.key=k.key
                WHERE o.missing=0 AND {VISIBLE} AND {readable} GROUP BY k.key/4096
            ) SELECT t.name,sum(mosh_block_count(b.packed,v.packed)) n FROM tag_blocks b
            JOIN visible_blocks v ON v.block=b.block JOIN tag_dictionary t ON t.id=b.tag
            WHERE b.suggested=0 GROUP BY t.id HAVING n>0 ORDER BY n DESC,t.name LIMIT ?3"
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
        let readable = crate::security::allowed("paint");
        let eligible = format!("o.missing=0 AND o.kind_code=0 AND {VISIBLE} AND {readable}");
        let (total, assigned): (i64, i64) = self.db.query_row(
            &format!(
                "{AUTHORITY} SELECT count(*),coalesce(sum(s.confirmed),0)
            FROM objects o LEFT JOIN tag_sets s ON s.id=o.tag_set WHERE {eligible}"
            ),
            params![self.actor, std::path::MAIN_SEPARATOR_STR],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let mut statement = self.db.prepare(&format!(
            "{AUTHORITY} SELECT o.* FROM objects o WHERE {eligible} ORDER BY o.key LIMIT 32"
        ))?;
        let sample = statement
            .query_map(params![self.actor, std::path::MAIN_SEPARATOR_STR], object)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut covered = 0;
        let mut evaluated = 0;
        for file in &sample {
            let tier = self.file_tier_for(file)?;
            if !tier["individuality_score"].is_null() {
                evaluated += 1;
                covered += i64::from(tier["effective"] == tier["target"]);
            }
        }
        Ok(
            json!({"objects":total,"evaluated":evaluated,"covered":covered,"incomplete":evaluated-covered,
            "unevaluated":total-evaluated,"direct_assignments":assigned}),
        )
    }

    pub fn tag_dictionary(&self, action: &str, from: &str, to: Option<&str>) -> Result<()> {
        self.admin()?;
        let from = tag_name(from)?;
        let to = to.map(tag_name).transpose()?;
        let tx = rusqlite::Transaction::new_unchecked(
            &self.db,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let id = tag_id(&tx, &from)?.context("unknown tag")?;
        match action {
            "delete" => {
                packed_tags::Writer::new(&tx).retag(id, None)?;
                tx.execute("DELETE FROM tag_dictionary WHERE id=?", [id])?;
            }
            "alias" | "rename" => {
                let to = to.as_deref().context("new name required")?;
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
                let target = tag_id(&tx, to.as_deref().context("destination tag required")?)?
                    .context("unknown destination tag")?;
                ensure!(id != target, "choose two distinct tags");
                packed_tags::Writer::new(&tx).retag(id, Some(target))?;
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
        let destination = to
            .as_deref()
            .map(|name| tag_id(&tx, name))
            .transpose()?
            .flatten();
        if action == "merge" {
            tx.execute(
                "UPDATE tag_registry SET replacement=?,retired=? WHERE id=?",
                params![destination, crate::now(), id],
            )?;
        }
        crate::registry::record(
            &tx,
            id,
            None,
            None,
            action,
            &self.actor,
            None,
            Some(&json!({"from":from,"to":destination})),
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
        let selected = self.find(reference)?;
        if !self.permits(&selected, "paint")? {
            return Ok(None);
        }
        Ok(self.db.query_row("SELECT level,body,revision,locked,origin FROM paint WHERE object=?",[selected.key],|r| {
            let revision = r.get::<_,i64>(2)?;
            Ok(json!({"level":r.get::<_,String>(0)?,"body":r.get::<_,String>(1)?,"revision":revision,
                "stale":revision!=selected.revision,"locked":r.get::<_,bool>(3)?,"origin":r.get::<_,String>(4)?}))
        }).optional()?)
    }

    pub fn set_paint(&self, reference: &str, level: &str, body: &str, locked: bool) -> Result<()> {
        let selected = self.find(reference)?;
        self.editable(&selected)?;
        ensure!(
            body.len() <= 32768 && matches!(level, "none" | "minimal" | "standard" | "detailed"),
            "invalid Paint level or body length"
        );
        let before = self.paint(&selected.id)?;
        let tx = self.db.unchecked_transaction()?;
        if level == "none" {
            tx.execute("DELETE FROM paint WHERE object=?", [selected.key])?;
        } else {
            tx.execute("INSERT INTO paint VALUES(?,?,?,?,?, 'user') ON CONFLICT(object) DO UPDATE SET
                level=excluded.level,body=excluded.body,revision=excluded.revision,locked=excluded.locked,origin='user'",
                params![selected.key,level,body,selected.revision,locked])?;
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
        let tx = rusqlite::Transaction::new_unchecked(
            &self.db,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let id: String = tx.query_row(
            "INSERT INTO objects(kind_code,name) VALUES(?,?) RETURNING id",
            params![if marriage { 2 } else { 1 }, name.trim()],
            |row| row.get(0),
        )?;
        tx.execute(
            "INSERT INTO owners VALUES(mosh_key(?),?)",
            params![id, self.actor],
        )?;
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
        let container = self.find(container)?;
        let target = self.find(target)?;
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
            UNION SELECT o.key FROM objects o JOIN reachable r ON o.parent_key=r.id WHERE o.missing=0
        ) SELECT EXISTS(SELECT 1 FROM reachable WHERE id=?2)",params![target.key,container.key],|r|r.get(0))?;
        ensure!(
            kind == "related" || !cycle,
            "this relationship would create a cycle"
        );
        tx.execute(
            "INSERT OR IGNORE INTO edges VALUES(?,?,?,?)",
            params![container.key, target.key, kind, selector.unwrap_or("")],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn collection_query(&self, container: &str, query: &str) -> Result<()> {
        let selected = self.find(container)?;
        self.editable(&selected)?;
        ensure!(
            selected.kind == "marriage",
            "saved references belong to Marriages"
        );
        crate::query::validate(query)?;
        self.db.execute(
            "INSERT OR IGNORE INTO collection_queries VALUES(?,?)",
            params![selected.key, query],
        )?;
        Ok(())
    }

    pub fn graph(&self, reference: Option<&str>) -> Result<Vec<Value>> {
        let id = reference
            .map(|reference| self.find(reference).map(|object| object.key))
            .transpose()?;
        let sql = format!(
            "{AUTHORITY} SELECT o.*,e.kind AS edge_kind,e.selector,mosh_public_id(e.container) AS source
            FROM edges e JOIN objects o ON o.key=e.target
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
            if let Ok(source) = self.find(&source) {
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
