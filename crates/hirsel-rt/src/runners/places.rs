//! `db@1` places (STORE-12, runners.md `db@1`), decision for decision as the interpreter's
//! `library/places.rs` makes them. A caller's `runner.send` of a transact or query call to a
//! `sim@1` runner modeling `db@1` goes to home like any sent runner call; home routes it on the
//! caller's stream to the place its node names ([`super::native::place`]). The place's head
//! is a run home starts at the first call: its root runs `$native.runner_main[head]`, which the
//! compiler roots for each place head the build names (`hirsel_run_schema::place_main_path`).
//! The head takes arrivals from its `Inbox` and ends each once; home gives the end to the
//! caller's call as its completion.
//!
//! A sent call is a `RunnerCall` on the caller's tape like any other runner call, so replay
//! starts no head: the recorded completion stands in for it.
//!
//! The place is reachable while its head runs and its inbox is held. Once the head ends,
//! every call it never took is `NotRun(Unreachable())` and every call it took and did not end
//! is `InDoubt(Abandoned())`; dropping the inbox ends the untaken ones the same way, and
//! dropping an arrival ends it `InDoubt(Abandoned())`.
use std::sync::Arc;

use hirsel_run_schema::{CallForm, CallOutcome, RunnerRequest};

use super::native::place::{Arrived, Place, Take};
use super::{
    DENIED, EntryIdAbi, allocate_owned_bytes, entry_path, execution, native, runner, state,
};
use crate::io::GrantResult;
use crate::stop::Stopped;
use crate::views::Slice;
use crate::{Header, STATUS_OK, TaskCtx, fault};

/// The tag bit of an `Inbox` or `Arrival` handle word: the inbox is `PLACE`, arrival `n` is
/// `PLACE | n` (n ≥ 1), both of the running head's place.
pub(crate) const PLACE: u64 = 1 << 59;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The place the running head serves.
fn place(ctx: *mut TaskCtx) -> &'static Arc<Place> {
    native::call(ctx)
        .and_then(|call| call.place.as_ref())
        .unwrap_or_else(|| fault("a place operation outside a place's head"))
}

/// Drop the running head's `Inbox` or `Arrival` handle.
///
/// # Safety
/// `ctx` is the running task of a place's head, which owned `handle`.
pub(crate) unsafe fn close(ctx: *mut TaskCtx, handle: u64) -> u8 {
    place(ctx).close(handle & !PLACE);
    STATUS_OK
}

/// `()` in SER-2 (`hirsel_types::wire`): the argument a head's `runner_main` decodes for its
/// `fn(rt: Io, a: ())`.
pub(super) const UNIT_ARGUMENT: &[u8] = &[
    b'H', b'S', b'C', b'1', 1, 0, 0, 0, // magic, version 1
    17, 0, 0, 0, 0, 0, 0, 0, // descriptor length
    1, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 0, // descriptor: one empty tuple
    0, 0, 0, 0, 0, 0, 0, 0, // payload length
];

/// `CallEnd` as compiled code lays it out: the u32 tag, then the variant's `Bytes` or reason.
#[repr(C)]
pub struct CallEndAbi {
    tag: u32,
    payload: Slice,
}

/// Write `outcome` as a fresh `CallEnd` at `out`.
///
/// # Safety
/// `out` is writable for a `CallEnd`.
unsafe fn write_end(ctx: *mut TaskCtx, out: *mut CallEndAbi, outcome: CallOutcome) {
    let (tag, payload) = match outcome {
        CallOutcome::Ok(b) => (0, Ok(b)),
        CallOutcome::Failed(b) => (1, Ok(b)),
        CallOutcome::Faulted(b) => (2, Ok(b)),
        CallOutcome::NotRun(r) => (3, Err(r)),
        CallOutcome::InDoubt(r) => (4, Err(r)),
        CallOutcome::Canceled(c) => (5, Err(c)),
    };
    unsafe {
        (&raw mut (*out).tag).write(tag);
        match payload {
            Ok(bytes) => (&raw mut (*out).payload).write(allocate_owned_bytes(ctx, bytes)),
            Err(reason) => (&raw mut (*out).payload).cast::<u32>().write(reason),
        }
    }
}

/// The `CallEnd` at `end`, borrowed.
///
/// # Safety
/// `end` is a live `CallEnd`.
unsafe fn read_end(end: *const CallEndAbi) -> CallOutcome {
    unsafe {
        let tag = (&raw const (*end).tag).read();
        let bytes = || {
            let s = (&raw const (*end).payload).read();
            crate::io::slice_bytes(s.owner, s.start, s.length).to_vec()
        };
        let reason = || (&raw const (*end).payload).cast::<u32>().read();
        match tag {
            0 => CallOutcome::Ok(bytes()),
            1 => CallOutcome::Failed(bytes()),
            2 => CallOutcome::Faulted(bytes()),
            3 => CallOutcome::NotRun(reason()),
            4 => CallOutcome::InDoubt(reason()),
            5 => CallOutcome::Canceled(reason()),
            _ => fault("a CallEnd with an unknown variant"),
        }
    }
}

/// `runner.send(r, form, f, argument)`: RUN-8's caller checks, then the call to home, which
/// routes it to the place; the place's end.
///
/// # Safety
/// `ctx` is live and the caller holds nothing a stop must drop.
unsafe fn send(
    ctx: *mut TaskCtx,
    word: u64,
    form: CallForm,
    entry: usize,
    argument: Vec<u8>,
) -> Stopped<CallOutcome> {
    let grant = runner(ctx, word);
    let path = entry_path(ctx, entry);
    let model = &grant.entry;
    let place = model.models.as_deref() == Some("db@1") && model.provider == "sim@1";
    let listed = grant
        .definitions
        .as_ref()
        .is_none_or(|allowed| allowed.contains(&path));
    let served = place && form.served() && listed && model.head.is_some();
    // SAFETY: the context is live.
    let canceled = unsafe { crate::tasks::canceled(ctx) };
    let name = model.name.clone();
    let level = model.level.clone();
    let (fingerprint, code) = execution(ctx)
        .config
        .callees
        .get(&path)
        .map_or(([0; 32], [0; 32]), |f| (f.fingerprint, f.code));
    let digest =
        hirsel_run_schema::request_digest(form, &path, &fingerprint, &code, &[], &[], &argument);
    let run = state(ctx);
    let instance = run.instance;
    let ordinal = run.requests;
    run.requests += 1;
    let key = hirsel_run_schema::call_key(&instance, ordinal);
    let request = RunnerRequest {
        runner: name.clone(),
        form,
        path: path.clone(),
        digest,
        grants: Vec::new(),
        definitions: Vec::new(),
        key,
        argument: argument.clone(),
    };
    // The caller's checks decide at once; replay checks the recorded completion against
    // this decision. Home assigns the stream position as the call arrives.
    let work = match (served, canceled) {
        (false, _) => Err(CallOutcome::NotRun(DENIED)),
        (true, Some(cause)) => Err(CallOutcome::Canceled(cause)),
        (true, None) => Ok(native::Work::Served(Arrived {
            form,
            path,
            fingerprint,
            code,
            caller: hex(&instance),
            stream: hirsel_run_schema::hash_parts(
                "hirsel.stream.v1",
                [name.as_bytes(), &instance[..]],
            ),
            epoch: 1,
            position: 0,
            digest,
            acknowledged: 0,
            level,
            argument,
        })),
    };
    // SAFETY: caller contract.
    let ticket = unsafe { native::send(ctx, request, grant, ordinal, work) }?;
    unsafe { native::wait(ctx, ticket) }?;
    let call = state(ctx)
        .calls
        .remove(&ticket)
        .unwrap_or_else(|| fault("a served call without its ticket"));
    Ok(call
        .completion
        .unwrap_or_else(|| fault("a served call ended without its completion"))
        .outcome)
}

// ----- The Hirsel-ABI entries ---------------------------------------------------------------

/// `runner.send[F](r, form, f, argument) -> CallEnd`: a served call of `f` with `argument`'s
/// bytes at `r`'s place.
///
/// # Safety
/// The context is live, `r` a borrowed Runner word, `form` a borrowed `CallForm`, `entry` a
/// borrowed Entry, the argument a live Bytes and `out` writable for a `CallEnd`.
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn hirsel_rt_v2_place_send(
    ctx: *mut TaskCtx,
    _destination: *mut u8,
    out: *mut CallEndAbi,
    r: *const u64,
    form: *const u32,
    entry: *const usize,
    owner: *mut Header,
    start: u64,
    length: u64,
) -> u8 {
    // SAFETY: this semantic entry owns its task until it suspends or returns.
    let _role = unsafe { crate::tasks::role::Guard::new(ctx) };
    if !unsafe { (*ctx).run_ref().free_production } {
        fault("db@1 places run natively only under tasks.free@1");
    }
    let form = match unsafe { form.read() } {
        0 => CallForm::Compute,
        1 => CallForm::Call,
        2 => CallForm::Transact,
        3 => CallForm::Query,
        _ => fault("a CallForm with an unknown variant"),
    };
    // SAFETY: caller contract.
    unsafe {
        let argument = crate::io::slice_bytes(owner, start, length).to_vec();
        let outcome = crate::stop::check(ctx, send(ctx, r.read(), form, entry.read(), argument));
        write_end(ctx, out, outcome);
    }
    STATUS_OK
}

/// `io.inbox(rt) -> Result[Inbox, GrantError]`: the place's inbox, once, to its head.
///
/// # Safety
/// The context is live, `io` a borrowed Io word and `out` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_rt_v2_io_inbox(
    ctx: *mut TaskCtx,
    _destination: *mut u8,
    out: *mut GrantResult,
    _io: *const u64,
) -> u8 {
    // SAFETY: this semantic entry owns its task until it suspends or returns.
    let _role = unsafe { crate::tasks::role::Guard::new(ctx) };
    let head = native::call(ctx).and_then(|call| call.place.as_ref());
    let result = match head {
        Some(place) if place.grant() => Ok(PLACE),
        _ => Err(crate::io::IoError::NotGranted),
    };
    unsafe {
        out.write(GrantResult::of(result));
        crate::tasks::yield_now(&mut *ctx);
    }
    STATUS_OK
}

/// `inbox.take(mut i, wait) -> Option[List[Arrival]]`: every queued arrival, as Arrival
/// handles; `wait` parks the head until one arrives or the place closes.
///
/// # Safety
/// The context is live, `inbox` the head's Inbox and `out` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_rt_v2_inbox_take(
    ctx: *mut TaskCtx,
    _destination: *mut u8,
    out: *mut *mut Header,
    _inbox: *const u64,
    wait: u8,
) -> u8 {
    // SAFETY: this semantic entry owns its task until it suspends or returns.
    let _role = unsafe { crate::tasks::role::Guard::new(ctx) };
    let place = place(ctx);
    let ids = loop {
        let waiter = crate::tasks::free::home::runner_waiter(ctx);
        match place.take(wait != 0, waiter) {
            Take::Arrivals(ids) => break ids,
            // Home wakes the take when a call arrives or the place closes.
            Take::Park => crate::tasks::park_retry(ctx, false),
        }
    };
    let words: Vec<u64> = ids.into_iter().map(|id| PLACE | id).collect();
    // SAFETY: the context is live; the list holds the arrivals the take hands over.
    unsafe {
        let block = crate::alloc::allocate_sequence(
            ctx,
            crate::text::handle_list_index(ctx),
            words.len() as u64,
            8,
            8,
        );
        let first = crate::alloc::payload(block)
            .add(crate::alloc::sequence_elements_offset(8) as usize)
            .cast::<u64>();
        for (index, word) in words.into_iter().enumerate() {
            first.add(index).write(word);
        }
        out.write(block);
        crate::tasks::yield_now(&mut *ctx);
    }
    STATUS_OK
}

/// `Arrived` as compiled code lays it out.
#[repr(C)]
pub struct ArrivedAbi {
    form: u32,
    entry: EntryIdAbi,
    caller: Slice,
    stream: Slice,
    epoch: u64,
    position: u64,
    digest: Slice,
    acknowledged: u64,
    level: Slice,
    argument: Slice,
}

/// `arrival.request(a) -> Arrived`: the served call, fresh in the head's run.
///
/// # Safety
/// The context is live, `arrival` a borrowed Arrival of the running head and `out` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_rt_v2_arrival_request(
    ctx: *mut TaskCtx,
    _destination: *mut u8,
    out: *mut ArrivedAbi,
    arrival: *const u64,
) -> u8 {
    // SAFETY: this semantic entry owns its task until it suspends or returns.
    let _role = unsafe { crate::tasks::role::Guard::new(ctx) };
    let id = unsafe { arrival.read() } & !PLACE;
    let call = place(ctx)
        .request(id)
        .unwrap_or_else(|| fault("an Arrival already ended"));
    let form = match call.form {
        CallForm::Compute => 0,
        CallForm::Call => 1,
        CallForm::Transact => 2,
        CallForm::Query => 3,
    };
    let Arrived {
        path,
        fingerprint,
        code,
        caller,
        stream,
        epoch,
        position,
        digest,
        acknowledged,
        level,
        argument,
        ..
    } = call;
    unsafe {
        let bytes = |b: Vec<u8>| allocate_owned_bytes(ctx, b);
        out.write(ArrivedAbi {
            form,
            entry: EntryIdAbi {
                path: bytes(path.into_bytes()),
                fingerprint: bytes(fingerprint.to_vec()),
                code: bytes(code.to_vec()),
            },
            caller: bytes(caller.into_bytes()),
            stream: bytes(stream.to_vec()),
            epoch,
            position,
            digest: bytes(digest.to_vec()),
            acknowledged,
            // `Option[String]`'s None is the null owner.
            level: level.map_or(
                Slice {
                    owner: std::ptr::null_mut(),
                    start: 0,
                    length: 0,
                },
                |level| bytes(level.into_bytes()),
            ),
            argument: bytes(argument),
        });
    }
    STATUS_OK
}

/// `arrival.end(take a, end)`: the arrival's end, given once; its caller's call completes.
///
/// # Safety
/// The context is live, `arrival` an Arrival of the running head the call takes and `end` a
/// borrowed CallEnd.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_rt_v2_arrival_end(
    ctx: *mut TaskCtx,
    arrival: *const u64,
    call_end: *const CallEndAbi,
) -> u8 {
    // SAFETY: this semantic entry owns its task until it suspends or returns.
    let _role = unsafe { crate::tasks::role::Guard::new(ctx) };
    let id = unsafe { arrival.read() } & !PLACE;
    if !place(ctx).end(id, unsafe { read_end(call_end) }) {
        fault("an Arrival ended twice");
    }
    unsafe { crate::tasks::yield_now(&mut *ctx) };
    STATUS_OK
}

#[cfg(test)]
mod tests {
    /// A head starts with the reference codec's encoding of `()`; any other bytes fault the
    /// head's `runner_main` as undecodable before its body runs.
    #[test]
    fn unit_argument_is_the_reference_encoding_of_unit() {
        use hirsel_types::serialization::{SchemaId, SchemaNode, SerializableSchema};
        use hirsel_types::wire::{Codec, Limits, Value, ValueId, ValueNode};
        let schema = SerializableSchema {
            version: 1,
            root: SchemaId(0),
            nodes: vec![SchemaNode::Tuple(vec![])],
        };
        let unit = Value {
            root: ValueId(0),
            nodes: vec![ValueNode::Fields(vec![])],
        };
        let expected = Codec::new(&schema)
            .unwrap()
            .encode(&unit, Limits::default())
            .unwrap();
        assert_eq!(super::UNIT_ARGUMENT, expected.as_slice(), "{expected:?}");
    }
}
