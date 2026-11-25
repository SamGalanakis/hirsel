//! CLOUD #3 and SQLITE #3: a W2 cut or injected EIO proves it reached the
//! operation it names. A cut records its intercepted step, logical file role
//! and completed prefix; an eligible host-call ordinal, counted before
//! effects, selects an EIO beside the mutation ordinal.
use super::{Bridge, Error, Op, Outcome, ZERO_AT};
use std::{ffi::c_void, slice};

/// An EIO injected at one eligible host call, named by phase, ordinal,
/// operation and logical file role. `reached` is whether that call matched.
pub(super) struct Eio {
    phase: u64,
    ordinal: u64,
    op: String,
    role: String,
    reached: Option<bool>,
}

pub(super) const PHASES: [&str; 4] = ["init", "write", "read", "reopen"];

/// A file's logical role: its name without sequence numbers (`j-.end`).
fn role(name: &str) -> String {
    name.chars().filter(|c| !c.is_ascii_digit()).collect()
}

fn fnv(mut hash: u64, text: &str) -> u64 {
    for byte in text.bytes().chain([b'\n']) {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
    }
    hash
}

impl Bridge {
    /// A write-phase step's kind and logical role, as its cut names it.
    fn step(&self, op: &Op<'_>) -> String {
        let kind = match op {
            Op::Create(_) => "create",
            Op::Write(name, bytes) => {
                // §3.3b's mode-specific images: a Direct extension (the
                // creation block or a window step), a Direct overwrite
                // of the zeroed window, and a Durable END rewrite whose
                // one sector straddles EOF. Each is pending until the
                // sync event that follows it.
                let old = self.fs.read(name).unwrap_or(&[]);
                let differs = old.iter().zip(*bytes).any(|(old, new)| old != new);
                if self.fs.is_direct(name) && bytes.len() > old.len() {
                    "extend"
                } else if self.fs.is_direct(name) && differs {
                    "window"
                } else if name.ends_with(".end") && differs && bytes.len() % 512 != 0 {
                    "straddle"
                } else if differs {
                    "overwrite"
                } else {
                    "write"
                }
            }
            Op::Append(..) => "append",
            Op::Sync(_) => "sync",
            Op::Rename(..) => "rename",
            Op::Remove(_) => "unlink",
            Op::SyncDir => "sync-dir",
        };
        let role = match op {
            Op::Create(name)
            | Op::Write(name, _)
            | Op::Append(name, _)
            | Op::Sync(name)
            | Op::Remove(name) => role(name),
            Op::Rename(from, to) => format!("{}>{}", role(from), role(to)),
            Op::SyncDir => "dir".to_owned(),
        };
        format!("{kind} {role}")
    }

    /// The write phase's cut and completed-prefix bookkeeping for one step;
    /// false once the step is cut.
    pub(super) fn reach(&mut self, op: &Op<'_>) -> bool {
        let step = (self.phase == 1 && !self.crashed).then(|| self.step(op));
        if self.phase == 1 && self.events == self.cut && !self.crashed {
            self.crashed = true;
            let receipt = format!(
                "{} {} {:016x}",
                self.events + 1,
                step.as_ref().unwrap(),
                self.prefix
            );
            eprintln!("w2-receipt {receipt}");
            self.receipt = Some(receipt);
        }
        if self.crashed {
            return false;
        }
        if let Some(step) = &step {
            if self.trace_cuts {
                eprintln!("w2-cut {} {step} {:016x}", self.events + 1, self.prefix);
            }
            self.prefix = fnv(self.prefix, step);
        }
        true
    }

    /// SQLITE #3: count an eligible host call before any of its effects, and
    /// fail it with EIO when it is the selected one. A selected ordinal that
    /// lands on another operation or role is recorded as a miss, not failed.
    pub(super) fn eligible(&mut self, op: u8, handle: u64, path: &[u8]) -> Option<Outcome> {
        let text = |bytes: &[u8]| {
            std::str::from_utf8(bytes)
                .expect("Hirsel String path")
                .to_owned()
        };
        let (name, role) = match op {
            0 => ("open", role(&text(path))),
            2 | 38 => ("write", role(&self.files[&handle].name)),
            16 => ("sync", role(&self.files[&handle].name)),
            ZERO_AT => ("zero", role(&self.files[&handle].name)),
            8 => ("remove", role(&text(path))),
            14 => {
                let split = path.iter().position(|&b| b == 0).expect("rename delimiter");
                let from = role(&text(&path[..split]));
                (
                    "rename",
                    format!("{from}>{}", role(&text(&path[split + 1..]))),
                )
            }
            17 => ("sync-dir", "dir".to_owned()),
            _ => return None,
        };
        self.calls += 1;
        let phase = PHASES[self.phase as usize];
        if self.trace_cuts {
            eprintln!("w2-call {phase} {} {name} {role}", self.calls);
        }
        let eio = self.eio.as_mut()?;
        if eio.phase != self.phase || eio.ordinal != self.calls {
            return None;
        }
        let matched = eio.op == name && eio.role == role;
        eio.reached = Some(matched);
        if !matched {
            eprintln!("w2-eio {phase} {} reached {name} {role}", self.calls);
            return None;
        }
        Some(Outcome {
            error: Error { tag: 5, code: 5 },
            ..Outcome::default()
        })
    }
}

/// Select the EIO: `<phase>:<ordinal>:<operation>:<role>`, phase by name.
///
/// # Safety
/// `p` is live and quiescent before the first run; `spec` points to `len`
/// readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn store_crash_eio(p: *mut c_void, spec: *const u8, len: usize) -> u8 {
    // SAFETY: the wrapper passes its own argument buffer before any run.
    let (bridge, spec) = unsafe { (&mut *p.cast::<Bridge>(), slice::from_raw_parts(spec, len)) };
    let Ok(spec) = std::str::from_utf8(spec) else {
        return 0;
    };
    let mut fields = spec.splitn(4, ':');
    let (Some(phase), Some(ordinal), Some(op), Some(role)) =
        (fields.next(), fields.next(), fields.next(), fields.next())
    else {
        return 0;
    };
    let (Some(phase), Ok(ordinal)) = (
        PHASES.iter().position(|&name| name == phase),
        ordinal.parse(),
    ) else {
        return 0;
    };
    bridge.eio = Some(Eio {
        phase: phase as u64,
        ordinal,
        op: op.to_owned(),
        role: role.to_owned(),
        reached: None,
    });
    1
}

/// 0: the selected call was never issued; 1: it was, and EIO answered it;
/// 2: the ordinal fell on another operation or role.
///
/// # Safety
/// `p` is live and quiescent.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn store_crash_eio_reached(p: *const c_void) -> u8 {
    // SAFETY: the wrapper reads between synchronous runs.
    match unsafe { &*p.cast::<Bridge>() }
        .eio
        .as_ref()
        .and_then(|eio| eio.reached)
    {
        None => 0,
        Some(true) => 1,
        Some(false) => 2,
    }
}

/// Whether the numeric cut intercepted a write-phase step.
///
/// # Safety
/// `p` is live and quiescent.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn store_crash_intercepted(p: *const c_void) -> u8 {
    // SAFETY: the wrapper reads between synchronous runs.
    u8::from(unsafe { &*p.cast::<Bridge>() }.receipt.is_some())
}
