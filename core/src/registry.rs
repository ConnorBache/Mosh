use crate::{Mosh, SearchOptions, now, organization};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, Transaction, TransactionBehavior, params};
use serde_json::{Value, json};

pub(crate) fn record(
    db: &Connection,
    tag: i64,
    object: Option<i64>,
    origin: Option<&str>,
    action: &str,
    actor: &str,
    revision: Option<i64>,
    details: Option<&Value>,
) -> Result<()> {
    db.prepare_cached("INSERT INTO tag_history(tag,object_key,origin,action,time,actor,revision,details) VALUES(?,?,?,?,?,?,?,?)")?
        .execute(params![tag,object,origin,action,now(),actor,revision,details.map(Value::to_string)])?;
    Ok(())
}

impl Mosh {
    pub fn tag_registry(&self, reference: Option<&str>, limit: usize) -> Result<Vec<Value>> {
        self.admin()?;
        let reference = reference.map(organization::tag_name).transpose()?;
        let mut stmt=self.db.prepare("SELECT r.id,r.canonical,r.category,r.description,r.created,r.creator,r.modified,r.retired,r.replacement,r.deleted,
            (SELECT json_group_array(alias) FROM tag_aliases a WHERE a.tag=r.id),
            coalesce((SELECT sum(mosh_block_size(packed)) FROM tag_blocks b WHERE b.tag=r.id AND b.suggested=0),0)
            FROM tag_registry r WHERE ?1 IS NULL OR r.canonical=?1 OR r.id=(SELECT tag FROM tag_aliases WHERE alias=?1)
            ORDER BY r.canonical,r.id DESC LIMIT ?2")?;
        let rows=stmt.query_map(params![reference,limit.clamp(1,1000)],|r|Ok(json!({
            "id":r.get::<_,i64>(0)?,"name":r.get::<_,String>(1)?,"category":r.get::<_,Option<String>>(2)?,
            "description":r.get::<_,Option<String>>(3)?,"created":r.get::<_,Option<i64>>(4)?,
            "creator":r.get::<_,Option<String>>(5)?,"modified":r.get::<_,Option<i64>>(6)?,
            "deprecated_at":r.get::<_,Option<i64>>(7)?,"replacement":r.get::<_,Option<i64>>(8)?,
            "deleted":r.get::<_,bool>(9)?,"aliases":serde_json::from_str::<Value>(&r.get::<_,String>(10)?).unwrap_or_default(),
            "stored_confirmed_assignments":r.get::<_,i64>(11)?
        })))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn tag_history(&self, name: &str, limit: usize) -> Result<Vec<Value>> {
        self.admin()?;
        let name = organization::tag_name(name)?;
        let mut stmt=self.db.prepare("SELECT h.tag,k.object,h.origin,h.action,h.actor,h.revision,h.time,h.details
            FROM tag_history h JOIN tag_registry r ON r.id=h.tag LEFT JOIN object_keys k ON k.key=h.object_key
            WHERE r.canonical=?1 OR r.id=(SELECT tag FROM tag_aliases WHERE alias=?1) ORDER BY h.seq DESC LIMIT ?2")?;
        let rows=stmt.query_map(params![name,limit.clamp(1,1000)],|r|Ok(json!({"tag_id":r.get::<_,i64>(0)?,
            "object":r.get::<_,Option<String>>(1)?,"origin":r.get::<_,Option<String>>(2)?,
            "action":r.get::<_,String>(3)?,"actor":r.get::<_,String>(4)?,"revision":r.get::<_,Option<i64>>(5)?,
            "time":r.get::<_,i64>(6)?,"details":r.get::<_,Option<String>>(7)?.and_then(|v|serde_json::from_str::<Value>(&v).ok())})))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn maintain_tag(
        &self,
        name: &str,
        action: &str,
        category: Option<&str>,
        description: Option<&str>,
        replacement: Option<&str>,
    ) -> Result<Value> {
        self.admin()?;
        let name = organization::tag_name(name)?;
        ensure!(
            category.is_none_or(|v| v.len() <= 96) && description.is_none_or(|v| v.len() <= 2048),
            "tag metadata is too long"
        );
        let tx = Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)?;
        let previous = organization::tag_id(&tx, &name)?;
        let tag = if action == "create" {
            organization::dictionary_id(&tx, &name)?
        } else {
            previous.context("unknown active tag")?
        };
        match action {
            "create" | "describe" => {
                tx.execute("UPDATE tag_registry SET category=coalesce(?,category),description=coalesce(?,description),
                    creator=CASE WHEN ? THEN ? ELSE creator END,modified=? WHERE id=?",
                    params![category,description,previous.is_none(),self.actor,now(),tag])?;
            }
            "retire" => {
                let replacement = replacement
                    .map(|text| {
                        organization::tag_id(&tx, &organization::tag_name(text)?)?
                            .context("unknown replacement")
                    })
                    .transpose()?;
                ensure!(
                    replacement != Some(tag),
                    "replacement must be a different tag"
                );
                tx.execute(
                    "UPDATE tag_registry SET retired=?,replacement=?,modified=? WHERE id=?",
                    params![now(), replacement, now(), tag],
                )?;
            }
            "reactivate" => {
                tx.execute(
                    "UPDATE tag_registry SET retired=NULL,replacement=NULL,modified=? WHERE id=?",
                    params![now(), tag],
                )?;
            }
            _ => anyhow::bail!("unknown registry action"),
        }
        record(
            &tx,
            tag,
            None,
            None,
            action,
            &self.actor,
            None,
            Some(&json!({"replacement":replacement,"category":category})),
        )?;
        tx.commit()?;
        Ok(json!(self.tag_registry(Some(&name), 20)?))
    }

    pub fn tag_preview(&self, name: &str, limit: usize) -> Result<Value> {
        self.admin()?;
        let name = organization::tag_name(name)?;
        let safe = crate::intelligence::query_value(&name)?;
        let matches = self.search(
            &format!("direct-tag:{safe} OR suggested-tag:{safe}"),
            &SearchOptions {
                limit: limit.clamp(1, 2000),
                ..Default::default()
            },
        )?;
        let mut stmt = self.db.prepare(
            "SELECT actor,name,query FROM saved_searches WHERE instr(lower(query),?)>0 LIMIT 200",
        )?;
        let dependent=stmt.query_map([&name],|r|Ok(json!({"actor":r.get::<_,String>(0)?,"name":r.get::<_,String>(1)?,"query":r.get::<_,String>(2)?})))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(json!({"matches":matches,"saved_searches":dependent}))
    }

    pub fn split_tag(&self, from: &str, into: &str, query: &str, confirmed: bool) -> Result<Value> {
        self.admin()?;
        let from = organization::tag_name(from)?;
        let into = organization::tag_name(into)?;
        ensure!(
            from != into && !query.trim().is_empty(),
            "choose a distinct tag and a selection query"
        );
        let safe = crate::intelligence::query_value(&from)?;
        let selected = self.search(
            &format!("({query}) AND direct-tag:{safe}"),
            &SearchOptions {
                limit: 2000,
                ..Default::default()
            },
        )?;
        if !confirmed {
            return Ok(json!({"affected":selected,"preview":true}));
        }
        ensure!(
            selected.len() < 2000,
            "narrow split to fewer than 2,000 objects"
        );
        let tx = Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)?;
        let from_id = organization::tag_id(&tx, &from)?.context("source tag is unavailable")?;
        let into_id = organization::dictionary_id(&tx, &into)?;
        ensure!(from_id != into_id, "destination resolves to the source tag");
        let mut writer = crate::packed_tags::Writer::new(&tx);
        for object in &selected {
            for (origin, _) in crate::packed_tags::ORIGINS {
                if let Some((confidence, evidence)) =
                    crate::packed_tags::detail(&tx, &object.id, from_id, origin)?
                {
                    if crate::packed_tags::detail(&tx, &object.id, into_id, origin)?
                        .is_none_or(|(previous, _)| confidence > previous)
                    {
                        writer.assign(
                            &object.id,
                            into_id,
                            origin,
                            Some((confidence, evidence.as_deref())),
                        )?;
                    }
                    writer.assign(&object.id, from_id, origin, None)?;
                }
            }
        }
        writer.flush()?;
        record(
            &tx,
            from_id,
            None,
            None,
            "split",
            &self.actor,
            None,
            Some(&json!({"into":into_id,"count":selected.len(),"query":query})),
        )?;
        tx.commit()?;
        Ok(json!({"affected":selected.len(),"into":into_id}))
    }
}
