//! Tape data for Exec contains only logical launch authority and ordinals.
use super::{Args, Cursor, Fault, Out, Request, ordinal_of};
use crate::exec_runtime::{Call, LogicalLaunch, ProcCall, SpawnCall};
use hirsel_core_ids::host_op::{EXEC_SIGNAL, EXEC_SPAWN, EXEC_WAIT};
use hirsel_core_net::exec::{ArgToken, PinnedProfile};
use std::collections::BTreeMap;

pub(super) fn arguments(launch: &LogicalLaunch<'_>) -> Vec<u8> {
    let mut out = Vec::new();
    field(&mut out, launch.program.as_bytes());
    field(&mut out, launch.dir.as_bytes());
    field(&mut out, launch.path.as_bytes());
    count(&mut out, launch.args.len());
    for arg in &launch.args {
        field(&mut out, arg);
    }
    count(&mut out, launch.env.len());
    for (key, value) in &launch.env {
        field(&mut out, key.as_bytes());
        field(&mut out, value);
    }
    out.extend(launch.piped.map(u8::from));
    out
}

pub(super) unsafe fn canonical(r: &Request) -> Args {
    match r.operation {
        EXEC_SPAWN => {
            // SAFETY: the Exec adapter pins the checked launch through issue.
            let launch = unsafe { &(*r.path.cast::<Call<SpawnCall>>()).data.launch };
            Args::of(&arguments(launch))
        }
        EXEC_WAIT => {
            // SAFETY: the Exec adapter pins the wait operands through issue.
            let until = unsafe { (*r.path.cast::<Call<ProcCall>>()).data.until };
            let mut args = Vec::new();
            if let Some((_, clock, at)) = until {
                super::put_varint(&mut args, ordinal_of(clock) as u64);
                args.extend_from_slice(&at.to_le_bytes());
            } else {
                args.push(0);
            }
            Args::of(&args)
        }
        EXEC_SIGNAL => Args::of(&[r.mode]),
        _ => unreachable!(),
    }
}

pub(super) fn snapshot(profiles: &BTreeMap<String, PinnedProfile>) -> Vec<u8> {
    let mut out = Vec::new();
    count(&mut out, profiles.len());
    for (name, profile) in profiles {
        let spec = &profile.spec;
        field(&mut out, name.as_bytes());
        out.extend_from_slice(&spec.pin);
        field(&mut out, spec.argv0.as_bytes());
        count(&mut out, spec.forms.len());
        for form in &spec.forms {
            count(&mut out, form.len());
            for token in form.tokens() {
                match token {
                    ArgToken::Lit(bytes) => {
                        out.push(0);
                        field(&mut out, bytes);
                    }
                    ArgToken::Any => out.push(1),
                    ArgToken::Rest => out.push(2),
                }
            }
        }
        let mut keys: Vec<_> = spec
            .fixed
            .keys()
            .map(|key| (key, 0))
            .chain(spec.caller_keys.iter().map(|key| (key, 1)))
            .collect();
        keys.sort_unstable_by_key(|(key, _)| *key);
        count(&mut out, keys.len());
        for (key, kind) in keys {
            field(&mut out, key.as_bytes());
            out.push(kind);
        }
        count(&mut out, spec.dirs.len());
        for dir in spec.dirs.keys() {
            field(&mut out, dir.as_bytes());
        }
    }
    out
}

fn count(out: &mut Vec<u8>, n: usize) {
    out.extend_from_slice(&u32::try_from(n).unwrap().to_le_bytes());
}

fn field(out: &mut Vec<u8>, bytes: &[u8]) {
    count(out, bytes.len());
    out.extend_from_slice(bytes);
}

pub(super) fn delivery(r: &Request, out: &Out, objects: u64) -> Result<[u64; 3], Fault> {
    use super::{ABORTED, ERROR, OK, TIMED_OUT};
    if out.written != 0 {
        return Err(Fault::Corrupt);
    }
    if out.status == OK {
        if out.error != (0, 0) {
            return Err(Fault::Corrupt);
        }
        match r.operation {
            EXEC_SPAWN if out.value == objects + 1 => {
                // SAFETY: the adapter pins the launch that specified the pipe plan.
                let plan = unsafe { (*r.path.cast::<Call<SpawnCall>>()).data.launch.piped };
                let mut c = Cursor::new(&out.payload);
                let mut next = objects + 2;
                let mut pipes = [0; 3];
                for (piped, output) in plan.into_iter().zip(&mut pipes) {
                    *output = c.varint()?;
                    if piped {
                        if *output != next {
                            return Err(Fault::Corrupt);
                        }
                        next += 1;
                    } else if *output != 0 {
                        return Err(Fault::Corrupt);
                    }
                }
                if !c.done() {
                    return Err(Fault::Corrupt);
                }
                return Ok(pipes);
            }
            EXEC_WAIT if out.value <= 255 || (257..=320).contains(&out.value) => {}
            EXEC_SIGNAL if out.value == 0 => {}
            _ => return Err(Fault::Corrupt),
        }
    } else {
        if out.value != 0 {
            return Err(Fault::Corrupt);
        }
        match (r.operation, out.status, out.error) {
            (EXEC_SPAWN, ERROR, (1, 0))
            | (EXEC_WAIT, TIMED_OUT, (0, 0))
            | (EXEC_WAIT, ABORTED, (0, 0))
            | (EXEC_SPAWN | EXEC_WAIT | EXEC_SIGNAL, ERROR, (4, 1..=4095)) => {}
            _ => return Err(Fault::Corrupt),
        }
    }
    if !out.payload.is_empty() {
        return Err(Fault::Corrupt);
    }
    Ok([0; 3])
}
