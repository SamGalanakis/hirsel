//! Git pack files (GIT-9, the limits of GIT-7). A pack is read as it arrives: the header's
//! count, each object's size, a delta's result size and the delta depth are checked before
//! the next byte is asked for, so a hostile response is refused at the bound it passes and
//! never read to its end. Every object is then hashed, deltas base first, and its id is the
//! hash, never a name the server gave.

use super::Failure;
use super::hash::{Format, Hasher, Oid, object_id};
use miniz_oxide::inflate::stream::{InflateState, inflate};
use miniz_oxide::{DataFormat, MZError, MZFlush, MZStatus};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

pub(crate) const OBJECT_COUNT_LIMIT: u64 = 4_194_304;
pub(crate) const OBJECT_LIMIT: u64 = 512 << 20;
pub(crate) const TOTAL_LIMIT: u64 = 4 << 30;
pub(crate) const DEPTH_LIMIT: u32 = 4095;

/// The bytes of a pack as a response delivers them.
pub(crate) trait Incoming {
    fn next(&mut self) -> Result<Option<Vec<u8>>, Failure>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Kind {
    Commit,
    Tree,
    Blob,
    Tag,
}

impl Kind {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Commit => "commit",
            Self::Tree => "tree",
            Self::Blob => "blob",
            Self::Tag => "tag",
        }
    }
}

enum Base {
    Whole(Kind),
    Offset(usize),
    Id(Oid),
}

struct Entry {
    offset: usize,
    data: usize,
    /// The inflated length of the entry's own data.
    size: u64,
    /// The object's size: `size`, or a delta's result size.
    result: u64,
    base: Base,
    depth: u32,
}

pub(crate) struct Pack {
    format: Format,
    bytes: Vec<u8>,
    entries: Vec<Entry>,
    /// Each entry's object type, known once its base is.
    kinds: Vec<Kind>,
    ids: HashMap<Oid, usize>,
    cache: Mutex<Cache>,
}

struct Cache {
    objects: HashMap<usize, Arc<Vec<u8>>>,
    order: VecDeque<usize>,
    bytes: usize,
    budget: usize,
}

impl Cache {
    fn new(entries: &[Entry]) -> Self {
        let limit = hirsel_guard::limit().unwrap_or(hirsel_guard::DEFAULT_LIMIT);
        let used = hirsel_guard::data_used()
            .or_else(|| Some(crate::host::resident_bytes()).filter(|&bytes| bytes != 0));
        // Applying a delta holds the base, instructions and result at once.
        // Give the memo a share of the space left after this scratch, leaving
        // room for the object index, snapshot and other packs in the command.
        let largest = entries
            .iter()
            .map(|entry| entry.size.max(entry.result))
            .max()
            .unwrap_or(0);
        let budget = used.map_or(0, |used| limit.saturating_sub(used + 3 * largest) / 4);
        Self {
            objects: HashMap::new(),
            order: VecDeque::new(),
            bytes: 0,
            budget: budget as usize,
        }
    }

    fn insert(&mut self, index: usize, object: Arc<Vec<u8>>) {
        let charge = |object: &Vec<u8>| {
            object.capacity() + size_of::<Vec<u8>>() + size_of::<(usize, Arc<Vec<u8>>, usize)>()
        };
        let bytes = charge(&object);
        if bytes > self.budget || self.objects.contains_key(&index) {
            return;
        }
        while self.bytes > self.budget - bytes {
            let old = self.order.pop_front().expect("a charged cached object");
            self.bytes -= charge(&self.objects.remove(&old).expect("a cached object"));
        }
        self.bytes += bytes;
        self.objects.insert(index, object);
        self.order.push_back(index);
    }
}

fn bad(text: impl Into<String>) -> Failure {
    Failure::Pack(text.into())
}

fn over(limit: &str) -> Failure {
    bad(format!("over the limit of {limit}"))
}

struct Input<'a> {
    incoming: &'a mut dyn Incoming,
    bytes: Vec<u8>,
    ended: bool,
}

impl Input<'_> {
    /// Pulls chunks until `upto` bytes are held; false when the response ended first.
    fn fill(&mut self, upto: usize) -> Result<bool, Failure> {
        while self.bytes.len() < upto {
            if self.ended {
                return Ok(false);
            }
            match self.incoming.next()? {
                Some(chunk) => self.bytes.extend_from_slice(&chunk),
                None => self.ended = true,
            }
        }
        Ok(true)
    }

    fn byte(&mut self, pos: &mut usize) -> Result<u8, Failure> {
        if !self.fill(*pos + 1)? {
            return Err(bad(format!("truncated at {}", self.bytes.len())));
        }
        *pos += 1;
        Ok(self.bytes[*pos - 1])
    }
}

/// Reads a delta's two leading sizes, if `head` holds them both.
fn delta_sizes(head: &[u8]) -> Option<(u64, u64, usize)> {
    let mut pos = 0;
    let mut varint = || {
        let mut value = 0u64;
        let mut shift = 0;
        loop {
            let byte = *head.get(pos)?;
            pos += 1;
            if shift > 63 {
                return Some(u64::MAX);
            }
            value |= u64::from(byte & 0x7f) << shift;
            shift += 7;
            if byte & 0x80 == 0 {
                return Some(value);
            }
        }
    };
    let base = varint()?;
    let result = varint()?;
    Some((base, result, pos))
}

pub(crate) fn parse(format: Format, incoming: &mut dyn Incoming) -> Result<Pack, Failure> {
    let mut input = Input {
        incoming,
        bytes: Vec::new(),
        ended: false,
    };
    if !input.fill(12)? {
        if input.bytes.len() >= 4 && &input.bytes[..4] != b"PACK" {
            return Err(bad("bad header"));
        }
        return Err(bad(format!("truncated at {}", input.bytes.len())));
    }
    let header = &input.bytes[..12];
    let version = u32::from_be_bytes(header[4..8].try_into().unwrap());
    if &header[..4] != b"PACK" || !matches!(version, 2 | 3) {
        return Err(bad("bad header"));
    }
    let count = u64::from(u32::from_be_bytes(header[8..12].try_into().unwrap()));
    if count > OBJECT_COUNT_LIMIT {
        return Err(over(&format!("{OBJECT_COUNT_LIMIT} objects")));
    }
    let mut pos = 12;
    let mut entries: Vec<Entry> = Vec::with_capacity(count.min(1 << 16) as usize);
    let mut total = 0u64;
    let mut state = InflateState::new_boxed(DataFormat::Zlib);
    let mut out = vec![0u8; 1 << 16];
    let add_size = |total: &mut u64, size: u64| {
        if size > OBJECT_LIMIT {
            return Err(over("512 MiB object"));
        }
        *total += size;
        if *total > TOTAL_LIMIT {
            return Err(over("4 GiB inflated"));
        }
        Ok(())
    };
    for _ in 0..count {
        let offset = pos;
        let mut byte = input.byte(&mut pos)?;
        let kind = (byte >> 4) & 7;
        let mut size = u64::from(byte & 15);
        let mut shift = 4;
        while byte & 0x80 != 0 {
            byte = input.byte(&mut pos)?;
            if shift > 57 {
                return Err(over("512 MiB object"));
            }
            size |= u64::from(byte & 0x7f) << shift;
            shift += 7;
        }
        let (base, depth) = match kind {
            1 => (Base::Whole(Kind::Commit), 0),
            2 => (Base::Whole(Kind::Tree), 0),
            3 => (Base::Whole(Kind::Blob), 0),
            4 => (Base::Whole(Kind::Tag), 0),
            6 => {
                let mut byte = input.byte(&mut pos)?;
                let mut distance = u64::from(byte & 0x7f);
                while byte & 0x80 != 0 {
                    byte = input.byte(&mut pos)?;
                    distance = distance
                        .checked_add(1)
                        .and_then(|d| d.checked_mul(128))
                        .map(|d| d | u64::from(byte & 0x7f))
                        .unwrap_or(u64::MAX);
                }
                let at = offset as i128 - i128::from(distance);
                let index = (distance != 0)
                    .then(|| entries.binary_search_by_key(&at, |e| e.offset as i128).ok())
                    .flatten()
                    .ok_or_else(|| bad(format!("delta base at {at} is not an earlier entry")))?;
                let depth = entries[index].depth + 1;
                if depth > DEPTH_LIMIT {
                    return Err(over(&format!("{DEPTH_LIMIT} delta links")));
                }
                (Base::Offset(index), depth)
            }
            7 => {
                let end = pos + format.len();
                if !input.fill(end)? {
                    return Err(bad(format!("truncated at {}", input.bytes.len())));
                }
                let id = Oid::from_bytes(&input.bytes[pos..end]);
                pos = end;
                (Base::Id(id), 0)
            }
            _ => return Err(bad(format!("object at {offset} has an unknown type"))),
        };
        let delta = !matches!(base, Base::Whole(_));
        if size > OBJECT_LIMIT {
            return Err(over("512 MiB object"));
        }
        let mut result = None;
        if !delta {
            add_size(&mut total, size)?;
            result = Some(size);
        }
        let data = pos;
        state.reset(DataFormat::Zlib);
        let mut written = 0u64;
        let mut head = Vec::new();
        loop {
            if pos == input.bytes.len() && !input.fill(pos + 1)? {
                return Err(bad(format!("truncated at {pos}")));
            }
            let step = inflate(&mut state, &input.bytes[pos..], &mut out, MZFlush::None);
            pos += step.bytes_consumed;
            if delta && result.is_none() {
                let take = (20 - head.len()).min(step.bytes_written);
                head.extend_from_slice(&out[..take]);
                if let Some((_, sizes, _)) = delta_sizes(&head) {
                    add_size(&mut total, sizes)?;
                    result = Some(sizes);
                }
            }
            written += step.bytes_written as u64;
            if written > OBJECT_LIMIT {
                return Err(over("512 MiB object"));
            }
            match step.status {
                Ok(MZStatus::StreamEnd) => break,
                Ok(_) => {}
                Err(MZError::Buf) if step.bytes_consumed == 0 && step.bytes_written == 0 => {
                    if !input.fill(input.bytes.len() + 1)? {
                        return Err(bad(format!("truncated at {}", input.bytes.len())));
                    }
                }
                Err(MZError::Buf) => {}
                Err(_) => return Err(bad(format!("object at {offset} does not inflate"))),
            }
        }
        if written != size {
            return Err(bad(format!(
                "object at {offset} inflates to {written} bytes, not {size}"
            )));
        }
        let result =
            result.ok_or_else(|| bad(format!("delta at {offset} does not apply to its base")))?;
        entries.push(Entry {
            offset,
            data,
            size,
            result,
            base,
            depth,
        });
    }
    let end = pos + format.len();
    if !input.fill(end)? {
        return Err(bad(format!("truncated at {}", input.bytes.len())));
    }
    let mut hasher = Hasher::new(format);
    hasher.update(&input.bytes[..pos]);
    let sum = hasher.finish().unwrap_or_else(|collision| collision.0);
    if sum.as_bytes() != &input.bytes[pos..end] {
        return Err(bad("checksum mismatch"));
    }
    if input.bytes.len() > end {
        return Err(bad(format!("trailing bytes at {end}")));
    }
    while let Some(chunk) = input.incoming.next()? {
        if !chunk.is_empty() {
            return Err(bad(format!("trailing bytes at {end}")));
        }
    }
    let cache = Mutex::new(Cache::new(&entries));
    let mut pack = Pack {
        format,
        bytes: input.bytes,
        entries,
        kinds: Vec::new(),
        ids: HashMap::new(),
        cache,
    };
    pack.identify()?;
    Ok(pack)
}

fn apply(base: &[u8], delta: &[u8], offset: usize) -> Result<Vec<u8>, Failure> {
    let refuse = || bad(format!("delta at {offset} does not apply to its base"));
    let (base_size, result_size, mut pos) = delta_sizes(delta).ok_or_else(refuse)?;
    if base_size != base.len() as u64 || result_size > OBJECT_LIMIT {
        return Err(refuse());
    }
    let mut out = Vec::with_capacity(result_size as usize);
    while pos < delta.len() {
        let op = delta[pos];
        pos += 1;
        if op & 0x80 != 0 {
            let mut field = |bits: u8, shift: u32, value: &mut u64| -> Result<(), Failure> {
                if op & bits != 0 {
                    *value |= u64::from(*delta.get(pos).ok_or_else(refuse)?) << shift;
                    pos += 1;
                }
                Ok(())
            };
            let (mut at, mut len) = (0u64, 0u64);
            for (i, bit) in [1u8, 2, 4, 8].into_iter().enumerate() {
                field(bit, 8 * i as u32, &mut at)?;
            }
            for (i, bit) in [16u8, 32, 64].into_iter().enumerate() {
                field(bit, 8 * i as u32, &mut len)?;
            }
            if len == 0 {
                len = 0x10000;
            }
            let end = at.checked_add(len).filter(|end| *end <= base.len() as u64);
            let end = end.ok_or_else(refuse)?;
            if out.len() as u64 + len > result_size {
                return Err(refuse());
            }
            out.extend_from_slice(&base[at as usize..end as usize]);
        } else if op != 0 {
            let insert = delta.get(pos..pos + op as usize).ok_or_else(refuse)?;
            if out.len() + insert.len() > result_size as usize {
                return Err(refuse());
            }
            out.extend_from_slice(insert);
            pos += op as usize;
        } else {
            return Err(refuse());
        }
    }
    if out.len() as u64 != result_size {
        return Err(refuse());
    }
    Ok(out)
}

impl Pack {
    fn inflate(&self, index: usize) -> Vec<u8> {
        let entry = &self.entries[index];
        let mut out = vec![0u8; entry.size as usize];
        let mut state = InflateState::new_boxed(DataFormat::Zlib);
        let step = inflate(
            &mut state,
            &self.bytes[entry.data..],
            &mut out,
            MZFlush::Finish,
        );
        debug_assert_eq!(step.bytes_written as u64, entry.size);
        out
    }

    /// The bytes of entry `index`, base first through its delta chain.
    fn materialize(&self, index: usize) -> Result<Arc<Vec<u8>>, Failure> {
        let mut chain = Vec::new();
        let mut at = index;
        let start = loop {
            if let Some(object) = self.cache.lock().unwrap().objects.get(&at) {
                break object.clone();
            }
            match &self.entries[at].base {
                Base::Whole(_) => break Arc::new(self.inflate(at)),
                Base::Offset(base) => {
                    chain.push(at);
                    at = *base;
                }
                Base::Id(id) => {
                    chain.push(at);
                    at = self.ids[id];
                }
            }
        };
        let mut object = start;
        for &delta in chain.iter().rev() {
            let bytes = apply(&object, &self.inflate(delta), self.entries[delta].offset)?;
            object = Arc::new(bytes);
            self.cache.lock().unwrap().insert(delta, object.clone());
        }
        Ok(object)
    }

    /// Hashes every object, each delta after its base, and refuses a missing base, a chain
    /// over the depth limit and a SHA-1 collision.
    fn identify(&mut self) -> Result<(), Failure> {
        let count = self.entries.len();
        self.kinds = vec![Kind::Blob; count];
        let mut done = vec![false; count];
        let mut by_offset: HashMap<usize, Vec<usize>> = HashMap::new();
        let mut by_id: HashMap<Oid, Vec<usize>> = HashMap::new();
        let mut ready = VecDeque::new();
        for (index, entry) in self.entries.iter().enumerate() {
            match &entry.base {
                Base::Whole(_) => ready.push_back(index),
                Base::Offset(base) => by_offset.entry(*base).or_default().push(index),
                Base::Id(id) => by_id.entry(*id).or_default().push(index),
            }
        }
        while let Some(index) = ready.pop_front() {
            let kind = match &self.entries[index].base {
                Base::Whole(kind) => *kind,
                Base::Offset(base) => self.kinds[*base],
                Base::Id(id) => {
                    let base = self.ids[id];
                    let depth = self.entries[base].depth + 1;
                    if depth > DEPTH_LIMIT {
                        return Err(over(&format!("{DEPTH_LIMIT} delta links")));
                    }
                    self.entries[index].depth = depth;
                    self.kinds[base]
                }
            };
            let object = self.materialize(index)?;
            let id = object_id(self.format, kind.name(), &object).map_err(|collision| {
                bad(format!(
                    "object {} is a SHA-1 collision attack",
                    collision.0
                ))
            })?;
            self.kinds[index] = kind;
            done[index] = true;
            let first = !self.ids.contains_key(&id);
            self.ids.entry(id).or_insert(index);
            ready.extend(by_offset.remove(&index).into_iter().flatten());
            if first {
                ready.extend(by_id.remove(&id).into_iter().flatten());
            }
        }
        if let Some(first) = done.iter().position(|done| !done) {
            let mut at = first;
            loop {
                match &self.entries[at].base {
                    Base::Offset(base) => at = *base,
                    Base::Id(id) => {
                        return Err(bad(format!("delta base {id} is not in the pack")));
                    }
                    Base::Whole(_) => unreachable!("a whole object always resolves"),
                }
            }
        }
        Ok(())
    }

    pub(crate) fn format(&self) -> Format {
        self.format
    }

    /// The kind and size of object `id`, without building it.
    pub(crate) fn header(&self, id: &Oid) -> Option<(Kind, u64)> {
        let index = *self.ids.get(id)?;
        Some((self.kinds[index], self.entries[index].result))
    }

    /// Object `id` of kind `kind`, refused as a bad pack when it is absent or another kind.
    pub(crate) fn read(&self, id: &Oid, kind: Kind) -> Result<Arc<Vec<u8>>, Failure> {
        let index = *self
            .ids
            .get(id)
            .ok_or_else(|| bad(format!("object {id} is not in the pack")))?;
        let found = self.kinds[index];
        if found != kind {
            return Err(bad(format!(
                "object {id} is a {}, not a {}",
                found.name(),
                kind.name()
            )));
        }
        self.materialize(index)
    }
}
