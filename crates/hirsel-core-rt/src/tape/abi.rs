//! The core surface's runtime rows (`rt.tape_open`, `rt.tape_end`,
//! `rt.tape_grant`, `rt.tape_across`) and the Tape resource's drop.
//!
//! A `Tape` value is a run-local u64 key into the run's `Files` registry, like
//! a HostFile's. A substitute is a dynamic grant (`Files::grant`) whose
//! provider is the tape's; a substitute `Args` is a dynamic grant naming a
//! vector the registry pins for the run, so `args.len` and `args.get` read the
//! recorded vector, also after the tape ends (reads of `Args` are not
//! operations, REPLAY-SPEC v2.3 R12).
//!
//! Open and end report in one word, which core's `tape.hirsel` decodes:
//! bits 0-7 a tag, bits 8-15 a sub-kind, bits 16-63 a value.
//! - open: 0 opened; 1 Refused (sub 0 Code, 1 Revision, 2 Arg, 3 Limit,
//!   4 Damaged(value); 5 HeaderTooLarge); 2 Io (sub the `IoError` tag 1-5, value `Other`'s code).
//! - end: 0 Sealed; 1 Diverged (sub the mismatch: 0 Op, 1 Object, 2 Arg,
//!   3 Missing, 4 Extra, 5 Outcome; value `at`); 2 Truncated(value);
//!   3 Damaged(value); 4 Corrupt; 5 Io (as open's).
use super::{Fault, Mismatch, Mode, OpenError, Refusal, Tape, format::*};
use crate::{
    Context,
    causes::{self, Cause},
    entry::{self, ARGS},
    provider::{Buffer, IoError},
};

fn word(tag: u8, sub: u8, value: u64) -> u64 {
    u64::from(tag) | (u64::from(sub) << 8) | (value << 16)
}
fn io(e: IoError) -> u64 {
    word(5, e.tag, u64::from(e.code))
}
/// End's word for a fault. `Ended` cannot reach `tape.end`, whose Tape is the
/// caller's own.
fn fault_word(f: Fault) -> u64 {
    match f {
        Fault::Diverged(d) => {
            let sub = match d.kind {
                Mismatch::Op => 0,
                Mismatch::Object => 1,
                Mismatch::Arg => 2,
                Mismatch::Missing => 3,
                Mismatch::Extra => 4,
                Mismatch::Outcome => 5,
                Mismatch::Schedule => 6,
            };
            word(1, sub, d.at)
        }
        Fault::Truncated(at) => word(2, 0, at),
        Fault::Damaged(k) => word(3, 0, k),
        Fault::Corrupt | Fault::Ended => word(4, 0, 0),
        Fault::Io(e) => io(e),
    }
}

/// # Safety
/// A live carrier of the run.
unsafe fn bytes<'a>(b: *const Buffer) -> &'a [u8] {
    // SAFETY: the emitted operand is a live String or Bytes carrier.
    unsafe {
        let b = &*b;
        if b.len == 0 {
            &[]
        } else {
            std::slice::from_raw_parts(b.data, b.len)
        }
    }
}

#[repr(C)]
pub struct Opened {
    pub tape: u64,
    pub code: u64,
}

/// `rt.tape_open`: open `name` in the Dir `dir` as a Run tape. `mode` is
/// core's `tape.Mode` case (0 Record, 1 Replay, 2 Across: a Replay that
/// opens a tape of another revision); `id` is the tape id's two words,
/// little-endian, first word first. Replay compares the argument always,
/// and the charge policy is 0 (S1 has one policy). `revision` is the entry
/// object's `hirsel_code_revision`.
/// # Safety
/// A live run context; the carriers and `revision` are live for the call.
#[allow(clippy::too_many_arguments)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_tape_open(
    context: *mut Context,
    dir: u64,
    name: *const Buffer,
    mode: u8,
    identity: *const Buffer,
    arg: *const Buffer,
    limit: u64,
    id0: u64,
    id1: u64,
    revision: *const [u8; 32],
) -> Opened {
    // SAFETY: the emitted operands are the run's; no registry borrow spans
    // the open's provider calls.
    unsafe {
        let provider = *entry::hirsel_core_capability_provider(context, dir);
        let handle = entry::hirsel_core_capability_handle(context, dir);
        let mut tape_id = [0; 16];
        tape_id[..8].copy_from_slice(&id0.to_le_bytes());
        tape_id[8..].copy_from_slice(&id1.to_le_bytes());
        let (identity, arg) = (bytes(identity), bytes(arg));
        let record = mode == 0;
        if record && Header::encoded_len(identity.len(), arg.len(), 0).is_err() {
            return Opened {
                tape: 0,
                code: word(1, 5, 0),
            };
        }
        let header = Header {
            kind: 1,
            mode: 1,
            tape_id,
            identity: if record {
                identity.to_vec()
            } else {
                Vec::new()
            },
            revision: [0; 32],
            charge_policy: 0,
            limit,
            bound: 0,
            chooser: crate::tasks::recorded_chooser(),
            arg: if record { arg.to_vec() } else { Vec::new() },
            net_rules: vec![],
            secret_grants: vec![],
        };
        let expect = super::Expect {
            identity,
            charge_policy: 0,
            limit,
            arg: Some(arg),
            across: mode == 2,
        };
        let mode = if record { Mode::Record } else { Mode::Replay };
        let code = match Tape::open(
            *revision,
            (provider, handle),
            bytes(name),
            mode,
            &header,
            &expect,
        ) {
            Ok((tape, _)) => {
                let key = (*(*context).host_files).registry.tape(tape);
                return Opened { tape: key, code: 0 };
            }
            Err(OpenError::Refused(r)) => match r {
                Refusal::Code => word(1, 0, 0),
                Refusal::Revision => word(1, 1, 0),
                Refusal::Arg => word(1, 2, 0),
                Refusal::Limit => word(1, 3, 0),
                Refusal::Damaged(k) => word(1, 4, k),
                Refusal::HeaderTooLarge => word(1, 5, 0),
            },
            Err(OpenError::Io(e)) => word(2, e.tag, u64::from(e.code)),
            // A header block that is cut or breaks the grammar is block 0
            // damaged, as a reader of the file sees it.
            Err(OpenError::Unreadable(_)) => word(1, 4, 0),
        };
        Opened { tape: 0, code }
    }
}

/// `rt.tape_end`: end the tape with the domain's outcome. `kind` is END's
/// (0 Returned, 1 Failed, 2 Cancelled); `operand` is the returned Bytes
/// carrier, or the head cell of the `Causes` the domain failed with.
/// # Safety
/// `tape` is a live key of the run, consumed here; `operand` is live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_tape_end(
    context: *mut Context,
    tape: u64,
    kind: u8,
    operand: *const u8,
) -> u64 {
    // SAFETY: the key is the caller's sole Tape; the chain is borrowed.
    unsafe {
        let tape = (*(*context).host_files).registry.untape(tape);
        let ending = if kind == END_RETURNED {
            Ending::returned(bytes(operand.cast()))
        } else {
            let mut list = vec![];
            let mut cell = operand.cast::<causes::Cell>();
            while !cell.is_null() {
                let c = causes::value(cell);
                let text = c.message().map(|m| match m.len {
                    0 => &[][..],
                    n => std::slice::from_raw_parts(m.data.cast_const(), n as usize),
                });
                list.push((c.kind(), text));
                cell = causes::next(cell);
            }
            Ending::causes(kind, list)
        };
        tape.end(&ending).map_or_else(fault_word, |()| 0)
    }
}

#[repr(C)]
pub struct Granted {
    pub value: u64,
    pub status: u8,
}

/// `rt.tape_across`: whether `tape` was opened Across and its revision is
/// not the opener's.
/// # Safety
/// A live run context; `tape` is a live key of the run.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_tape_across(context: *mut Context, tape: u64) -> bool {
    // SAFETY: the registry is the run's.
    unsafe {
        (*(*context).host_files)
            .registry
            .tape_ref(tape)
            .revision_differs()
    }
}

/// `rt.tape_grant`: the substitute of the capability `source` of `kind`
/// (`entry`'s kind numbers). A refusal fails with cause `Tape` at `site`.
/// # Safety
/// A live run context; `tape` and `source` are live operands of the run.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_tape_grant(
    context: *mut Context,
    tape: u64,
    kind: u8,
    source: u64,
    site: u64,
) -> Granted {
    // SAFETY: the registry is the run's; the tape keeps no borrow of it.
    unsafe {
        let files = (*context).host_files;
        let result = if kind == ARGS {
            let vector = crate::host_file::args(context, source);
            let views: Vec<&[u8]> = vector.iter().map(|b| bytes(b)).collect();
            (*files)
                .registry
                .tape_ref(tape)
                .on_tape_args(&views)
                .map(|stored| (*files).registry.vector(stored))
        } else {
            let provider = *entry::hirsel_core_capability_provider(context, source);
            let handle = entry::hirsel_core_capability_handle(context, source);
            (*files)
                .registry
                .tape_ref(tape)
                .on_tape(kind, (provider, handle))
                .map(|(p, h)| {
                    let group = (*files).registry.tape_ref(tape).group();
                    (*files).registry.taped_grant(p, h, group)
                })
        };
        match result {
            Ok(value) => Granted { value, status: 0 },
            Err(_) => {
                causes::record(&mut *context, Cause::new(causes::TAPE, site));
                Granted {
                    value: 0,
                    status: 1,
                }
            }
        }
    }
}

/// The Tape resource's drop: its buffered frames are written and nothing
/// else (REPLAY-SPEC section 8).
/// # Safety
/// `tape` is the sole live key, consumed here.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_tape_drop(context: *mut Context, tape: u64) {
    // SAFETY: the key is the run's.
    drop(unsafe { (*(*context).host_files).registry.untape(tape) });
}
