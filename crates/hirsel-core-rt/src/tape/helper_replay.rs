//! A helper stream's replay (docs/recording.md, "Replay of a run with
//! helpers"). Each helper stream replays as its own unit, on a thread of its
//! own, started from its stream's entry and never from its parent's spawn: its
//! code is the link's spawn-table row for the entry's identity and its input
//! is the entry's value. Its takes come only from its own `TAKE` frames, and
//! its `END` is checked against the outcome it reaches.
use super::schedule::Group;
use super::units::replay::{Receiver, Unpacked, arrived, packed};
use super::units::verdicts;
use super::*;
use crate::units_lang::SpawnEntry;
use hirsel_core_layout::cross::FUNCTION;

/// One helper stream of the tape, read through its own file.
pub(super) struct Stream {
    pub(super) stream: u32,
    pub(super) unit: String,
    pub(super) source: ProviderFile,
    pub(super) blocks: StreamBlocks,
}

/// What every helper stream's replay shares with the root's: the tape, the
/// streams the index holds and whether one unit replays alone.
pub(super) struct Run {
    tape_id: [u8; 16],
    chooser: u8,
    origin: (Inner, Vec<u8>),
    streams: BTreeMap<u32, String>,
    alone: bool,
}

impl Run {
    pub(super) fn of(replay: &ReplayInput) -> Self {
        Self {
            tape_id: replay.stored.tape_id,
            chooser: replay.stored.chooser,
            origin: replay.origin.clone(),
            streams: replay.streams.clone(),
            alone: replay.unit.is_some(),
        }
    }
}

/// A helper ready to replay: its tape, its code, its wrapped input packed as
/// a spawn packs it, its limit and its step bound.
pub(crate) struct Ready {
    pub(crate) helper: helper::Helper,
    pub(crate) entry: SpawnEntry,
    pub(crate) data: Vec<u8>,
    pub(crate) n: u64,
    pub(crate) steps: Option<u64>,
}

/// Start each helper stream's replay. A stream whose entry cannot start ends
/// at position 0 with its verdict: `Diverged(Op)` when its code descriptor
/// does not resolve in this build.
/// # Safety
/// `context` is the root's, live for the run, with its host files.
pub(super) unsafe fn start(
    context: *const crate::Context,
    streams: Vec<Stream>,
    run: &Run,
    abort: ReplayAbort,
) {
    for stream in streams {
        verdicts::started();
        let unit = stream.unit.clone();
        // SAFETY: forwarded.
        match unsafe { open(context, stream, run, abort) } {
            Ok(ready) => {
                // SAFETY: forwarded.
                unsafe { crate::units_lang::replay::start_helper(context, ready) }
            }
            Err(Opened::Verdict(line)) => verdicts::ended(&unit, Some(line)),
            Err(Opened::Refused(why)) => refuse(abort, why),
        }
    }
}

enum Opened {
    /// The unit's replay ends at position 0 with this line.
    Verdict(String),
    /// This build does not replay the entry: the whole replay is refused.
    Refused(&'static str),
}

fn refuse(abort: ReplayAbort, why: &str) {
    let line = format!("replay: refused: {why}\n");
    // SAFETY: `abort.stderr` is the launcher's stderr fd.
    unsafe {
        crate::os::diagnostic(abort.stderr, line.as_ptr(), line.len());
        libc::_exit(2);
    }
}

/// A unit's tape state before its first frame: the run's chooser, no objects.
fn fresh(tape_id: [u8; 16], chooser: u8, mode: Mode) -> Live {
    Live {
        tape_id,
        waiting: BTreeMap::new(),
        pending: BTreeMap::new(),
        schedule: schedule::Schedule::new(chooser),
        mode,
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
    }
}

/// Open one helper stream: its entry, its code and its input, and its own
/// cell replaying the frames after its entry.
unsafe fn open(
    context: *const crate::Context,
    stream: Stream,
    run: &Run,
    abort: ReplayAbort,
) -> Result<Ready, Opened> {
    let Stream {
        stream,
        unit,
        source,
        blocks,
    } = stream;
    let at_zero = |fault| Opened::Verdict(format_verdict(fault, 0, &unit));
    let mut reader = FrameReader::open(source, blocks).map_err(at_zero)?;
    let entry = reader.entry().map_err(at_zero)?;
    let body = entry.body();
    let resolved = run::identities::spawn_entry(body.descriptor)
        .zip(run::identities::code_of(body.descriptor));
    let Some((spawned, code)) = resolved else {
        let fault = Fault::Diverged(Divergence {
            at: 0,
            kind: Mismatch::Op,
        });
        return Err(Opened::Verdict(format!(
            "{} [event 0: frame ENTRY; expected a spawn table row for its code descriptor; \
             observed none]",
            format_verdict(fault, 0, &unit)
        )));
    };
    let mut live = fresh(run.tape_id, run.chooser, Mode::Replay);
    live.replay_abort = Some(abort);
    let cell = cell();
    let generation = cell
        .state
        .lock()
        .unwrap()
        .generation
        .checked_add(1)
        .unwrap_or_else(|| crate::process::stop(b"TapeSlots"));
    // The wrapped input: the adapter's code word first, which the bridge
    // reads, `f`'s code word at its row, then `x` from the entry's value. Each capability it holds takes the helper's ordinal and
    // a word of the run designating it on the helper's cell.
    let x = &spawned.x;
    let owners = x.owners();
    let Some(&[f, FUNCTION, ..]) = owners.first().filter(|row| row[0] >= 8 && x.at >= 8) else {
        return Err(Opened::Refused("a spawn table row with no code word"));
    };
    let mut flat = vec![0u8; x.bytes as usize];
    flat[..8].copy_from_slice(&spawned.adapter.to_le_bytes());
    flat[f as usize..f as usize + 8].copy_from_slice(&code.to_le_bytes());
    // SAFETY: the root's host files are live for the run.
    let registry = unsafe { &(*(*context).host_files).registry };
    let at = Receiver {
        registry: Some(registry),
        provider: cell.provider,
        generation,
    };
    let mut word = |kind: u64, c: &mut format::Cursor<'_>| arrived(&mut live, at, kind, c);
    let span = (x.at as usize, x.len as usize);
    let data = match packed(body.value.bytes(), flat, span, &owners[1..], &mut word) {
        Ok(data) => data,
        Err(Unpacked::Layout) => return Err(at_zero(Fault::Corrupt)),
        Err(Unpacked::Unsupported(why)) => return Err(Opened::Refused(why)),
    };
    let (n, steps) = (body.n, body.bound);
    live.replayer = Some(Replayer {
        reader,
        closes: Closes::new(run.origin.clone(), run.tape_id, stream),
        lookahead: None,
        taken: 0,
        comparison: None,
        pending_reads: BTreeMap::new(),
        stream,
        unit,
        streams: run.streams.clone(),
        alone: run.alone,
        spawns: 0,
    });
    let mut state = cell.state.lock().unwrap();
    state.generation = generation;
    state.live = Some(live);
    drop(state);
    Ok(Ready {
        helper: helper::Helper::replaying(Group::root(cell, generation), stream),
        entry: spawned,
        data,
        n,
        steps,
    })
}

/// A helper's `END`: the stream's next frame is `expected`, and nothing
/// follows it.
pub(super) fn ended(rep: &mut Replayer, expected: &Frame) -> Result<(), Fault> {
    let Frame::HelperEnd {
        gap: at,
        ending: outcome,
    } = expected
    else {
        unreachable!("a helper's END")
    };
    let frame = rep.next()?;
    let mismatch = match &frame {
        Frame::HelperEnd { gap, .. } if gap != at => Some(Mismatch::Schedule),
        Frame::HelperEnd { ending, .. } if ending != outcome => {
            Some(if outcome.kind() == 3 || ending.kind() == 3 {
                Mismatch::Schedule
            } else {
                Mismatch::Outcome
            })
        }
        Frame::HelperEnd { .. } => None,
        Frame::Wake { .. }
        | Frame::Cancel { .. }
        | Frame::Retired { .. }
        | Frame::Done { .. }
        | Frame::Choose { .. }
        | Frame::Wait { .. } => Some(Mismatch::Schedule),
        _ => Some(Mismatch::Extra),
    };
    if let Some(kind) = mismatch {
        let observed = format!("END gap={at} outcome_kind={}", outcome.kind());
        return Err(rep.compared(kind, &frame, format_args!("{observed}")));
    }
    rep.closes.finish()?;
    match rep.reader.next(rep.taken + 1) {
        Err(Fault::Diverged(Divergence {
            kind: Mismatch::Extra,
            ..
        })) => Ok(()),
        Err(f) => Err(f),
        Ok(_) => Err(Fault::Corrupt),
    }
}

impl ReplayInput {
    /// The helper streams this replay starts, each as its own unit: every
    /// one the index holds, or the one `--unit` names. Each is read through
    /// its own file.
    pub(super) fn helper_streams(&mut self) -> Result<Vec<Stream>, OpenError> {
        let (chosen, _) = self.chosen()?;
        let mut streams = Vec::new();
        for (&stream, unit) in &self.streams {
            if stream == 0 || self.unit.is_some() && stream != chosen {
                continue;
            }
            let blocks = self.index.take(stream).expect("an indexed stream");
            let (dir, name) = &self.origin;
            // SAFETY: the replay's directory provider is pinned for the run.
            let source = unsafe { ProviderFile::open(*dir, 0, name) }.map_err(OpenError::Io)?;
            streams.push(Stream {
                stream,
                unit: unit.clone(),
                source,
                blocks,
            });
        }
        Ok(streams)
    }

    /// Admission (recording.md, "Replay"): the recording's entry identity,
    /// revision, argument and limit are the replaying build's.
    pub(super) fn admit(&self, revision: [u8; 32], expect: &Expect) -> Result<(), OpenError> {
        let stored = &self.stored;
        if stored.identity != expect.identity {
            return Err(OpenError::Refused(Refusal::Code));
        }
        if stored.revision != revision && !expect.across {
            return Err(OpenError::Refused(Refusal::Revision));
        }
        if expect.arg.is_some_and(|a| a != stored.arg) {
            return Err(OpenError::Refused(Refusal::Arg));
        }
        if stored.limit != expect.limit || stored.charge_policy != expect.charge_policy {
            return Err(OpenError::Refused(Refusal::Limit));
        }
        Ok(())
    }
}

impl Live {
    /// The root's replay sealed: wait for the other streams' verdicts and
    /// exit 1 if any was not `Sealed`.
    pub(super) fn conclude_sealed(&self) {
        if let Some(abort) = self.replay_abort
            && units::verdicts::conclude(abort.stderr, None)
        {
            // SAFETY: the replay ends here.
            unsafe { libc::_exit(1) };
        }
    }
}
