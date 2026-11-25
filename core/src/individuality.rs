use crate::{Mosh, Object, SearchOptions, content, organization, packed_tags, tiers};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, Transaction, TransactionBehavior, params};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

const CANDIDATES: usize = 96;
const SEEDS: usize = 8;
const HIGH_OVERLAP: f64 = 0.80;

struct Neighbor {
    object: Object,
    tags: BTreeSet<i64>,
    overlap: f64,
    duplicate: bool,
}

fn count(db: &Connection, tag: i64) -> Result<i64> {
    Ok(db
        .prepare_cached(
            "SELECT coalesce(sum(mosh_block_size(packed)),0)
        FROM tag_blocks WHERE tag=? AND suggested=0",
        )?
        .query_row([tag], |r| r.get(0))?)
}

fn name(db: &Connection, tag: i64) -> Result<String> {
    Ok(db
        .prepare_cached("SELECT name FROM tag_dictionary WHERE id=?")?
        .query_row([tag], |r| r.get(0))?)
}

pub(crate) fn structured_signals(source: &str) -> BTreeMap<String, String> {
    let mut signals = BTreeMap::new();
    for line in source.lines().take(300) {
        let Some((field, value)) = line.trim().split_once(':') else {
            continue;
        };
        let field = field.to_ascii_lowercase();
        let value = value.trim();
        if !matches!(
            field.as_str(),
            "component" | "subsystem" | "project" | "environment" | "region" | "version"
        ) || value.is_empty()
            || value.len() > 56
            || !value
                .chars()
                .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '_' | '-' | '.'))
        {
            continue;
        }
        let tag = format!("{field}:{}", crate::passages::normalized(value));
        signals.entry(tag).or_insert_with(|| line.trim().into());
        if signals.len() == 12 {
            break;
        }
    }
    signals
}

impl Mosh {
    pub fn individuality(&self, reference: &str) -> Result<Value> {
        let file = self.find(reference)?;
        ensure!(
            file.kind == "file" && self.permits(&file, "paint")?,
            "Individuality is unavailable"
        );
        let key = tiers::key(&self.db, &file.id)?;
        let own = packed_tags::confirmed(&self.db, key)?;
        let mut frequencies = BTreeMap::new();
        if self.actor == "local" {
            for &tag in &own {
                frequencies.insert(tag, count(&self.db, tag)?);
            }
        }
        let mut seeds = own.iter().copied().collect::<Vec<_>>();
        seeds.sort_by_key(|tag| (frequencies.get(tag).copied().unwrap_or(1), *tag));
        let mut candidates = BTreeMap::new();
        let read = crate::security::allowed("paint");
        let authority = organization::AUTHORITY;
        let visible = organization::VISIBLE;
        let sql = format!(
            "{authority} SELECT o.id FROM object_keys k JOIN objects o ON o.id=k.object
            WHERE k.tag_set=(SELECT tag_set FROM object_keys WHERE key=?3)
            AND k.key<>?3 AND o.kind='file' AND o.missing=0 AND {visible} AND {read}
            ORDER BY k.key LIMIT 32"
        );
        let mut exact = self.db.prepare(&sql)?;
        for id in exact.query_map(
            params![self.actor, std::path::MAIN_SEPARATOR_STR, key],
            |r| r.get::<_, String>(0),
        )? {
            let id = id?;
            if let Ok(object) = self.find(&id) {
                candidates.insert(id, object);
            }
        }
        for &tag in seeds.iter().take(SEEDS) {
            let term = crate::intelligence::query_value(&name(&self.db, tag)?)?;
            let query = format!("kind:file AND direct-tag:{term}");
            for candidate in self.search(
                &query,
                &SearchOptions {
                    limit: 16,
                    ..Default::default()
                },
            )? {
                if candidate.id != file.id && self.permits(&candidate, "paint")? {
                    candidates.insert(candidate.id.clone(), candidate);
                }
                if candidates.len() >= CANDIDATES {
                    break;
                }
            }
            if candidates.len() >= CANDIDATES {
                break;
            }
        }
        let mut neighbors = Vec::with_capacity(candidates.len());
        for (_, object) in candidates {
            let tags = packed_tags::confirmed(&self.db, tiers::key(&self.db, &object.id)?)?;
            if self.actor != "local" {
                for &tag in &tags {
                    *frequencies.entry(tag).or_insert(1) += 1;
                }
            }
            let duplicate = self.permits(&file, "content")?
                && self.permits(&object, "content")?
                && file.size == object.size
                && file
                    .hash
                    .as_ref()
                    .is_some_and(|hash| object.hash.as_ref() == Some(hash));
            neighbors.push(Neighbor {
                object,
                tags,
                overlap: 0.0,
                duplicate,
            });
        }
        let weight = |tag: &i64| {
            1.0 / (1.0 + (frequencies.get(tag).copied().unwrap_or(1).max(1) as f64).ln())
        };
        for neighbor in &mut neighbors {
            let shared = own.intersection(&neighbor.tags).map(weight).sum::<f64>();
            let all = own.union(&neighbor.tags).map(weight).sum::<f64>();
            neighbor.overlap = if all > 0.0 { shared / all } else { 0.0 };
        }
        neighbors.sort_by(|a, b| {
            b.overlap
                .total_cmp(&a.overlap)
                .then_with(|| a.object.id.cmp(&b.object.id))
        });
        let closest = neighbors.iter().find(|neighbor| !neighbor.duplicate);
        let (feedback_total, difficult):(i64,i64)=self.db.query_row("SELECT count(*),
            coalesce(sum(NOT EXISTS(SELECT 1 FROM json_each(r.result) j WHERE j.key=0 AND j.value=?3)),0)
            FROM retrieval_feedback f JOIN retrievals r ON r.id=f.run WHERE f.object_key=?1 AND r.actor=?2 AND f.helpful=1",
            params![key,self.actor,file.id],|r|Ok((r.get(0)?,r.get(1)?)))?;
        let threshold = if difficult > 0 {
            HIGH_OVERLAP - 0.05
        } else {
            HIGH_OVERLAP
        };
        let score = if own.len() >= 2 {
            closest.map(|nearest| ((1.0 - nearest.overlap) * 100.0).round() as u32)
        } else {
            None
        };
        let collisions = neighbors
            .iter()
            .filter(|n| !n.duplicate && n.tags == own)
            .count();
        let verified_duplicates = neighbors.iter().filter(|n| n.duplicate).count();
        let close_distinct = neighbors
            .iter()
            .filter(|n| !n.duplicate && n.overlap >= threshold)
            .count();
        let reason = if own.len() < 2 {
            "Too few confirmed direct tags to characterize this file."
        } else if collisions > 0 {
            "Distinct files share the same confirmed tag set."
        } else if close_distinct > 0 {
            "The nearest distinct files share most distinguishing tag weight."
        } else if closest.is_none() {
            "The permitted sample contains no comparable distinct files."
        } else {
            "The permitted sample contains differing confirmed direct tags."
        };
        let relative = closest.map(|n| &n.tags);
        let mut distinct = own
            .iter()
            .copied()
            .filter(|tag| relative.is_none_or(|tags| !tags.contains(tag)))
            .collect::<Vec<_>>();
        distinct.sort_by(|a, b| weight(b).total_cmp(&weight(a)));
        let distinct=distinct.into_iter().take(10).map(|tag|Ok(json!({"tag":name(&self.db,tag)?,
            "sampled_or_indexed_frequency":frequencies.get(&tag).copied().unwrap_or(1),"weight":weight(&tag)}))).collect::<Result<Vec<_>>>()?;
        let nearest=neighbors.iter().take(10).map(|n|json!({"id":n.object.id,"name":n.object.name,
            "overlap":n.overlap,"confirmed_collision":n.tags==own,"verified_content_duplicate":n.duplicate})).collect::<Vec<_>>();
        let mut redundant_in_sample = Vec::new();
        let common = seeds.iter().take(8).copied().collect::<Vec<_>>();
        for (index, a) in common.iter().enumerate() {
            for b in common.iter().skip(index + 1) {
                let both = neighbors
                    .iter()
                    .filter(|n| n.tags.contains(a) && n.tags.contains(b))
                    .count();
                let either = neighbors
                    .iter()
                    .filter(|n| n.tags.contains(a) || n.tags.contains(b))
                    .count();
                if both >= 3 && both == either && both < neighbors.len() {
                    redundant_in_sample.push(json!({"first":name(&self.db,*a)?,"second":name(&self.db,*b)?,"co_occurrences":both}));
                }
            }
        }
        let tier = self.file_tier_for(&file)?;
        let proposed =
            if close_distinct > 0 && tier["locked"] != true && tier["requested"].is_null() {
                tiers::policies(&self.db)?
                    .into_iter()
                    .map(|t| t.level)
                    .find(|l| u64::from(*l) > tier["target"].as_u64().unwrap_or(1))
            } else {
                None
            };
        Ok(
            json!({"id":file.id,"score":score,"reason":reason,"nearest":nearest,"signals":distinct,
            "confirmed_direct_tags":own.len(),"sample":neighbors.len(),"distinct_collisions":collisions,
            "high_overlap_distinct":close_distinct,"verified_duplicates":verified_duplicates,
            "overlap_threshold":threshold,"confirmed_queries":feedback_total,"difficult_queries":difficult,
            "recommended_target":proposed,"redundant_in_sample":redundant_in_sample}),
        )
    }

    pub fn vocabulary_advice(&self, reference: &str) -> Result<Value> {
        let file = self.find(reference)?;
        self.require_contents(&file)?;
        let individuality = self.individuality(&file.id)?;
        let key = tiers::key(&self.db, &file.id)?;
        let feedback:i64=self.db.query_row("SELECT count(*) FROM retrieval_feedback f JOIN retrievals r ON r.id=f.run
            WHERE f.object_key=?1 AND r.actor=?2 AND f.helpful=1 AND
            (json_extract(r.metrics,'$.matches')=0 OR EXISTS(SELECT 1 FROM json_each(r.result) j WHERE j.key>=1 AND j.value=?3))",
            params![key,self.actor,file.id],|r|r.get(0))?;
        let near = individuality["nearest"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default();
        let mut cluster = Vec::new();
        let threshold = individuality["overlap_threshold"]
            .as_f64()
            .unwrap_or(HIGH_OVERLAP);
        for neighbor in near.iter().filter(|n| {
            n["verified_content_duplicate"] == false
                && n["overlap"].as_f64().unwrap_or(0.0) >= threshold
        }) {
            if let Some(id) = neighbor["id"].as_str()
                && let Ok(obj) = self.find(id)
                && self.permits(&obj, "content")?
            {
                cluster.push(obj);
            }
        }
        let mut supports = BTreeMap::<String, Vec<Value>>::new();
        for item in std::iter::once(&file).chain(&cluster) {
            for (tag, evidence) in structured_signals(&content::preview(item)) {
                supports
                    .entry(tag)
                    .or_default()
                    .push(json!({"file":item.id,"revision":item.revision,"evidence":evidence}));
            }
        }
        let mut proposals = Vec::new();
        let mut broad = Vec::new();
        for (tag, evidence) in supports {
            if evidence.len() == 1 + cluster.len() && !cluster.is_empty() {
                broad.push(tag);
            } else if evidence.iter().any(|item| item["file"] == file.id) {
                let existing = organization::tag_id(&self.db, &tag)?.is_some();
                proposals.push(json!({"tag":tag,"evidence":evidence,"existing":existing,
                    "reusable":evidence.len()>1,"recommendation":if existing || evidence.len()==1 {"enrich-file"} else {"review-vocabulary"}}));
            }
        }
        let method = if individuality["high_overlap_distinct"] == 0
            && individuality["verified_duplicates"].as_u64().unwrap_or(0) > 0
        {
            "accept-verified-duplicates"
        } else if cluster.is_empty() {
            "no-change"
        } else if proposals
            .iter()
            .any(|p| p["recommendation"] == "review-vocabulary")
        {
            "review-vocabulary"
        } else if !proposals.is_empty() || feedback > 0 {
            "enrich-file"
        } else if individuality["confirmed_queries"].as_u64().unwrap_or(0) > 0 && feedback == 0 {
            "accept-observed-overlap"
        } else {
            "review-ambiguity"
        };
        let own = packed_tags::confirmed(&self.db, key)?;
        let mut tag_outcomes = Vec::new();
        for id in own.into_iter().take(12) {
            let tag = name(&self.db, id)?;
            let (queries,top1):(i64,i64)=self.db.query_row("SELECT count(*),
                coalesce(sum(EXISTS(SELECT 1 FROM json_each(r.result) j WHERE j.key=0 AND j.value=?3)),0)
                FROM retrieval_feedback f JOIN retrievals r ON r.id=f.run
                WHERE f.object_key=?1 AND r.actor=?2 AND f.helpful=1
                AND (EXISTS(SELECT 1 FROM json_each(r.plan,'$.branches') b,json_each(b.value,'$.tags') t WHERE t.value=?4)
                    OR instr(lower(r.request),'tag:'||?4)>0 OR instr(lower(r.request),'tag:'||char(34)||?4||char(34))>0)",
                params![key,self.actor,file.id,tag],|r|Ok((r.get(0)?,r.get(1)?)))?;
            if queries > 0 {
                tag_outcomes
                    .push(json!({"tag":tag,"confirmed_uses":queries,"confirmed_top1_uses":top1}));
            }
        }
        Ok(
            json!({"individuality":individuality,"recommendation":method,"confirmed_difficult_retrievals":feedback,
            "proposals":proposals,"broad_in_sample":broad,"observed_tag_outcomes":tag_outcomes}),
        )
    }

    pub fn apply_vocabulary_tag(
        &self,
        reference: &str,
        tag: &str,
        confirmed: bool,
    ) -> Result<Value> {
        self.admin()?;
        ensure!(
            confirmed,
            "review the evidence and confirm the vocabulary change"
        );
        let tag = organization::tag_name(tag)?;
        let advice = self.vocabulary_advice(reference)?;
        let proposal = advice["proposals"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|p| p["tag"] == tag)
            .context("evidence no longer supports this proposal")?;
        let tx = Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)?;
        let tag_id = organization::dictionary_id(&tx, &tag)?;
        let retired: bool = tx.query_row(
            "SELECT retired IS NOT NULL FROM tag_registry WHERE id=?",
            [tag_id],
            |r| r.get(0),
        )?;
        ensure!(!retired, "the proposed tag is retired");
        let mut writer = packed_tags::Writer::new(&tx);
        let mut changed = 0;
        for evidence in proposal["evidence"].as_array().into_iter().flatten() {
            let id = evidence["file"]
                .as_str()
                .context("proposal file is unavailable")?;
            let object = self.find(id)?;
            self.require_contents(&object)?;
            ensure!(
                evidence["revision"] == object.revision,
                "source changed during the review"
            );
            let source = content::preview(&object);
            let current = structured_signals(&source);
            let exact = evidence["evidence"]
                .as_str()
                .context("proposal evidence is missing")?;
            ensure!(
                current.get(&tag).map(String::as_str) == Some(exact),
                "proposal evidence changed"
            );
            let key = tiers::key(&tx, id)?;
            if packed_tags::confirmed(&tx, key)?.contains(&tag_id) {
                continue;
            }
            writer.assign(
                id,
                tag_id,
                packed_tags::CONTEXTUAL,
                Some((1.0, Some(exact))),
            )?;
            crate::registry::record(
                &tx,
                tag_id,
                Some(key),
                Some("contextual"),
                "assigned",
                &self.actor,
                Some(object.revision),
                Some(&json!({"reason":"confirmed-vocabulary-review"})),
            )?;
            changed += 1;
        }
        writer.flush()?;
        tx.commit()?;
        Ok(json!({"tag":tag,"changed_files":changed,"approved":true}))
    }
}
