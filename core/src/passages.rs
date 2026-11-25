use crate::{Mosh, Object, content, organization};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Draft {
    pub quote: String,
    pub kind: String,
    pub actor: Option<String>,
    pub recipient: Option<String>,
    pub topic: Option<String>,
    pub status: Option<String>,
    pub date: Option<String>,
}

pub fn normalized(value: &str) -> String {
    value
        .trim()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn named(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 48
        && value.chars().next().is_some_and(char::is_uppercase)
        && value
            .chars()
            .all(|ch| ch.is_alphabetic() || matches!(ch, ' ' | '_' | '-'))
}

fn person(line: &str) -> Option<(&str, &str)> {
    let (who, line) = line.split_once(':')?;
    named(who.trim()).then_some((who.trim(), line.trim()))
}

fn calendar_date(value: &str) -> Option<String> {
    let text = value.trim().trim_start_matches('[');
    let candidate = text.get(..10)?;
    chrono::NaiveDate::parse_from_str(candidate, "%Y-%m-%d")
        .ok()
        .map(|d| d.to_string())
}

pub fn dialogue(source: &str) -> Vec<Draft> {
    let lines = source.lines().collect::<Vec<_>>();
    let mut offset = 0;
    let mut dated = None;
    let mut events = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let first = offset;
        offset += source[offset..]
            .find('\n')
            .map_or(source.len() - offset, |n| n + 1);
        if let Some(date) = calendar_date(line) {
            dated = Some(date);
        }
        let Some((actor, spoken)) = person(line) else {
            continue;
        };
        let Some((recipient, request)) = spoken.split_once(',') else {
            continue;
        };
        let recipient = recipient.trim();
        if !named(recipient) || recipient.eq_ignore_ascii_case(actor) {
            continue;
        }
        let request = request.trim();
        let lower = request.to_ascii_lowercase();
        if lower.starts_with("the deadline for ") {
            for change in [" moves to ", " is now ", " changed to "] {
                if let Some((topic, due)) = request[17..].split_once(change)
                    && let Some(date) = calendar_date(due)
                {
                    events.push(Draft {
                        quote: (*line).into(),
                        kind: "deadline".into(),
                        actor: Some(actor.into()),
                        recipient: Some(recipient.into()),
                        topic: Some(topic.into()),
                        status: Some("changed".into()),
                        date: Some(date),
                    });
                    break;
                }
            }
            continue;
        }
        let Some(prefix) = ["can you ", "could you ", "would you ", "please "]
            .into_iter()
            .find(|p| lower.starts_with(*p))
        else {
            continue;
        };
        let task = request[prefix.len()..]
            .trim_end_matches(['?', '.', '!'])
            .trim();
        if task.is_empty() {
            continue;
        }
        let mut end = first + line.len();
        let mut status = None;
        if let Some((response_actor, response)) = lines.get(index + 1).and_then(|line| person(line))
            && response_actor.eq_ignore_ascii_case(recipient)
        {
            let response = response.to_lowercase();
            if ["sure", "yes", "i'll", "i’ll", "i will", "i can", "accepted"]
                .iter()
                .any(|word| response.starts_with(word))
            {
                status = Some("accepted".into());
                end = offset + lines[index + 1].len();
            } else if ["no,", "i can't", "i can’t", "i cannot", "declined"]
                .iter()
                .any(|word| response.starts_with(word))
            {
                status = Some("declined".into());
                end = offset + lines[index + 1].len();
            }
        }
        if let Some(quote) = source.get(first..end) {
            events.push(Draft {
                quote: quote.into(),
                kind: "assignment".into(),
                actor: Some(actor.into()),
                recipient: Some(recipient.into()),
                topic: Some(task.into()),
                status,
                date: dated.clone(),
            });
        }
    }
    events
}

pub fn supported(draft: &Draft, source: &str) -> Option<(usize, usize)> {
    if draft.quote.trim().len() < 8
        || draft.quote.len() > 4096
        || !matches!(
            draft.kind.as_str(),
            "assignment" | "decision" | "commitment" | "deadline" | "fragment"
        )
    {
        return None;
    }
    let start = source.find(&draft.quote)?;
    let lower = draft.quote.to_lowercase();
    for value in [&draft.actor, &draft.recipient, &draft.topic]
        .into_iter()
        .flatten()
    {
        if value.trim().is_empty() || value.len() > 200 || !lower.contains(&value.to_lowercase()) {
            return None;
        }
    }
    if draft.kind == "assignment"
        && (draft.actor.is_none() || draft.recipient.is_none() || draft.topic.is_none())
    {
        return None;
    }
    Some((start, start + draft.quote.len()))
}

fn assignment_roles(draft: &Draft) -> bool {
    let (Some(actor), Some(recipient)) = (&draft.actor, &draft.recipient) else {
        return false;
    };
    let actor = normalized(actor);
    let recipient = normalized(recipient);
    if actor == recipient {
        return false;
    }
    if dialogue(&draft.quote).iter().any(|reference| {
        reference.kind == "assignment"
            && reference
                .actor
                .as_deref()
                .is_some_and(|value| normalized(value) == actor)
            && reference
                .recipient
                .as_deref()
                .is_some_and(|value| normalized(value) == recipient)
            && reference
                .topic
                .as_deref()
                .zip(draft.topic.as_deref())
                .is_some_and(|(source, topic)| normalized(source).contains(&normalized(topic)))
    }) {
        return true;
    }
    let text = normalized(&draft.quote);
    [" assigned ", " delegated ", " asked ", " tasked "]
        .iter()
        .any(|verb| {
            text.split_once(verb).is_some_and(|(before, after)| {
                before.split_whitespace().last() == Some(actor.as_str())
                    && (after.starts_with(&format!("{recipient} "))
                        || after.ends_with(&format!(" to {recipient}")))
            })
        })
}

pub fn event_tags(draft: &Draft) -> Vec<String> {
    let mut tags = vec![format!("activity:{}", draft.kind)];
    let (actor, recipient) = if draft.kind == "assignment" {
        ("assigner", "assignee")
    } else {
        ("speaker", "recipient")
    };
    for (field, value) in [
        (actor, &draft.actor),
        (recipient, &draft.recipient),
        ("status", &draft.status),
        ("date", &draft.date),
    ] {
        if let Some(value) = value {
            tags.push(format!("{field}:{}", normalized(value)));
        }
    }
    if let Some(topic) = &draft.topic {
        let value = normalized(topic);
        for word in value.split(|ch: char| !ch.is_alphanumeric()) {
            if word.len() >= 5
                && !matches!(
                    word,
                    "handle"
                        | "please"
                        | "could"
                        | "would"
                        | "start"
                        | "should"
                        | "perform"
                        | "about"
                )
            {
                tags.push(format!("topic:{word}"));
            }
        }
    }
    tags
}

pub fn save(
    db: &Connection,
    key: i64,
    revision: i64,
    source: &str,
    draft: &Draft,
    deterministic: bool,
) -> Result<Option<i64>> {
    let Some((start, end)) = supported(draft, source) else {
        return Ok(None);
    };
    if !deterministic && draft.kind == "assignment" && !assignment_roles(draft) {
        return Ok(None);
    }
    // Dates asserted by a model must occur in its evidence. A deterministic dated
    // conversation may establish the date in the preceding timestamp header.
    let date = draft
        .date
        .as_deref()
        .and_then(calendar_date)
        .filter(|date| deterministic || draft.quote.contains(date));
    let status = draft.status.as_ref().map(|v| normalized(v)).filter(|v| {
        matches!(
            v.as_str(),
            "accepted" | "declined" | "pending" | "changed" | "completed"
        ) && (deterministic || normalized(&draft.quote).contains(v.as_str()))
    });
    db.execute("INSERT INTO passages(object_key,revision,first_byte,end_byte,digest,kind,actor,recipient,topic,status,date)
        VALUES(?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT DO NOTHING",params![key,revision,start,end,
        blake3::hash(draft.quote.as_bytes()).as_bytes(),draft.kind,draft.actor.as_deref().map(normalized),
        draft.recipient.as_deref().map(normalized),draft.topic.as_deref().map(normalized),status,date])?;
    let id=db.query_row("SELECT id FROM passages WHERE object_key=? AND revision=? AND first_byte=? AND end_byte=? AND kind=?",
        params![key,revision,start,end,draft.kind],|r|r.get::<_,i64>(0))?;
    let mut verified = draft.clone();
    verified.date = date;
    verified.status = status;
    for tag in event_tags(&verified) {
        if organization::tag_name(&tag).is_ok() {
            let tag = organization::dictionary_id(db, &tag)?;
            db.execute(
                "INSERT OR IGNORE INTO passage_tags VALUES(?,?)",
                params![id, tag],
            )?;
        }
    }
    Ok(Some(id))
}

pub fn excerpt(
    db: &Connection,
    object: &Object,
    passage: i64,
    source: &str,
) -> Result<Option<Value>> {
    let record=db.prepare_cached("SELECT p.first_byte,p.end_byte,p.digest,p.kind,p.actor,p.recipient,p.topic,p.status,p.date,p.promoted
        FROM passages p JOIN object_keys k ON k.key=p.object_key WHERE k.object=? AND p.revision=? AND p.id=?")?
        .query_row(params![object.id,object.revision,passage],|r|Ok((r.get::<_,usize>(0)?,r.get::<_,usize>(1)?,
            r.get::<_,Vec<u8>>(2)?,r.get::<_,String>(3)?,r.get::<_,Option<String>>(4)?,r.get::<_,Option<String>>(5)?,
            r.get::<_,Option<String>>(6)?,r.get::<_,Option<String>>(7)?,r.get::<_,Option<String>>(8)?,r.get::<_,bool>(9)?))).optional()?;
    let Some((start, end, digest, kind, actor, recipient, topic, status, date, promoted)) = record
    else {
        return Ok(None);
    };
    let Some(quote) = source
        .get(start..end)
        .filter(|text| blake3::hash(text.as_bytes()).as_bytes() == digest.as_slice())
    else {
        return Ok(None);
    };
    let mut stmt=db.prepare("SELECT t.name FROM passage_tags p JOIN tag_dictionary t ON t.id=p.tag WHERE passage=? ORDER BY t.name")?;
    let tags = stmt
        .query_map([passage], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let start_window = if kind == "assignment"
        && quote.to_ascii_lowercase().contains("start next week")
    {
        use chrono::{Datelike, Duration, NaiveDate};
        date.as_deref().and_then(|date| NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()).map(|date| {
            let monday = date + Duration::days(7 - i64::from(date.weekday().num_days_from_monday()));
            json!({"from":monday.to_string(),"through":(monday+Duration::days(6)).to_string(),"precision":"week"})
        })
    } else {
        None
    };
    Ok(Some(
        json!({"id":passage,"quote":quote,"start":start,"end":end,"kind":kind,"actor":actor,"recipient":recipient,
        "topic":topic,"status":status,"date":date,"start_window":start_window,"promoted":promoted,"tags":tags,"verified_excerpt":true}),
    ))
}

impl Mosh {
    pub fn passages(&self, reference: &str) -> Result<Vec<Value>> {
        let object = self.get(reference)?;
        self.require_contents(&object)?;
        let mut stmt = self.db.prepare(
            "SELECT p.id FROM passages p JOIN object_keys k ON k.key=p.object_key
            WHERE k.object=? AND p.revision=? ORDER BY p.first_byte LIMIT 100",
        )?;
        let ids = stmt
            .query_map(params![object.id, object.revision], |r| r.get::<_, i64>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if ids.is_empty() {
            return Ok(vec![]);
        }
        let source = content::preview(&object);
        let mut rows = Vec::new();
        for id in ids {
            if let Some(row) = excerpt(&self.db, &object, id, &source)? {
                rows.push(row);
            }
        }
        Ok(rows)
    }
}
