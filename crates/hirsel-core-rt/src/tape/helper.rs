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
use crate::net::CrossingSockets;
use crate::units_lang::packing::{append, copy_elements, copy_flat};
use crate::units_lang::{Cross, SpawnEntry};
use hirsel_core_layout::cross::{
    CAPABILITY, CLIENT, ELEMENT, FUNCTION, MEDIA, OTHER, PADDING, SECRET, SEQ, SHARED, SOCKET,
    elements, held_guards, inside, live, width_mask,
};
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
    grants: Vec<units::item_grants::Designation>,
    sources: Vec<(u64, Box<Provider>, u64)>,
}

impl Helper {
    /// A helper replaying its own stream on `group`'s cell: the entry's
    /// capabilities are already designated there, by words of the run.
    pub(super) fn replaying(group: Group, stream: u32) -> Self {
        Self {
            group: Some(group),
            stream,
            spawn_args: None,
            grants: vec![],
            sources: vec![],
        }
    }

    pub(crate) fn group(&self) -> Option<Group> {
        self.group
    }

    pub(crate) fn stream(&self) -> u32 {
        self.stream
    }

    pub(crate) fn spawn_args(&self) -> Option<&[u8]> {
        self.spawn_args.as_deref()
    }

    /// The helper's taped sockets: carriers of its own stream's objects, as
    /// the root's are of its stream's. None when its stream is cut.
    pub(crate) fn net_tape(&self) -> Option<crate::net::SocketTape> {
        self.group
            .map(|g| crate::net::SocketTape::new(g.cell.provider, handle(g.generation, 0)))
    }

    pub(crate) fn secret_tape(&self) -> Option<crate::secret::MacTape> {
        self.group.map(|g| crate::secret::MacTape {
            provider: g.cell.provider,
            handle: handle(g.generation, 0),
        })
    }

    /// Designate the entry's capabilities in the helper's registry: on its
    /// cell, or on their sources when its stream is cut.
    pub(crate) fn designate(&self, registry: &mut crate::host_file::Registry) {
        if let Some(group) = self.group {
            for d in &self.grants {
                d.apply(&group.cell.provider, registry);
            }
        }
        for (word, provider, handle) in &self.sources {
            registry.designate_in_unit(*word, &**provider, *handle);
        }
    }

    /// Write the helper's `END` as `join` delivers it: its result, or its
    /// causes. Its scheduler has stopped, so no other frame follows. Returns
    /// the outcome for the parent's end `TAKE`, or why it has none. The
    /// result's capabilities and `Secret` words resolve through `context`, the
    /// helper's, and its registry.
    /// # Safety
    /// `result` holds an initialized `R` of `bytes` bytes, with `owners`,
    /// when `failed` is false; `context` and its host files are live.
    pub(crate) unsafe fn end(
        &self,
        failed: bool,
        causes: &[&Cause],
        result: *const u8,
        bytes: u64,
        owners: &[[u64; 4]],
        context: *const crate::Context,
        sockets: Option<&CrossingSockets>,
    ) -> Result<EndTake, &'static str> {
        let mut handles = Vec::new();
        let mut grants = Vec::new();
        let written = if failed {
            Ok(None)
        } else {
            let r = Cross {
                bytes,
                owners: owners.as_ptr(),
                count: owners.len() as u64,
                at: 0,
                len: bytes,
            };
            // SAFETY: forwarded.
            let words = unsafe { capability_words(result, &r, None) };
            if !words.is_empty() {
                // SAFETY: by the contract.
                grants = unsafe {
                    units::item_grants::resolve(context, &(*(*context).host_files).registry, &words)
                };
            }
            // The END leaves out the ordinals. The owner's take gives each
            // handle its ordinal there.
            let mut handle = |kind, word: u64| {
                handles.push((kind, word));
                match kind {
                    CLIENT => units::client_snapshot(word, 0),
                    SHARED => units::shared_snapshot(word, 0),
                    // SAFETY: the initialized result uniquely holds this media word.
                    MEDIA => unsafe { crate::media::snapshot(word, None) },
                    SOCKET => units::crossing::sent(sockets, word, Some(0)),
                    _ => units::item_grants::sent(&grants, kind, word),
                }
            };
            // SAFETY: forwarded.
            unsafe { value(result, &r, None, &mut handle) }.map(Some)
        };
        // The END holds the result in canonical form, or the causes.
        let body = written.and_then(|written| {
            let canonical;
            let outcome = match &written {
                Some(w) => {
                    canonical = format::Value::decode(w).map_err(|_| NO_CAUSE)?.canonical();
                    let value = format::CanonicalValue::decode(&canonical).map_err(|_| NO_CAUSE)?;
                    HelperOutcome::Returned(value)
                }
                None if causes.is_empty() => return Err(NO_CAUSE),
                None => {
                    let wire = causes.iter().map(|c| wire_cause(c)).collect();
                    if causes[0].kind() == CANCELLED {
                        HelperOutcome::Cancelled(wire)
                    } else {
                        HelperOutcome::Failed(wire)
                    }
                }
            };
            Ok((HelperEnding::encode(&outcome), written))
        });
        let take = |ending, written| EndTake {
            ending,
            written,
            handles,
            grants,
            sockets: sockets.cloned(),
        };
        let Some(group) = self.group else {
            return body.map(|(ending, written)| take(ending, written));
        };
        let mut state = group.cell.state.lock().unwrap();
        if state.generation != group.generation {
            return body.map(|(ending, written)| take(ending, written));
        }
        let Some(live) = state.live.as_mut() else {
            return body.map(|(ending, written)| take(ending, written));
        };
        if live.mode == Mode::Replay {
            // The helper's replay ends at its `END`: the outcome it reached is
            // the recorded one, and its stream holds nothing after it.
            let out = match body {
                Ok((ending, written)) => {
                    let gap = live.schedule.events - live.schedule.last;
                    let frame = Frame::HelperEnd {
                        gap,
                        ending: ending.clone(),
                    };
                    let rep = live.replayer.as_mut().unwrap();
                    match super::helper_replay::ended(rep, &frame) {
                        Ok(()) => Ok(take(ending, written)),
                        Err(fault) => {
                            live.fault = Some(refuse(fault));
                            live.abort_replay(fault, 0);
                            Err(DIVERGED)
                        }
                    }
                }
                Err(why) => {
                    live.refuse_replay(why);
                    Err(why)
                }
            };
            let (unit, launcher) = (live.unit().to_owned(), live.replay_abort.is_some());
            drop(state);
            let _ = finish(group.cell, group.generation, Seal::Unsealed);
            if launcher && out.is_ok() {
                units::verdicts::ended(&unit, None);
            }
            return out;
        }
        let rec = live.recorder.as_mut().unwrap();
        let out = match body {
            Ok((ending, written)) => {
                let gap = live.schedule.events - live.schedule.last;
                live.schedule.last = live.schedule.events;
                let _ = rec.frame(&Frame::HelperEnd {
                    gap,
                    ending: ending.clone(),
                });
                Ok(take(ending, written))
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
const DIVERGED: &str = "the helper's replay diverged at its END";

/// A helper's outcome as its owner's end `TAKE` holds it: its `END`, a
/// result as written, with its handles in the order written, each with
/// ordinal 0 until the take gives it the owner's, and its capabilities'
/// grants.
pub(crate) struct EndTake {
    pub(super) ending: HelperEnding,
    pub(super) written: Option<Vec<u8>>,
    pub(super) handles: Vec<(u64, u64)>,
    pub(super) grants: Vec<units::item_grants::Granted>,
    /// The result's sockets in transit, which the owner's take names.
    pub(super) sockets: Option<CrossingSockets>,
}

/// The `END` a helper's stream ends with when its unit deadlocks.
pub(super) fn deadlock(gap: u64, events: u64) -> Frame {
    Frame::HelperEnd {
        gap,
        ending: HelperEnding::encode(&HelperOutcome::Deadlock(events)),
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
    spawned: &SpawnEntry,
    sockets: Option<&CrossingSockets>,
) -> Option<Helper> {
    let spawner = crate::tasks::current_group()?;
    let x = &spawned.x;
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
        .filter(|&&(kind, _)| kind == CAPABILITY)
        .map(|&(_, word)| {
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
        // SAFETY: as above.
        unsafe { units::item_grants::resolve(context, &(*files).registry, &words) }
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
        sockets: BTreeMap::new(),
        recorder: None,
        replayer: None,
    };
    let mut grants = Vec::new();
    // SAFETY: forwarded; each capability word designates a grant of the run.
    let body = unsafe {
        entry(context, n, steps, input, spawned, &mut |kind, word| {
            units::arrive(
                &mut live,
                cell.provider.state as usize,
                generation,
                &granted,
                &mut grants,
                sockets,
                kind,
                word,
            )
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
    let spawn_args = unit_spawn_args(&entry);
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

/// The parts of a `UNIT_SPAWN`'s arguments: the entry's code descriptor, `n`
/// and `x` in canonical form.
fn unit_spawn_args(entry: &Entry) -> Vec<u8> {
    let b = entry.body();
    let mut args = b.descriptor.to_vec();
    args.extend_from_slice(&b.n.to_le_bytes());
    args.extend(b.value.canonical());
    args
}

/// A replayed spawn's `UNIT_SPAWN` arguments, as a recorded spawn writes
/// them, or why this build does not replay the spawn: an entry holding a
/// handle (docs/recording.md, "Replay of a run with helpers").
/// # Safety
/// As for `spawn`.
pub(crate) unsafe fn replay_args(
    context: *const crate::Context,
    n: u64,
    steps: Option<u64>,
    input: *const u8,
    spawned: &SpawnEntry,
    sockets: Option<&CrossingSockets>,
) -> Result<Vec<u8>, &'static str> {
    let x = &spawned.x;
    let held = Cross {
        owners: x.owners()[1.min(x.owners().len())..].as_ptr(),
        count: x.count.saturating_sub(1),
        ..*x
    };
    // SAFETY: the input is initialized as `x` describes it.
    let words = unsafe { capability_words(input, &held, None) };
    // SAFETY: the spawner's context and its host files are live.
    let files = unsafe { (*context).host_files };
    let granted = if words.is_empty() || files.is_null() {
        Vec::new()
    } else {
        // SAFETY: as above; no cell is locked here.
        unsafe { units::item_grants::resolve(context, &(*files).registry, &words) }
    };
    // SAFETY: forwarded.
    let body = unsafe {
        entry(
            context,
            n,
            steps,
            input,
            spawned,
            &mut |kind, word| match kind {
                CAPABILITY | SECRET => units::item_grants::sent(&granted, kind, word),
                CLIENT => units::client_snapshot(word, 0),
                SHARED => units::shared_snapshot(word, 0),
                SOCKET => units::crossing::sent(sockets, word, Some(0)),
                MEDIA => crate::media::snapshot(word, None),
                _ => Err(HANDLE),
            },
        )
    }?;
    Ok(unit_spawn_args(
        &Entry::decode(0, 0, body).expect("a well-formed entry"),
    ))
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
    spawned: &SpawnEntry,
    handle: &mut dyn FnMut(u64, u64) -> Result<Vec<u8>, &'static str>,
) -> Result<Vec<u8>, &'static str> {
    let _ = context;
    let x = &spawned.x;
    let owners = x.owners();
    // The wrapped input's first field is `f`, so its row comes first.
    let Some(&[f, FUNCTION, ..]) = owners.first() else {
        return Err("the entry has no code word");
    };
    // SAFETY: the input holds `f`'s code word at its row.
    let code = unsafe { input.add(f as usize).cast::<u64>().read_unaligned() };
    let identity = run::identities::identity_of(code).ok_or(NO_IDENTITY)?;
    // A replay starts the helper from the link's row for its identity, so
    // the recording holds only entries that row starts as this spawn does.
    match run::identities::spawn_entry(identity) {
        Some(row) if row.same(spawned) => {}
        Some(_) => return Err(OTHER_ENTRY),
        None => return Err(NO_ENTRY),
    }
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

const HANDLE: &str = "the value holds a handle whose snapshot this build does not write";

/// The capability and `Secret` words of a value by row kind, in the order it
/// holds them, nested ones included: what `item_grants::resolve` reads
/// before any cell is locked.
/// # Safety
/// As for `value`.
pub(super) unsafe fn capability_words(
    base: *const u8,
    x: &Cross,
    content: Option<*const u8>,
) -> Vec<(u64, u64)> {
    let mut words = Vec::new();
    let mut collect = |kind, word| {
        if matches!(kind, CAPABILITY | SECRET) {
            words.push((kind, word));
        }
        Ok(Vec::new())
    };
    // SAFETY: forwarded. A value that cannot be written is cut where it is
    // written, so its error here is not this walk's.
    let _ = unsafe { value(base, x, content, &mut collect) };
    words
}
const NO_IDENTITY: &str = "the entry's function has no code identity (a closure)";
const NO_ENTRY: &str = "the entry's function has no row in the link's spawn table";
const OTHER_ENTRY: &str =
    "the link's spawn table row for the entry's function differs from the spawn's";

/// A value that crosses units: its plain bytes with padding and owner words zeroed,
/// then a part per owner in ascending offset order, each sequence's part
/// followed by the parts of its elements' owner words, element by element. A
/// packed value (an item in a connection) passes `content`, its sequences'
/// elements one after another in owner-row order; otherwise each sequence
/// word points at them.
pub(super) unsafe fn value(
    base: *const u8,
    x: &Cross,
    content: Option<*const u8>,
    handle: &mut dyn FnMut(u64, u64) -> Result<Vec<u8>, &'static str>,
) -> Result<Vec<u8>, &'static str> {
    let mut plain = Vec::new();
    let bytes = x.len as usize;
    // SAFETY: the span contains the live fields described by the rows. The
    // copy initializes any padding before exposing it as bytes.
    unsafe {
        append(&mut plain, bytes, |to| {
            copy_flat(base, to, x.at, bytes, x.owners())
        });
    }
    let read = |at: u64| {
        // SAFETY: an owner row names initialized words of the value.
        unsafe { base.add(at as usize).cast::<u64>().read_unaligned() }
    };
    let tag = |at: u64, width: u64| {
        let mut word = [0u8; 8];
        // SAFETY: a guard names the initialized tag word of a variant in the value.
        unsafe {
            std::ptr::copy_nonoverlapping(base.add(at as usize), word.as_mut_ptr(), width as usize)
        };
        u64::from_le_bytes(word)
    };
    // Each owner row with the element rows that follow it, a variant's rows
    // only for its case.
    let rows = live(x.owners(), &tag);
    let mut owners = Vec::new();
    let mut i = 0;
    while i < rows.len() {
        let inner = elements(&rows[i + 1..]);
        if rows[i][1] != PADDING {
            owners.push((rows[i], inner));
        }
        i += 1 + inner.len();
    }
    let mut packed = BTreeMap::new();
    if let Some(content) = content {
        let mut at = 0usize;
        for &([offset, kind, element, _], _) in &owners {
            if kind == SEQ {
                // SAFETY: the packed content holds each sequence's elements in row order.
                packed.insert(offset, unsafe { content.add(at) });
                at += (read(offset + 8) * element) as usize;
            }
        }
    }
    owners.sort_by_key(|o| o.0[0]);
    let mut parts = Vec::new();
    let mut count = 0u64;
    for &([at, kind, element, _], inner) in &owners {
        let words = if kind == SEQ { 3 } else { 1 };
        let start = (at - x.at) as usize;
        plain[start..start + 8 * words].fill(0);
        count += 1;
        match kind {
            SEQ => {
                let len = read(at + 8);
                let bytes = (len * element) as usize;
                parts.push(0);
                put_varint(&mut parts, bytes as u64);
                if bytes == 0 {
                    continue;
                }
                let from = packed.get(&at).copied().unwrap_or(read(at) as *const u8);
                let start = parts.len();
                // SAFETY: the carrier or packed content names these live
                // elements. The copy initializes their padding.
                unsafe {
                    append(&mut parts, bytes, |to| {
                        copy_elements(from, to, bytes, element as usize, inner)
                    });
                }
                if inner.is_empty() {
                    continue;
                }
                // Each element's owner words zeroed, then their parts.
                let elements = &mut parts[start..];
                let inner: Vec<_> = inner
                    .iter()
                    .map(|&[offset, kind, low, count]| [offset, kind & !ELEMENT, low, count])
                    .collect();
                let mut tail = Vec::new();
                for e in 0..len as usize {
                    let base = e * element as usize;
                    let mut words = live(&inner, &|at, width| {
                        let at = base + at as usize;
                        let mut word = [0u8; 8];
                        word[..width as usize].copy_from_slice(&elements[at..at + width as usize]);
                        u64::from_le_bytes(word)
                    });
                    words.sort_by_key(|o| o[0]);
                    for [offset, kind, ..] in words {
                        if kind == PADDING {
                            continue;
                        }
                        let at = base + offset as usize;
                        let held = &mut elements[at..at + 8];
                        let word = u64::from_le_bytes((&*held).try_into().unwrap());
                        held.fill(0);
                        word_part(kind, word, &mut tail, handle)?;
                        count += 1;
                    }
                }
                parts.extend_from_slice(&tail);
            }
            SHARED => {
                // The snapshot's head, then the payload as a value: a holder
                // points at the node's count, and the payload follows it.
                let node = read(at);
                parts.push(1);
                parts.extend(handle(kind, node)?);
                let (bytes, rows) = units::node_layout(node).ok_or(units::UNNAMED_NODE)?;
                let payload = Cross {
                    bytes,
                    owners: rows.as_ptr(),
                    count: rows.len() as u64,
                    at: 0,
                    len: bytes,
                };
                // SAFETY: a live holder pins its node, whose payload is a
                // live value laid out as its owner rows say.
                parts.extend(unsafe { value((node + 8) as *const u8, &payload, None, handle) }?);
            }
            _ => word_part(kind, read(at), &mut parts, handle)?,
        }
    }
    // A variant laid out in an owner word's niche (a `Shared` holder's null)
    // keeps its case: a word whose zero would name another case is written
    // as the first value past the niche (recording.md, "Values").
    for guard in held_guards(x.owners(), &tag) {
        let width = ((guard[1] >> 8) & 0xff) as usize;
        let Some(start) = guard[0].checked_sub(x.at).map(|s| s as usize) else {
            continue;
        };
        if guard[1] & OTHER == 0 || start + width > plain.len() {
            continue;
        }
        let mut word = [0u8; 8];
        word[..width].copy_from_slice(&plain[start..start + width]);
        if inside(&guard, u64::from_le_bytes(word)) {
            let past = guard[2].wrapping_add(guard[3]) & width_mask(&guard);
            plain[start..start + width].copy_from_slice(&past.to_le_bytes()[..width]);
        }
    }
    let mut out = Vec::with_capacity(plain.len() + parts.len() + 20);
    put_varint(&mut out, plain.len() as u64);
    out.extend_from_slice(&plain);
    put_varint(&mut out, count);
    out.extend_from_slice(&parts);
    Ok(out)
}

/// The part of one owner word: a handle's snapshot or a function's identity.
fn word_part(
    kind: u64,
    word: u64,
    parts: &mut Vec<u8>,
    handle: &mut dyn FnMut(u64, u64) -> Result<Vec<u8>, &'static str>,
) -> Result<(), &'static str> {
    match kind {
        CAPABILITY | CLIENT | MEDIA | SECRET | SOCKET => {
            parts.push(1);
            parts.extend(handle(kind, word)?);
        }
        FUNCTION => {
            let identity = run::identities::identity_of(word).ok_or(NO_IDENTITY)?;
            parts.push(2);
            put_varint(parts, identity.len() as u64);
            parts.extend_from_slice(identity);
        }
        _ => return Err(HANDLE),
    }
    Ok(())
}

/// A cause as a helper `END` writes it: its kind, site and detail.
fn wire_cause(cause: &Cause) -> format::Cause<'_> {
    let m = &cause.message;
    let detail = match cause.kind() {
        FAIL => format::CauseDetail::Fail {
            message: if m.len == 0 {
                &[]
            } else {
                // SAFETY: a live Fail cause owns its message bytes.
                unsafe { std::slice::from_raw_parts(m.data, m.len as usize) }
            },
            // Its String capacity, 0 for static text: the join charges it.
            capacity: m.capacity,
        },
        CAPACITY => format::CauseDetail::Capacity {
            requested: m.data as u64,
            available: m.len,
        },
        _ => format::CauseDetail::None,
    };
    format::Cause {
        kind: cause.kind(),
        site: cause.site,
        detail,
    }
}
