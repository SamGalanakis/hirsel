//! A helper unit's recording (docs/recording.md, "Unit recordings", slice
//! U2). Each helper that a recorded unit spawns gets its own cell and its own
//! recorder, writing its own stream of the recording's file: the header block
//! with its parent's stream number, its spawn index and its entry, then the
//! frames of its own operations, then its `END`.
//!
//! A capability its entry holds is designated again on its cell, so the
//! helper's operations on it reach its own stream and never its parent's.
//! An entry holding what this slice does not write (a handle other than a
//! capability, a closure, a capability without a snapshot) gives the helper
//! a stream truncated at 0 and the reason in the launcher's report.
use super::schedule::Group;
use super::*;
use crate::causes::{CANCELLED, CAPACITY, Cause, FAIL};
use crate::units_lang::Cross;
use hirsel_core_layout::cross::{CAPABILITY, CLIENT, FUNCTION, SEQ, SHARED};
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};

/// Stream numbers: unique in the process, so in each recording's file.
static NEXT_STREAM: AtomicU32 = AtomicU32::new(1);

/// A helper's recording: its tape group, and its own designation of each
/// capability word its entry holds. A helper whose stream is cut at its
/// entry has no group: each capability word designates the source the
/// recording substituted, so the helper runs as it would unrecorded.
pub(crate) struct Helper {
    group: Option<Group>,
    /// The stream number its parent's `UNIT_SPAWN` delivers, cut or not.
    stream: u32,
    /// The parts of its `UNIT_SPAWN` arguments, one after another: the code
    /// descriptor, `n` and `x` in canonical form. None when its entry is cut.
    spawn_args: Option<Vec<u8>>,
    grants: Vec<(u64, u64)>,
    sources: Vec<(u64, Box<Provider>, u64)>,
}

impl Helper {
    pub(crate) fn group(&self) -> Option<Group> {
        self.group
    }

    pub(crate) fn stream(&self) -> u32 {
        self.stream
    }

    pub(crate) fn spawn_args(&self) -> Option<&[u8]> {
        self.spawn_args.as_deref()
    }

    /// Designate the entry's capabilities in the helper's registry: on its
    /// cell, or on their sources when its stream is cut.
    pub(crate) fn designate(&self, registry: &mut crate::host_file::Registry) {
        if let Some(group) = self.group {
            for &(word, handle) in &self.grants {
                registry.designate_in_unit(word, &group.cell.provider, handle);
            }
        }
        for (word, provider, handle) in &self.sources {
            registry.designate_in_unit(*word, &**provider, *handle);
        }
    }

    /// Write the helper's `END` as `join` delivers it: its result, or its
    /// causes. Its scheduler has stopped, so no other frame follows. Returns
    /// the outcome kind and body for the parent's end `TAKE`, or why it has none.
    /// # Safety
    /// `result` holds an initialized `R` of `bytes` bytes, with `owners`,
    /// when `failed` is false.
    pub(crate) unsafe fn end(
        &self,
        failed: bool,
        causes: &[&Cause],
        result: *const u8,
        bytes: u64,
        owners: &[[u64; 4]],
    ) -> Result<(u8, Vec<u8>), &'static str> {
        let body = if failed {
            let kind = if causes.first().is_some_and(|c| c.kind() == CANCELLED) {
                2
            } else {
                1
            };
            Ok((kind, encode_causes(causes)))
        } else {
            let r = Cross {
                bytes,
                owners: owners.as_ptr(),
                count: owners.len() as u64,
                at: 0,
                len: bytes,
            };
            // SAFETY: forwarded.
            unsafe { value(result, &r, None, &mut |_, _| Err(RESULT_HANDLE)) }.map(|v| (0, v))
        };
        let body = body.and_then(|(kind, body)| {
            HelperEnding::decode(kind, body.clone())
                .map(|ending| (kind, body, ending))
                .map_err(|_| NO_CAUSE)
        });
        let Some(group) = self.group else {
            return body.map(|(kind, body, _)| (kind, body));
        };
        let mut state = group.cell.state.lock().unwrap();
        if state.generation != group.generation {
            return body.map(|(kind, body, _)| (kind, body));
        }
        let Some(live) = state.live.as_mut() else {
            return body.map(|(kind, body, _)| (kind, body));
        };
        let rec = live.recorder.as_mut().unwrap();
        let out = match body {
            Ok((kind, body, ending)) => {
                let gap = live.schedule.events - live.schedule.last;
                live.schedule.last = live.schedule.events;
                let _ = rec.frame(&Frame::HelperEnd { gap, ending });
                Ok((kind, body))
            }
            Err(why) => {
                rec.cut(why);
                Err(why)
            }
        };
        drop(state);
        let _ = finish(group.cell, group.generation, Seal::Unsealed);
        out
    }
}

const NO_CAUSE: &str = "the helper failed without a cause its END can carry";
const RESULT_HANDLE: &str =
    "the result holds a capability or client, whose snapshot a helper's END does not write";

/// The `END` a helper's stream ends with when its unit deadlocks.
pub(super) fn deadlock(gap: u64, events: u64) -> Frame {
    let mut body = Vec::new();
    put_varint(&mut body, events);
    Frame::HelperEnd {
        gap,
        ending: HelperEnding::decode(3, body).expect("a well-formed Deadlock END"),
    }
}

impl Recorder {
    /// End this stream at its next frame for `why`: it records nothing more.
    pub(super) fn cut(&mut self, why: &'static str) {
        if let Sink::Writer(stream) = &self.sink
            && !self.truncated
            && self.failed.is_none()
        {
            writer::cut(
                stream.path().to_vec(),
                self.written_frames,
                writer::Cut::Entry(why),
            );
        }
        self.truncated = true;
    }
}

/// Start the recording of a helper that the running task spawns: `input` is
/// its wrapped input (`f` and `x`) as `x` describes it. None when the task's
/// unit is not recording.
/// # Safety
/// `context` is the spawner's, and `input` holds its initialized input.
pub(crate) unsafe fn spawn(
    context: *const crate::Context,
    n: u64,
    steps: Option<u64>,
    input: *const u8,
    x: &Cross,
) -> Option<Helper> {
    let spawner = crate::tasks::current_group()?;
    // Resolve each capability word before any cell is locked: the registry's
    // resolution may itself reach the spawner's cell.
    // The wrapped input's first row is `f`'s, outside `x`'s span.
    let held = Cross {
        owners: x.owners()[1.min(x.owners().len())..].as_ptr(),
        count: x.count.saturating_sub(1),
        ..*x
    };
    // SAFETY: the input is initialized as `x` describes it.
    let words = unsafe { capability_words(input, &held, None) };
    let resolved: Vec<_> = words
        .iter()
        .map(|&word| {
            // SAFETY: the spawner's context resolves its own capability words.
            unsafe {
                (
                    word,
                    crate::entry::hirsel_core_capability_provider(context, word),
                    crate::entry::hirsel_core_capability_handle(context, word),
                )
            }
        })
        .collect();
    // SAFETY: the spawner's context and its host files are live.
    let files = unsafe { (*context).host_files };
    let granted = if words.is_empty() || files.is_null() {
        Vec::new()
    } else {
        // SAFETY: as above.
        units::item_grants::resolve(unsafe { &(*files).registry }, &words)
    };
    let cell = cell();
    let mut fresh = cell.state.lock().unwrap();
    let generation = fresh
        .generation
        .checked_add(1)
        .unwrap_or_else(|| crate::process::stop(b"TapeSlots"));
    let mut state = spawner.cell.state.lock().unwrap();
    let current = state.generation == spawner.generation;
    let parent = state
        .live
        .as_mut()
        .filter(|live| current && live.mode == Mode::Record);
    let Some(parent) = parent else {
        drop(fresh);
        super::FREE.with_borrow_mut(|f| f.push(cell));
        return None;
    };
    let from = parent.recorder.as_mut().unwrap();
    let index = from.spawns;
    from.spawns += 1;
    let tape_id = from.tape_id;
    let parent_stream = from.stream;
    let stream = match &from.sink {
        Sink::Writer(stream) => Ok(stream.helper(index)),
        _ => Err((
            vec![index],
            "the recording is not written by the tape writer",
        )),
    };
    let mut live = Live {
        tape_id,
        waiting: BTreeMap::new(),
        pending: BTreeMap::new(),
        schedule: schedule::Schedule::new(parent.schedule.chooser()),
        mode: Mode::Record,
        fault: None,
        replay_abort: None,
        objects: vec![],
        replay_net_rules: BTreeMap::new(),
        takes: units::Takes::default(),
        exec_rules: BTreeMap::new(),
        typed_resources: BTreeMap::new(),
        udp_retirements: BTreeMap::new(),
        clocks: BTreeMap::new(),
        grants: BTreeMap::new(),
        recorder: None,
        replayer: None,
    };
    let mut grants = Vec::new();
    // SAFETY: forwarded; each capability word designates a grant of the run.
    let body = unsafe {
        entry(context, n, steps, input, x, &mut |kind, word| {
            if kind == CLIENT {
                let ordinal = live.unit_object();
                live.takes.clients.insert(word as usize, ordinal);
                return units::client_snapshot(word, ordinal);
            }
            if kind == SHARED {
                let ordinal = live.unit_object();
                live.takes.nodes.insert(word as usize, ordinal);
                return units::shared_snapshot(word, ordinal);
            }
            units::item_grants::arrived(&mut live, generation, &granted, word, &mut grants)
        })
    };
    drop(state);
    let (stream, body) = match (stream, body) {
        (Ok(stream), Ok(body)) => (stream, body),
        (Ok(stream), Err(why)) => {
            writer::cut(stream.path().to_vec(), 0, writer::Cut::Entry(why));
            drop(fresh);
            return Some(cut(cell, &resolved));
        }
        (Err((path, why)), _) => {
            writer::cut(path, 0, writer::Cut::Entry(why));
            drop(fresh);
            return Some(cut(cell, &resolved));
        }
    };
    let mut recorder = Recorder {
        sink: Sink::Writer(stream),
        stream: NEXT_STREAM.fetch_add(1, Relaxed),
        spawns: 0,
        truncated: false,
        tape_id,
        encoder: Encoder::default(),
        buffer: Recorder::fresh(Vec::new()),
        seq: 0,
        chooses: [None, None],
        repeat: 0,
        frames: 0,
        written_frames: 0,
        failed: None,
    };
    let stream_number = recorder.stream;
    let entry = Entry::decode(parent_stream, index, body).expect("a well-formed entry");
    let spawn_args = {
        let b = entry.body();
        let mut args = b.descriptor.to_vec();
        args.extend_from_slice(&b.n.to_le_bytes());
        args.extend(b.value.canonical());
        args
    };
    let rest = entry.encode(&mut recorder.buffer);
    let _ = recorder.close(0);
    for chunk in rest {
        format::put_data(&mut recorder.buffer, chunk);
        recorder.frames += 1;
        if recorder.full() {
            let _ = recorder.close(0);
        }
    }
    live.recorder = Some(recorder);
    fresh.generation = generation;
    fresh.live = Some(live);
    drop(fresh);
    Some(Helper {
        group: Some(Group::root(cell, generation)),
        stream: stream_number,
        spawn_args: Some(spawn_args),
        grants,
        sources: vec![],
    })
}

/// A helper whose stream is cut at its entry: its cell goes back unused, and
/// each capability word the recording substituted designates its source.
fn cut(cell: &'static Cell, resolved: &[(u64, *const Provider, u64)]) -> Helper {
    super::FREE.with_borrow_mut(|f| f.push(cell));
    let sources = resolved
        .iter()
        .filter_map(|&(word, provider, handle)| {
            // SAFETY: a resolved provider points at a live Provider.
            let provider = unsafe { &*provider };
            if !dispatch::owns(provider) {
                return None;
            }
            let (source, at) = dispatch::source(provider, handle)?;
            Some((word, Box::new(source), at))
        })
        .collect();
    Helper {
        group: None,
        stream: NEXT_STREAM.fetch_add(1, Relaxed),
        spawn_args: None,
        grants: vec![],
        sources,
    }
}

/// The joined entry: `f`'s identity, `n`, the bound and `x` as a value.
unsafe fn entry(
    context: *const crate::Context,
    n: u64,
    steps: Option<u64>,
    input: *const u8,
    x: &Cross,
    handle: &mut dyn FnMut(u64, u64) -> Result<Vec<u8>, &'static str>,
) -> Result<Vec<u8>, &'static str> {
    let _ = context;
    let owners = x.owners();
    // The wrapped input's first field is `f`, so its row comes first.
    let Some(&[f, FUNCTION, ..]) = owners.first() else {
        return Err("the entry has no code word");
    };
    // SAFETY: the input holds `f`'s code word at its row.
    let code = unsafe { input.add(f as usize).cast::<u64>().read_unaligned() };
    let identity = run::identities::identity_of(code).ok_or(NO_IDENTITY)?;
    let mut body = Vec::new();
    put_varint(&mut body, identity.len() as u64);
    body.extend_from_slice(identity);
    body.extend_from_slice(&n.to_le_bytes());
    match steps {
        None => body.push(0),
        Some(bound) => {
            body.push(1);
            body.extend_from_slice(&bound.to_le_bytes());
        }
    }
    let x = Cross {
        owners: owners[1..].as_ptr(),
        count: x.count - 1,
        ..*x
    };
    // SAFETY: forwarded.
    body.extend(unsafe { value(input, &x, None, handle) }?);
    Ok(body)
}

const HANDLE: &str = "the value holds a socket handle, whose snapshot this build does not write";

/// The capability words of a value, in the order it holds them, nested ones
/// included: what `item_grants::resolve` reads before any cell is locked.
/// # Safety
/// As for `value`.
pub(super) unsafe fn capability_words(
    base: *const u8,
    x: &Cross,
    content: Option<*const u8>,
) -> Vec<u64> {
    let mut words = Vec::new();
    let mut collect = |kind, word| {
        if kind == CAPABILITY {
            words.push(word);
        }
        Ok(Vec::new())
    };
    // SAFETY: forwarded. A value that cannot be written is cut where it is
    // written, so its error here is not this walk's.
    let _ = unsafe { value(base, x, content, &mut collect) };
    words
}
const NO_IDENTITY: &str = "the entry's function has no code identity (a closure)";

/// A value that crosses units: its plain bytes with each owner word zeroed,
/// then a part per owner in ascending offset order. A packed value (an item
/// in a connection) passes `content`, its sequences' elements one after
/// another in owner-row order; otherwise each sequence word points at them.
pub(super) unsafe fn value(
    base: *const u8,
    x: &Cross,
    content: Option<*const u8>,
    handle: &mut dyn FnMut(u64, u64) -> Result<Vec<u8>, &'static str>,
) -> Result<Vec<u8>, &'static str> {
    // SAFETY: the value holds `x.len` initialized bytes at `x.at`.
    let mut plain =
        unsafe { std::slice::from_raw_parts(base.add(x.at as usize), x.len as usize) }.to_vec();
    let mut packed = BTreeMap::new();
    if let Some(content) = content {
        let mut at = 0usize;
        for &[offset, kind, element, _] in x.owners() {
            if kind == SEQ {
                // SAFETY: a sequence carrier's length word is initialized.
                let len = unsafe { base.add(offset as usize + 8).cast::<u64>().read_unaligned() };
                // SAFETY: the packed content holds each sequence's elements in row order.
                packed.insert(offset, unsafe { content.add(at) });
                at += (len * element) as usize;
            }
        }
    }
    let mut owners = x.owners().to_vec();
    owners.sort_by_key(|o| o[0]);
    let mut parts = Vec::new();
    for &[at, kind, element, _] in &owners {
        let word = |i: u64| {
            // SAFETY: an owner row names initialized words of the value.
            unsafe {
                base.add((at + 8 * i) as usize)
                    .cast::<u64>()
                    .read_unaligned()
            }
        };
        let words = if kind == SEQ { 3 } else { 1 };
        let start = (at - x.at) as usize;
        plain[start..start + 8 * words].fill(0);
        match kind {
            SEQ => {
                let bytes = (word(1) * element) as usize;
                parts.push(0);
                put_varint(&mut parts, bytes as u64);
                if bytes > 0 {
                    let from = packed.get(&at).copied().unwrap_or(word(0) as *const u8);
                    // SAFETY: a sequence carrier, or the packed content, names
                    // its initialized elements.
                    let elements = unsafe { std::slice::from_raw_parts(from, bytes) };
                    parts.extend_from_slice(elements);
                }
            }
            CAPABILITY | CLIENT => {
                parts.push(1);
                parts.extend(handle(kind, word(0))?);
            }
            SHARED => {
                // The snapshot's head, then the payload as a value: a holder
                // points at the node's count, and the payload follows it.
                parts.push(1);
                parts.extend(handle(kind, word(0))?);
                let (bytes, rows) = units::node_layout(word(0)).ok_or(units::UNNAMED_NODE)?;
                let payload = Cross {
                    bytes,
                    owners: rows.as_ptr(),
                    count: rows.len() as u64,
                    at: 0,
                    len: bytes,
                };
                // SAFETY: a live holder pins its node, whose payload is a
                // live value laid out as its owner rows say.
                parts.extend(unsafe { value((word(0) + 8) as *const u8, &payload, None, handle) }?);
            }
            FUNCTION => {
                let identity = run::identities::identity_of(word(0)).ok_or(NO_IDENTITY)?;
                parts.push(2);
                put_varint(&mut parts, identity.len() as u64);
                parts.extend_from_slice(identity);
            }
            _ => return Err(HANDLE),
        }
    }
    let mut out = Vec::with_capacity(plain.len() + parts.len() + 20);
    put_varint(&mut out, plain.len() as u64);
    out.extend_from_slice(&plain);
    put_varint(&mut out, owners.len() as u64);
    out.extend_from_slice(&parts);
    Ok(out)
}

/// Causes as a helper `END` writes them: each one's kind, site and detail.
fn encode_causes(causes: &[&Cause]) -> Vec<u8> {
    let mut out = Vec::new();
    put_varint(&mut out, causes.len() as u64);
    for cause in causes {
        out.push(cause.kind());
        out.extend_from_slice(&cause.site.to_le_bytes());
        match cause.kind() {
            FAIL => {
                let m = &cause.message;
                let text = if m.len == 0 {
                    &[][..]
                } else {
                    // SAFETY: a live Fail cause owns its message bytes.
                    unsafe { std::slice::from_raw_parts(m.data, m.len as usize) }
                };
                put_varint(&mut out, text.len() as u64);
                out.extend_from_slice(text);
            }
            CAPACITY => {
                out.extend_from_slice(&(cause.message.data as u64).to_le_bytes());
                out.extend_from_slice(&cause.message.len.to_le_bytes());
            }
            _ => {}
        }
    }
    out
}
