use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, functions::FunctionFlags, params};
use std::collections::{BTreeMap, HashMap};

pub const USER: u8 = 1;
pub const FILESYSTEM: u8 = 4;
pub const SUGGESTED: u8 = 16;
pub const AI: u8 = 32;
pub const CONTEXTUAL: u8 = 64;
pub const ORIGINS: [(u8, &str); 7] = [
    (USER, "user"),
    (2, "automatic"),
    (FILESYSTEM, "filesystem"),
    (8, "git"),
    (SUGGESTED, "suggested"),
    (AI, "ai"),
    (CONTEXTUAL, "contextual"),
];
const BLOCK: i64 = 4096;
const BUFFERED_BLOCKS: usize = 4096;
const SET_GAPS: u8 = 128;
const SET_RUNS: u8 = 129;
type Tags = BTreeMap<i64, u8>;
type BlockKey = (i64, bool, i64);

fn push_num(buffer: &mut Vec<u8>, mut n: u64) {
    while n >= 128 {
        buffer.push((n as u8 & 127) | 128);
        n >>= 7;
    }
    buffer.push(n as u8);
}

fn pop_num(buffer: &mut &[u8]) -> Result<u64> {
    let mut result = 0u64;
    for shift in (0..64).step_by(7) {
        let (&byte, tail) = buffer.split_first().context("truncated packed integer")?;
        *buffer = tail;
        let part = u64::from(byte & 127);
        ensure!(shift != 63 || part <= 1, "packed integer overflow");
        result |= part << shift;
        if byte < 128 {
            return Ok(result);
        }
    }
    anyhow::bail!("packed integer overflow")
}

fn add_gap(current: u64, buffer: &mut &[u8]) -> Result<u64> {
    current
        .checked_add(pop_num(buffer)?)
        .context("packed gap overflow")
}

fn pack_set(tags: &Tags) -> Vec<u8> {
    let common = tags
        .values()
        .next()
        .copied()
        .filter(|bit| !matches!(bit, 32 | 33) && tags.values().all(|value| value == bit))
        .unwrap_or(0);
    let mut packed = vec![common];
    let mut previous = 0;
    for (&tag, &origins) in tags {
        push_num(&mut packed, (tag - previous) as u64);
        if common == 0 {
            packed.push(origins);
        }
        previous = tag;
    }
    if tags.is_empty() {
        return packed;
    }
    // Store uncommon origins by ordinal instead of spending an origin byte on every tag.
    let mut frequency = [0usize; 128];
    for &bits in tags.values() {
        frequency[bits as usize] += 1;
    }
    let default = frequency
        .iter()
        .enumerate()
        .max_by_key(|(_, count)| *count)
        .unwrap()
        .0 as u8;
    let mut overrides = Vec::new();
    let mut after = 0;
    for (index, &bits) in tags.values().enumerate() {
        if bits != default {
            push_num(&mut overrides, (index - after) as u64);
            overrides.push(bits);
            after = index + 1;
        }
    }
    let mut gaps = vec![SET_GAPS, default];
    push_num(&mut gaps, tags.len() as u64);
    let mut prior = 0;
    for &tag in tags.keys() {
        push_num(&mut gaps, (tag - prior) as u64);
        prior = tag;
    }
    gaps.extend_from_slice(&overrides);
    if gaps.len() < packed.len() {
        packed = gaps;
    }

    let mut runs = vec![SET_RUNS, default];
    push_num(&mut runs, tags.len() as u64);
    let mut keys = tags.keys().copied().peekable();
    let mut prior = 0;
    while let Some(first) = keys.next() {
        let mut last = first;
        while let Some(next) = last
            .checked_add(1)
            .and_then(|value| keys.next_if_eq(&value))
        {
            last = next;
        }
        push_num(&mut runs, (first - prior) as u64);
        push_num(&mut runs, (last - first + 1) as u64);
        prior = last;
    }
    runs.extend_from_slice(&overrides);
    if runs.len() < packed.len() {
        packed = runs;
    }
    packed
}

fn unpack_set(packed: &[u8]) -> Result<Tags> {
    let (&common, mut tail) = packed.split_first().context("missing tag-set header")?;
    if matches!(common, 32 | 33 | SET_GAPS | SET_RUNS) {
        let (&default, remaining) = tail.split_first().context("missing default tag origin")?;
        ensure!((1..128).contains(&default), "invalid default tag origin");
        tail = remaining;
        let count = pop_num(&mut tail)?;
        ensure!(count > 0, "packed tag count must be positive");
        let mut entries = Vec::new();
        let mut last = 0;
        while entries.len() < count as usize {
            let first = add_gap(last, &mut tail)?;
            let length = if matches!(common, 33 | SET_RUNS) {
                pop_num(&mut tail)?
            } else {
                1
            };
            ensure!(
                first > last && length > 0 && length <= count - entries.len() as u64,
                "invalid tag run"
            );
            last = first.checked_add(length - 1).context("tag run overflow")?;
            ensure!(last <= i64::MAX as u64, "tag ID out of range");
            entries.try_reserve(usize::try_from(length)?)?;
            entries.extend((first..=last).map(|tag| (tag as i64, default)));
        }
        let mut after = 0;
        while !tail.is_empty() {
            let index = add_gap(after, &mut tail)?;
            ensure!(index < count, "tag origin override exceeds set");
            let (&bits, rest) = tail.split_first().context("missing tag origin override")?;
            ensure!((1..128).contains(&bits), "invalid tag origin override");
            entries[index as usize].1 = bits;
            tail = rest;
            after = index + 1;
        }
        return Ok(entries.into_iter().collect());
    }
    let mut tags = Tags::new();
    let mut tag = 0u64;
    while !tail.is_empty() {
        let next = add_gap(tag, &mut tail)?;
        ensure!(
            next > tag && next <= i64::MAX as u64,
            "invalid packed tag set"
        );
        tag = next;
        let origins = if common == 0 {
            let (&origins, rest) = tail.split_first().context("missing tag origins")?;
            tail = rest;
            origins
        } else {
            common
        };
        ensure!((1..128).contains(&origins), "invalid tag origins");
        tags.insert(tag as i64, origins);
    }
    Ok(tags)
}

pub fn pack_keys(keys: impl IntoIterator<Item = i64>) -> Vec<u8> {
    let mut keys = keys.into_iter().collect::<Vec<_>>();
    keys.sort_unstable();
    keys.dedup();
    let mut packed = Vec::new();
    let mut previous = 0;
    for key in keys {
        push_num(&mut packed, (key - previous) as u64);
        previous = key;
    }
    packed
}

#[derive(Clone)]
struct Block([u64; 64]);

impl Block {
    fn empty() -> Self {
        Self([0; 64])
    }

    fn from_bytes(packed: &[u8]) -> Result<Self> {
        let (&kind, mut data) = packed
            .split_first()
            .context("missing posting block header")?;
        let mut out = Self::empty();
        match kind {
            b'A' if data.is_empty() => out.0.fill(u64::MAX),
            b'B' if data.len() == 512 => {
                for (word, bytes) in out.0.iter_mut().zip(data.chunks_exact(8)) {
                    *word = u64::from_le_bytes(bytes.try_into()?);
                }
            }
            b'L' => {
                let mut offset = 0;
                while !data.is_empty() {
                    let next = add_gap(offset, &mut data)?;
                    ensure!(next < BLOCK as u64, "posting offset exceeds block");
                    out.set(next as usize, true);
                    offset = next + 1;
                }
            }
            b'R' => {
                let mut start = 0;
                while !data.is_empty() {
                    start = add_gap(start, &mut data)?;
                    let length = pop_num(&mut data)?;
                    let end = start.checked_add(length).context("posting run overflow")?;
                    ensure!(
                        length > 0 && end <= BLOCK as u64,
                        "posting run exceeds block"
                    );
                    for offset in start..end {
                        out.set(offset as usize, true);
                    }
                    start = end;
                }
            }
            _ => anyhow::bail!("unrecognized posting block"),
        }
        Ok(out)
    }

    fn set(&mut self, slot: usize, enabled: bool) {
        let bit = 1u64 << (slot % 64);
        if enabled {
            self.0[slot / 64] |= bit;
        } else {
            self.0[slot / 64] &= !bit;
        }
    }

    fn slots(&self) -> impl Iterator<Item = usize> + '_ {
        self.0.iter().enumerate().flat_map(|(index, &word)| {
            let mut word = word;
            std::iter::from_fn(move || {
                if word == 0 {
                    return None;
                }
                let offset = word.trailing_zeros() as usize;
                word &= word - 1;
                Some(index * 64 + offset)
            })
        })
    }

    fn encode(&self) -> Option<Vec<u8>> {
        let count = self.0.iter().map(|word| word.count_ones()).sum::<u32>();
        if count == 0 {
            return None;
        }
        if count == BLOCK as u32 {
            return Some(vec![b'A']);
        }
        let mut best = vec![b'B'];
        for word in self.0 {
            best.extend_from_slice(&word.to_le_bytes());
        }
        if count < 512 {
            let mut list = vec![b'L'];
            let mut previous = 0;
            for slot in self.slots() {
                push_num(&mut list, (slot - previous) as u64);
                previous = slot + 1;
            }
            if list.len() < best.len() {
                best = list;
            }
        }
        // Count run starts before trying a run list. Each run needs at least two bytes.
        let mut carry = 0;
        let mut starts = 0;
        for &word in &self.0 {
            starts += (word & !((word << 1) | carry)).count_ones();
            carry = word >> 63;
        }
        if 1 + 2 * starts < best.len() as u32 {
            let mut runs = vec![b'R'];
            let mut slots = self.slots().peekable();
            let mut previous = 0;
            while let Some(first) = slots.next() {
                let mut end = first + 1;
                while slots.next_if_eq(&end).is_some() {
                    end += 1;
                }
                push_num(&mut runs, (first - previous) as u64);
                push_num(&mut runs, (end - first) as u64);
                previous = end;
            }
            if runs.len() < best.len() {
                best = runs;
            }
        }
        Some(best)
    }
}

fn sql<T>(result: Result<T>) -> rusqlite::Result<T> {
    result.map_err(|e| rusqlite::Error::UserFunctionError(e.into()))
}

pub fn register(db: &Connection) -> Result<()> {
    let flags = FunctionFlags::SQLITE_DETERMINISTIC
        | FunctionFlags::SQLITE_INNOCUOUS
        | FunctionFlags::SQLITE_UTF8;
    db.create_scalar_function("mosh_block_slots", 1, flags, |ctx| {
        let block = sql(Block::from_bytes(ctx.get_raw(0).as_blob()?))?;
        Ok(serde_json::to_string(&block.slots().collect::<Vec<_>>()).unwrap())
    })?;
    db.create_scalar_function("mosh_make_block", 1, flags, |ctx| {
        let slots: Vec<usize> = serde_json::from_str(&ctx.get::<String>(0)?)
            .map_err(|e| rusqlite::Error::UserFunctionError(Box::new(e)))?;
        let mut block = Block::empty();
        for slot in slots {
            sql((slot < BLOCK as usize)
                .then_some(())
                .context("invalid live slot"))?;
            block.set(slot, true);
        }
        Ok(block.encode())
    })?;
    db.create_scalar_function("mosh_block_count", 2, flags, |ctx| {
        let a = sql(Block::from_bytes(ctx.get_raw(0).as_blob()?))?;
        let b = sql(Block::from_bytes(ctx.get_raw(1).as_blob()?))?;
        Ok(a.0
            .into_iter()
            .zip(b.0)
            .map(|(a, b)| (a & b).count_ones())
            .sum::<u32>())
    })?;
    db.create_scalar_function("mosh_block_size", 1, flags, |ctx| {
        let block = sql(Block::from_bytes(ctx.get_raw(0).as_blob()?))?;
        Ok(block.0.iter().map(|word| word.count_ones()).sum::<u32>())
    })?;
    db.create_scalar_function("mosh_common_slots", -1, flags, |ctx| {
        let mut common = Block([u64::MAX; 64]);
        for i in 0..ctx.len() {
            let block = sql(Block::from_bytes(ctx.get_raw(i).as_blob()?))?;
            for (a, b) in common.0.iter_mut().zip(block.0) {
                *a &= b;
            }
        }
        Ok(serde_json::to_string(&common.slots().collect::<Vec<_>>()).unwrap())
    })?;
    db.create_scalar_function("mosh_set_tags", 1, flags, |ctx| {
        let tags = sql(unpack_set(ctx.get_raw(0).as_blob()?))?;
        Ok(serde_json::to_string(&tags.into_iter().collect::<Vec<_>>()).unwrap())
    })?;
    db.create_scalar_function("mosh_keys_contains", 2, flags, |ctx| {
        let mut tail = ctx.get_raw(0).as_blob()?;
        let target = ctx.get::<i64>(1)? as u64;
        let mut key = 0;
        while key < target && !tail.is_empty() {
            key = sql(add_gap(key, &mut tail))?;
        }
        Ok(key == target)
    })?;
    Ok(())
}

pub fn origin_bit(origin: &str) -> Result<u8> {
    ORIGINS
        .iter()
        .find_map(|(bit, name)| (*name == origin).then_some(*bit))
        .context("unknown tag origin")
}

fn default_evidence(origin: u8) -> Option<&'static str> {
    (origin == FILESYSTEM).then_some("file metadata")
}

fn key(db: &Connection, object: &str) -> Result<i64> {
    Ok(db
        .prepare_cached("SELECT key FROM object_keys WHERE object=?")?
        .query_row([object], |r| r.get(0))?)
}

fn read_set(db: &Connection, object_key: i64) -> Result<(Option<i64>, Tags)> {
    let (set_id,packed): (Option<i64>,Option<Vec<u8>>) = db.prepare_cached(
        "SELECT s.id,s.packed FROM object_keys k LEFT JOIN tag_sets s ON s.id=k.tag_set WHERE k.key=?",
    )?.query_row([object_key],|r|Ok((r.get(0)?,r.get(1)?)))?;
    Ok((
        set_id,
        packed
            .as_deref()
            .map(unpack_set)
            .transpose()?
            .unwrap_or_default(),
    ))
}

pub(crate) fn confirmed(db: &Connection, object: i64) -> Result<std::collections::BTreeSet<i64>> {
    Ok(read_set(db, object)?
        .1
        .into_iter()
        .filter_map(|(id, origins)| (origins & !SUGGESTED != 0).then_some(id))
        .collect())
}

pub fn detail(
    db: &Connection,
    object: &str,
    tag: i64,
    origin: u8,
) -> Result<Option<(f64, Option<String>)>> {
    let object_key = key(db, object)?;
    let (_, tags) = read_set(db, object_key)?;
    if tags.get(&tag).is_none_or(|bits| bits & origin == 0) {
        return Ok(None);
    }
    Ok(Some(detail_by_key(db, object_key, tag, origin)?))
}

fn detail_by_key(
    db: &Connection,
    object: i64,
    tag: i64,
    origin: u8,
) -> Result<(f64, Option<String>)> {
    let detail = db
        .prepare_cached(
            "SELECT confidence,evidence FROM tag_details WHERE object_key=? AND tag=? AND origin=?",
        )?
        .query_row(params![object, tag, origin], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    Ok(detail.unwrap_or_else(|| (1.0, default_evidence(origin).map(str::to_owned))))
}

fn store_detail(
    db: &Connection,
    object: i64,
    tag: i64,
    origin: u8,
    confidence: f64,
    evidence: Option<&str>,
) -> Result<()> {
    if confidence == 1.0 && evidence == default_evidence(origin) {
        db.prepare_cached("DELETE FROM tag_details WHERE object_key=? AND tag=? AND origin=?")?
            .execute(params![object, tag, origin])?;
    } else {
        db.prepare_cached("INSERT INTO tag_details VALUES(?,?,?,?,?) ON CONFLICT DO UPDATE SET confidence=excluded.confidence,evidence=excluded.evidence")?
            .execute(params![object,tag,origin,confidence,evidence])?;
    }
    Ok(())
}

/// A transaction-local write buffer for reverse blocks. Drain it before querying postings.
pub struct Writer<'a> {
    db: &'a Connection,
    blocks: HashMap<BlockKey, Block>,
}

impl<'a> Writer<'a> {
    pub fn new(db: &'a Connection) -> Self {
        Self {
            db,
            blocks: HashMap::new(),
        }
    }

    fn posting(&mut self, tag: i64, suggested: bool, key: i64, add: bool) -> Result<()> {
        let block = key / BLOCK;
        if self.blocks.len() == BUFFERED_BLOCKS
            && !self.blocks.contains_key(&(tag, suggested, block))
        {
            self.flush()?;
        }
        let bucket = match self.blocks.entry((tag, suggested, block)) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => {
                let existing: Option<Vec<u8>> = self
                    .db
                    .prepare_cached(
                        "SELECT packed FROM tag_blocks WHERE tag=? AND suggested=? AND block=?",
                    )?
                    .query_row(params![tag, suggested, block], |r| r.get(0))
                    .optional()?;
                entry.insert(
                    existing
                        .as_deref()
                        .map(Block::from_bytes)
                        .transpose()?
                        .unwrap_or_else(Block::empty),
                )
            }
        };
        bucket.set((key % BLOCK) as usize, add);
        Ok(())
    }

    pub fn flush(&mut self) -> Result<()> {
        let mut insert = self.db.prepare_cached("INSERT INTO tag_blocks VALUES(?,?,?,?) ON CONFLICT DO UPDATE SET packed=excluded.packed")?;
        let mut delete = self
            .db
            .prepare_cached("DELETE FROM tag_blocks WHERE tag=? AND suggested=? AND block=?")?;
        for ((tag, suggested, block), value) in std::mem::take(&mut self.blocks) {
            if let Some(bytes) = value.encode() {
                insert.execute(params![tag, suggested, block, bytes])?;
            } else {
                delete.execute(params![tag, suggested, block])?;
            }
        }
        Ok(())
    }

    fn replace(&mut self, object: i64, old_id: Option<i64>, old: &Tags, tags: &Tags) -> Result<()> {
        if old == tags {
            return Ok(());
        }
        let set_id = if tags.is_empty() {
            None
        } else {
            let packed = pack_set(tags);
            let digest = blake3::hash(&packed);
            let prior: Option<(i64, Vec<u8>)> = self
                .db
                .prepare_cached("SELECT id,packed FROM tag_sets WHERE digest=?")?
                .query_row([digest.as_bytes()], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?;
            Some(match prior {
                Some((id, bytes)) => {
                    ensure!(bytes == packed, "tag-set digest collision");
                    id
                }
                None => {
                    self.db
                        .prepare_cached(
                            "INSERT INTO tag_sets(digest,packed,confirmed) VALUES(?,?,?)",
                        )?
                        .execute(params![
                            digest.as_bytes(),
                            packed,
                            tags.values()
                                .filter(|bits| **bits & !SUGGESTED != 0)
                                .count()
                        ])?;
                    self.db.last_insert_rowid()
                }
            })
        };
        // Retractions precede additions. Shared sets are immutable.
        for (&tag, &before) in old {
            let after = tags.get(&tag).copied().unwrap_or(0);
            if before & !SUGGESTED != 0 && after & !SUGGESTED == 0 {
                self.posting(tag, false, object, false)?;
            }
            if before & SUGGESTED != 0 && after & SUGGESTED == 0 {
                self.posting(tag, true, object, false)?;
            }
        }
        for (&tag, &after) in tags {
            let before = old.get(&tag).copied().unwrap_or(0);
            if after & !SUGGESTED != 0 && before & !SUGGESTED == 0 {
                self.posting(tag, false, object, true)?;
            }
            if after & SUGGESTED != 0 && before & SUGGESTED == 0 {
                self.posting(tag, true, object, true)?;
            }
        }
        self.db
            .prepare_cached("UPDATE object_keys SET tag_set=? WHERE key=?")?
            .execute(params![set_id, object])?;
        if let Some(old) = old_id {
            self.db.prepare_cached("DELETE FROM tag_sets WHERE id=?1 AND NOT EXISTS(SELECT 1 FROM object_keys WHERE tag_set=?1)")?.execute([old])?;
        }
        Ok(())
    }

    pub fn assign(
        &mut self,
        object: &str,
        tag: i64,
        origin: u8,
        value: Option<(f64, Option<&str>)>,
    ) -> Result<i64> {
        let object = key(self.db, object)?;
        let (id, old) = read_set(self.db, object)?;
        let mut tags = old.clone();
        if let Some((confidence, evidence)) = value {
            ensure!(
                (0.0..=1.0).contains(&confidence),
                "confidence must be between zero and one"
            );
            *tags.entry(tag).or_default() |= origin;
            store_detail(self.db, object, tag, origin, confidence, evidence)?;
        } else if let Some(bits) = tags.get_mut(&tag) {
            *bits &= !origin;
            if *bits == 0 {
                tags.remove(&tag);
            }
            self.db
                .prepare_cached(
                    "DELETE FROM tag_details WHERE object_key=? AND tag=? AND origin=?",
                )?
                .execute(params![object, tag, origin])?;
        }
        if value.is_none() {
            require_file_tag(self.db, object, &tags)?;
        }
        self.replace(object, id, &old, &tags)?;
        Ok(object)
    }

    pub fn filesystem(&mut self, object: &str, ids: &[i64]) -> Result<()> {
        let object = key(self.db, object)?;
        let (id, old) = read_set(self.db, object)?;
        let mut tags = old.clone();
        tags.retain(|_, bits| {
            *bits &= !FILESYSTEM;
            *bits != 0
        });
        for &tag in ids {
            *tags.entry(tag).or_default() |= FILESYSTEM;
        }
        self.db
            .prepare_cached("DELETE FROM tag_details WHERE object_key=? AND origin=?")?
            .execute(params![object, FILESYSTEM])?;
        self.replace(object, id, &old, &tags)
    }

    pub fn invalidate_enrichment(&mut self, object: &str, previous_revision: i64) -> Result<()> {
        let key = key(self.db, object)?;
        let (id, old) = read_set(self.db, key)?;
        let mut tags = old.clone();
        tags.retain(|_, mask| {
            *mask &= !(AI | CONTEXTUAL);
            *mask != 0
        });
        if tags == old {
            return Ok(());
        }
        if tags.values().all(|mask| mask & !SUGGESTED == 0) {
            let fallback = crate::organization::dictionary_id(self.db, "file")?;
            *tags.entry(fallback).or_default() |= FILESYSTEM;
        }
        for (&tag, &mask) in &old {
            for (origin, label) in [(AI, "ai"), (CONTEXTUAL, "contextual")] {
                if mask & origin == 0 {
                    continue;
                }
                crate::registry::record(
                    self.db,
                    tag,
                    Some(key),
                    Some(label),
                    "stale",
                    "scanner",
                    Some(previous_revision),
                    None,
                )?;
            }
        }
        self.db
            .prepare_cached("DELETE FROM tag_details WHERE object_key=? AND origin IN (?,?)")?
            .execute(params![key, AI, CONTEXTUAL])?;
        self.replace(key, id, &old, &tags)
    }

    pub fn retag(&mut self, from: i64, to: Option<i64>) -> Result<()> {
        self.flush()?;
        let mut after = -1;
        loop {
            let Some(block) = self
                .db
                .prepare_cached("SELECT min(block) FROM tag_blocks WHERE tag=? AND block>?")?
                .query_row(params![from, after], |r| r.get::<_, Option<i64>>(0))?
            else {
                break;
            };
            let mut query = self
                .db
                .prepare_cached("SELECT packed FROM tag_blocks WHERE tag=? AND block=?")?;
            let records = query
                .query_map(params![from, block], |r| r.get::<_, Vec<u8>>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            drop(query);
            let mut members = Block::empty();
            for record in records {
                for (word, bits) in members.0.iter_mut().zip(Block::from_bytes(&record)?.0) {
                    *word |= bits;
                }
            }
            for slot in members.slots() {
                let object = block * BLOCK + slot as i64;
                let (id, old) = read_set(self.db, object)?;
                let Some(&bits) = old.get(&from) else {
                    anyhow::bail!("posting has no corresponding tag");
                };
                let mut tags = old.clone();
                tags.remove(&from);
                if let Some(to) = to {
                    for (origin, _) in ORIGINS {
                        if bits & origin == 0 {
                            continue;
                        }
                        let previous = detail_by_key(self.db, object, from, origin)?;
                        let destination_has_origin =
                            tags.get(&to).is_some_and(|mask| mask & origin != 0);
                        if !destination_has_origin
                            || previous.0 > detail_by_key(self.db, object, to, origin)?.0
                        {
                            store_detail(
                                self.db,
                                object,
                                to,
                                origin,
                                previous.0,
                                previous.1.as_deref(),
                            )?;
                        }
                    }
                    *tags.entry(to).or_default() |= bits;
                }
                self.db
                    .prepare_cached("DELETE FROM tag_details WHERE object_key=? AND tag=?")?
                    .execute(params![object, from])?;
                require_file_tag(self.db, object, &tags)?;
                self.replace(object, id, &old, &tags)?;
            }
            self.flush()?;
            after = block;
        }
        Ok(())
    }
}

fn require_file_tag(db: &Connection, key: i64, tags: &Tags) -> Result<()> {
    if tags.values().all(|bits| bits & !SUGGESTED == 0) {
        let is_file=db.prepare_cached("SELECT o.kind='file' FROM object_keys k JOIN objects o ON o.id=k.object WHERE k.key=?")?
            .query_row([key],|r|r.get::<_,bool>(0))?;
        ensure!(
            !is_file,
            "a file must retain at least one confirmed direct tag"
        );
    }
    Ok(())
}

pub fn migrate(db: &Connection) -> Result<()> {
    db.execute_batch(include_str!("packed.sql"))?;
    db.execute(
        "INSERT INTO object_keys(object) SELECT id FROM objects ORDER BY rowid",
        [],
    )?;
    let mut writer = Writer::new(db);
    let mut statement = db.prepare(
        "SELECT k.key,a.tag,a.origin,a.confidence,a.evidence
        FROM tag_assignments a JOIN object_keys k ON k.object=a.object
        ORDER BY a.object,a.tag,a.origin",
    )?;
    let mut rows = statement.query([])?;
    let mut current = None;
    let mut tags = Tags::new();
    while let Some(row) = rows.next()? {
        let key = row.get(0)?;
        if let Some(old) = current.filter(|previous| *previous != key) {
            writer.replace(old, None, &Tags::new(), &tags)?;
            tags.clear();
        }
        current = Some(key);
        let (tag, origin, confidence, evidence): (i64, String, f64, Option<String>) =
            (row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?);
        let origin = origin_bit(&origin)?;
        *tags.entry(tag).or_default() |= origin;
        if confidence != 1.0 || evidence.as_deref() != default_evidence(origin) {
            store_detail(db, key, tag, origin, confidence, evidence.as_deref())?;
        }
    }
    if let Some(key) = current {
        writer.replace(key, None, &Tags::new(), &tags)?;
    }
    drop(rows);
    drop(statement);
    writer.flush()?;
    db.execute_batch("DROP TABLE tag_assignments;")?;
    Ok(())
}

pub fn rebuild(db: &Connection) -> Result<()> {
    db.execute("DELETE FROM tag_blocks", [])?;
    let mut statement = db.prepare(
        "SELECT k.key,s.packed FROM object_keys k JOIN tag_sets s ON s.id=k.tag_set ORDER BY k.key",
    )?;
    let mut rows = statement.query([])?;
    let mut writer = Writer::new(db);
    while let Some(row) = rows.next()? {
        let key = row.get(0)?;
        for (tag, origins) in unpack_set(row.get_ref(1)?.as_blob()?)? {
            if origins & !SUGGESTED != 0 {
                writer.posting(tag, false, key, true)?;
            }
            if origins & SUGGESTED != 0 {
                writer.posting(tag, true, key, true)?;
            }
        }
    }
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codecs_round_trip_and_choose_each_representation() -> Result<()> {
        for (expected, members) in [
            (b'L', vec![2, 9, 128, 4095]),
            (b'R', (800..1500).collect()),
            (b'B', (0..4096).step_by(2).collect()),
            (b'A', (0..4096).collect()),
        ] {
            let mut block = Block::empty();
            for &slot in &members {
                block.set(slot, true);
            }
            let packed = block.encode().unwrap();
            assert_eq!(packed[0], expected);
            assert_eq!(
                Block::from_bytes(&packed)?.slots().collect::<Vec<_>>(),
                members
            );
        }
        let tags = Tags::from([
            (7, USER),
            (12, USER | SUGGESTED),
            (85, FILESYSTEM),
            (1 << 50, USER),
        ]);
        assert_eq!(unpack_set(&pack_set(&tags))?, tags);
        let runs = (1..=1000)
            .map(|tag| (tag, if tag == 5 { FILESYSTEM } else { USER }))
            .collect::<Tags>();
        let packed = pack_set(&runs);
        assert_eq!(packed[0], SET_RUNS);
        assert!(packed.len() < 16);
        assert_eq!(unpack_set(&packed)?, runs);
        let gaps = (1..=1000)
            .map(|tag| (tag * 11, if tag == 5 { FILESYSTEM } else { USER }))
            .collect::<Tags>();
        let packed = pack_set(&gaps);
        assert_eq!(packed[0], SET_GAPS);
        assert_eq!(unpack_set(&packed)?, gaps);
        assert!(unpack_set(&[USER, 128]).is_err());
        assert!(Block::from_bytes(&[b'R', 0, 0]).is_err());
        Ok(())
    }
}

#[cfg(test)]
#[path = "kernel_storage_test.rs"]
mod kernel_storage_test;
