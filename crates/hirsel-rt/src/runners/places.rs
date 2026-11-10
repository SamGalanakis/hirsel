//! `db@1` places in this execution (STORE-12, runners.md `db@1`), decision for decision as the
//! interpreter's `library/places.rs` makes them. A caller's `runner.send` of a transact or
//! query call to a `sim@1` runner modeling `db@1` becomes an arrival at the place its node
//! names. The place's head is a run of this execution that starts at the first call: its
//! root runs `$native.runner_main[head]`, which the compiler roots for each place head the
//! build names (`hirsel_run_schema::place_main_path`). The head takes arrivals from its
//! `Inbox` and ends each once; the end goes back to the caller's call as its completion.
//!
//! A caller's transact calls run on one call stream per runner, one request in flight; a
//! query takes position 0. A sent call is a `RunnerCall` on the caller's tape like any other
//! runner call, so replay starts no head: the recorded completion stands in for it, and the
//! stream advances from the completions the caller takes, live or replayed.
//!
//! The place is reachable while its head runs and its inbox is held. Once the head ends,
//! every call it never took is `NotRun(Unreachable())` and every call it took and did not end
//! is `InDoubt(Abandoned())`; dropping the inbox ends the untaken ones the same way, and
//! dropping an arrival ends it `InDoubt(Abandoned())`.
use std::collections::{BTreeMap, VecDeque};

use hirsel_run_schema::{CallForm, CallOutcome, RunnerCompletion, RunnerRequest};

use super::{
    ABANDONED, Callee, DENIED, EntryIdAbi, Run, Runners, Start, allocate_owned_bytes, entry_path,
    execution, root_run, runner, state,
};
use crate::fiber::Stack;
use crate::io::{AdapterError, Binding, Completion, GrantResult, Request};
use crate::io_real::RealIo;
use crate::stop::{Stop, Stopped};
use crate::views::Slice;
use crate::{Header, STATUS_OK, TaskCtx, fault};

/// The tag bit of an `Inbox` or `Arrival` handle word: the inbox is `PLACE`, arrival `n` is
/// `PLACE | n` (n ≥ 1), both of the running head's place.
pub(crate) const PLACE: u64 = 1 << 59;
/// `NotRunReason::Unreachable`.
const UNREACHABLE: u32 = 2;

/// The execution's places, by place key (the runner's node, else its name), and its callers'
/// call streams.
#[derive(Default)]
pub(super) struct Places {
    places: BTreeMap<String, Place>,
    /// Each caller run's transact stream, by (run, runner name).
    streams: BTreeMap<(usize, String), Stream>,
    /// Places whose head starts at the sending run's next idle point.
    starts: Vec<String>,
}

struct Place {
    /// The head's run, once started.
    run: Option<usize>,
    head: HeadStart,
    /// `io.inbox` granted the inbox.
    granted: bool,
    /// The inbox was dropped or the head ended: nothing more arrives.
    closed: bool,
    queued: VecDeque<Owed>,
    taken: BTreeMap<u64, Owed>,
    arrivals: u64,
    /// The head task parked in `inbox.take(i, true)`.
    waiter: Option<u32>,
}

/// What the head run starts from.
struct HeadStart {
    path: String,
    node: String,
    grants: hirsel_run_schema::IoGrants,
}

/// An arrival the place owes an end: the caller's run and wait token, and the call.
struct Owed {
    caller: usize,
    token: u64,
    call: Arrived,
}

/// A served call as `arrival.request` states it.
struct Arrived {
    form: CallForm,
    path: String,
    fingerprint: [u8; 32],
    code: [u8; 32],
    caller: String,
    stream: [u8; 32],
    epoch: u64,
    position: u64,
    digest: [u8; 32],
    acknowledged: u64,
    level: Option<String>,
    argument: Vec<u8>,
}

/// A caller's transact stream at one place: the last position with a definite end, the
/// request in flight, and the transact calls waiting for it to end.
#[derive(Default)]
struct Stream {
    applied: u64,
    in_flight: Option<u64>,
    waiting: VecDeque<(String, Owed)>,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn place_seed(node: &str, part: &str) -> u64 {
    let h = hirsel_run_schema::hash_parts("hirsel.place.v1", [node.as_bytes(), part.as_bytes()]);
    u64::from_le_bytes(h[..8].try_into().unwrap())
}

/// The running head's place key.
fn my_place(ctx: *mut TaskCtx) -> String {
    let e = execution(ctx);
    e.runs[e.active]
        .callee
        .as_ref()
        .and_then(|c| c.head.clone())
        .unwrap_or_else(|| fault("a place operation outside a place's head"))
}

fn place_of(key: &str) -> &'static mut Place {
    execution(crate::stop::running())
        .places
        .places
        .get_mut(key)
        .unwrap_or_else(|| fault("a head without its place"))
}

/// End `owed` with `outcome`: its caller's call completes, and the caller runs again.
fn end(owed: Owed, outcome: CallOutcome) {
    let e = execution(crate::stop::running());
    // SAFETY: the caller's root context is live and its binding bound.
    unsafe { Binding::get(e.runs[owed.caller].root) }
        .adapter
        .deliver(
            owed.token,
            RunnerCompletion {
                outcome,
                callee: None,
            },
        );
    e.ready(owed.caller);
}

/// `owed` arrives at place `key`: queued for its head, which runs again; a closed place
/// never runs it.
fn arrive(key: &str, owed: Owed) {
    let e = execution(crate::stop::running());
    let place = e.places.places.get_mut(key).unwrap();
    if place.closed {
        end(owed, CallOutcome::NotRun(UNREACHABLE));
        return;
    }
    place.queued.push_back(owed);
    if let Some(run) = place.run {
        e.ready(run);
    }
}

/// The head of run `me` resumed: wake its parked take when something arrived or the place
/// closed.
///
/// # Safety
/// `ctx` is the running task of run `me`.
pub(super) unsafe fn resumed(ctx: *mut TaskCtx, me: usize) {
    let e = execution(ctx);
    let Some(key) = e.runs[me].callee.as_ref().and_then(|c| c.head.clone()) else {
        return;
    };
    let place = place_of(&key);
    if (!place.queued.is_empty() || place.closed)
        && let Some(task) = place.waiter.take()
    {
        crate::tasks::wake_all(ctx, &[task]);
    }
}

/// Start the heads run `me`'s calls reached, each as a new run at the back of the ready
/// queue. Replay starts none.
///
/// # Safety
/// `ctx` is live.
pub(super) unsafe fn take_heads(ctx: *mut TaskCtx, me: usize) -> Stopped<()> {
    let starts = std::mem::take(&mut execution(ctx).places.starts);
    for key in starts {
        let e = execution(ctx);
        let Some(readiness) = e.readiness.clone() else {
            continue;
        };
        let index = e.runs.len();
        let place = e.places.places.get_mut(&key).unwrap();
        place.run = Some(index);
        let HeadStart { path, node, grants } = &place.head;
        let seed = |part: &str| place_seed(node, part);
        let lookup = |path: &str| {
            e.config
                .entries
                .iter()
                .find(|(_, p)| p == path)
                .map(|(code, _)| *code)
                .unwrap_or_else(|| fault("a place head this program does not compile"))
        };
        let environment = super::native::empty_environment();
        let start = Start {
            main: (
                lookup(&hirsel_run_schema::place_main_path(path)),
                environment,
            ),
            entry: (lookup(path), environment),
            argument: UNIT_ARGUMENT.to_vec(),
            form: CallForm::Call,
        };
        let mut instance = [0; 16];
        instance.copy_from_slice(
            &hirsel_run_schema::hash_parts("hirsel.place.v1", [node.as_bytes()])[..16],
        );
        e.tags += 1;
        let tag = e.tags;
        let stdout = e
            .config
            .stdout
            .clone()
            .unwrap_or_else(|| crate::host::Sink::new(crate::launch::RawStdout));
        // A `sim@1` place's dirs are in memory (the interpreter's simulated world): exactly
        // the granted ones, at their granted access.
        let mounts = grants
            .dirs
            .iter()
            .map(|dir| (dir.path.clone(), dir.access))
            .collect();
        let mut io = RealIo::fixture(BTreeMap::new(), mounts, vec![], stdout).map_err(|e| {
            Stop::refusal(AdapterError::Unsupported(format!("a place's dirs: {e:?}")))
        })?;
        // Its store authorities are exactly the granted ones; claims draw from the place
        // instance, as the interpreter's place world does.
        io.set_authorities(&grants.authorities, Some(instance))
            .map_err(|e| {
                Stop::refusal(AdapterError::Unsupported(format!(
                    "a place's authorities: {e:?}"
                )))
            })?;
        io.set_projections(super::projections(grants));
        io.set_cooperative(readiness.tagged(tag));
        io.clock_grants(
            grants.monotonic_clock.is_some(),
            grants.wall_clock.is_some(),
        );
        io.set_random(
            hirsel_run_schema::ProviderContract::RandomSplitMix64,
            seed("random"),
        );
        io.discard_history();
        let io = std::rc::Rc::new(std::cell::RefCell::new(io));
        let mut binding = Box::new(Binding::from_box(Box::new(crate::launch::Shared(
            io.clone(),
        ))));
        let mut config = e.config.runtime;
        config.hash_seed = seed("hash");
        let mut head = std::ptr::null_mut();
        // SAFETY: the configuration is the root's, which created a context already.
        if unsafe { crate::context::hirsel_rt_v2_ctx_create(&config, &mut head) } != 0 {
            fault("a place head's context could not be created");
        }
        // SAFETY: the head context was just created and runs nothing yet.
        unsafe {
            (*head).run().root = root_run(ctx);
            binding.bind(&mut *head);
            if let Some(table) = e.config.failure_table {
                crate::isolate::attach(&mut *head, table);
            }
        }
        let stack = Stack::new();
        let sp = stack.prepare(super::callee_entry, head.cast(), std::ptr::null_mut());
        e.runs.push(Run {
            root: head,
            resume: head,
            sp,
            ended: false,
            state: Runners {
                instance,
                ..Runners::default()
            },
            callee: Some(Callee {
                caller: me,
                token: 0,
                started: false,
                suspensions: 0,
                crash_after: None,
                start,
                reply: None,
                chain: Vec::new(),
                stack: Some(stack),
                io,
                binding,
                recording: false,
                head: Some(key),
            }),
        });
        e.ready.push_back(index);
    }
    Ok(())
}

/// The head of place `key` ended: the calls it never took never ran; those it took are in
/// doubt; later calls are unreachable.
pub(super) fn head_ended(key: &str) {
    let place = place_of(key);
    place.closed = true;
    let queued = std::mem::take(&mut place.queued);
    let taken = std::mem::take(&mut place.taken);
    for owed in queued {
        end(owed, CallOutcome::NotRun(UNREACHABLE));
    }
    for owed in taken.into_values() {
        end(owed, CallOutcome::InDoubt(ABANDONED));
    }
}

/// Drop the running head's `Inbox` or `Arrival` handle.
///
/// # Safety
/// `ctx` is the running task of a place's head, which owned `handle`.
pub(crate) unsafe fn close(ctx: *mut TaskCtx, handle: u64) -> u8 {
    let place = place_of(&my_place(ctx));
    let id = handle & !PLACE;
    if id == 0 {
        place.closed = true;
        for owed in std::mem::take(&mut place.queued) {
            end(owed, CallOutcome::NotRun(UNREACHABLE));
        }
    } else if let Some(owed) = place.taken.remove(&id) {
        end(owed, CallOutcome::InDoubt(ABANDONED));
    }
    STATUS_OK
}

/// `()` in SER-2 (`hirsel_types::wire`): the argument a head's `runner_main` decodes for its
/// `fn(rt: Io, a: ())`.
const UNIT_ARGUMENT: &[u8] = &[
    b'H', b'S', b'C', b'1', 1, 0, 0, 0, // magic, version 1
    17, 0, 0, 0, 0, 0, 0, 0, // descriptor length
    1, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 0, // descriptor: one empty tuple
    0, 0, 0, 0, 0, 0, 0, 0, // payload length
];

// ----- The caller's served call -------------------------------------------------------------

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

/// `runner.send(r, form, f, argument)`: RUN-8's caller checks, then the call on its stream
/// to the place; the place's end.
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
    let head = model
        .head
        .clone()
        .filter(|_| place && form.served() && listed);
    // SAFETY: the context is live.
    let canceled = unsafe { crate::tasks::canceled(ctx) };
    let refused = match (&head, canceled) {
        (None, _) => Some(CallOutcome::NotRun(DENIED)),
        (Some(_), Some(cause)) => Some(CallOutcome::Canceled(cause)),
        (Some(_), None) => None,
    };
    let name = model.name.clone();
    let key = model.node.clone().unwrap_or_else(|| name.clone());
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
    let request = RunnerRequest {
        runner: name.clone(),
        form,
        path: path.clone(),
        digest,
        grants: Vec::new(),
        definitions: Vec::new(),
        key: hirsel_run_schema::call_key(&instance, ordinal),
        argument: argument.clone(),
    };
    // SAFETY: the context is live and its binding bound.
    let readiness = unsafe { Binding::get(ctx) }.adapter.runner_readiness();
    let e = execution(ctx);
    e.driving = true;
    if e.active == 0 && e.readiness.is_none() {
        e.readiness = readiness;
    }
    if let Some(outcome) = refused {
        // The caller's checks decide at once; replay checks the recorded completion against
        // this decision.
        let completion = RunnerCompletion {
            outcome,
            callee: None,
        };
        // SAFETY: as above.
        let token = unsafe { crate::host_waits::next_token(ctx) };
        unsafe { Binding::get(ctx) }
            .adapter
            .deliver(token, completion.clone());
        match unsafe { crate::host_waits::send(ctx, Request::RunnerCall(Box::new(request))) }? {
            Ok(Completion::Runner(recorded)) if recorded == completion => {}
            _ => return Err(Stop::refusal(AdapterError::InvalidCompletion)),
        }
        // SAFETY: the context is live.
        unsafe { crate::tasks::try_yield(&mut *ctx) }?;
        return Ok(completion.outcome);
    }
    let head = head.unwrap();
    let me = e.active;
    let live = e.readiness.is_some();
    let place = e.places.places.entry(key.clone()).or_insert_with(|| Place {
        run: None,
        head: HeadStart {
            path: head,
            node: key.clone(),
            grants: grant.grants.clone(),
        },
        granted: false,
        closed: false,
        queued: VecDeque::new(),
        taken: BTreeMap::new(),
        arrivals: 0,
        waiter: None,
    });
    if live && place.run.is_none() && !e.places.starts.contains(&key) {
        e.places.starts.push(key.clone());
    }
    // A closed place's call is sent too: it ends `NotRun(Unreachable())` as it arrives, and
    // replay takes that end from the tape, never having run the head.
    // SAFETY: as above.
    let token =
        match unsafe { crate::host_waits::send(ctx, Request::RunnerCall(Box::new(request))) }? {
            Err(token) => token,
            _ => return Err(Stop::refusal(AdapterError::InvalidCompletion)),
        };
    let e = execution(ctx);
    let stream = e.places.streams.entry((me, name.clone())).or_default();
    let owed = Owed {
        caller: me,
        token,
        call: Arrived {
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
            position: if form == CallForm::Transact {
                stream.applied + 1
            } else {
                0
            },
            digest,
            acknowledged: stream.applied,
            level,
            argument,
        },
    };
    let transact = form == CallForm::Transact;
    if transact && stream.in_flight.is_some() {
        // One request in flight per stream: this one waits for its position.
        stream.waiting.push_back((key, owed));
    } else {
        if transact {
            stream.in_flight = Some(token);
        }
        // Replay routes nothing: the recorded completion stands in for the place.
        if live {
            arrive(&key, owed);
        }
    }
    // SAFETY: caller contract.
    let completion = match unsafe { crate::host_waits::await_sent(ctx, token) }? {
        Completion::Runner(completion) => completion,
        _ => return Err(Stop::refusal(AdapterError::InvalidCompletion)),
    };
    if transact {
        // A definite end, or one that may have committed, consumes its position; the next
        // waiting transact call goes at the position after it.
        let e = execution(ctx);
        let stream = e.places.streams.get_mut(&(me, name)).unwrap();
        if matches!(
            completion.outcome,
            CallOutcome::Ok(_) | CallOutcome::Failed(_) | CallOutcome::InDoubt(_)
        ) {
            stream.applied += 1;
        }
        stream.in_flight = None;
        if let Some((key, mut owed)) = stream.waiting.pop_front() {
            owed.call.position = stream.applied + 1;
            owed.call.acknowledged = stream.applied;
            stream.in_flight = Some(owed.token);
            if e.readiness.is_some() {
                arrive(&key, owed);
            }
        }
    }
    Ok(completion.outcome)
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
    if unsafe { (*ctx).run_ref().free_production } {
        fault("db@1 places run natively only in the harness execution");
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
    let e = execution(ctx);
    let head = e.runs[e.active]
        .callee
        .as_ref()
        .and_then(|c| c.head.clone());
    let result = match head.map(|key| place_of(&key)) {
        Some(place) if !place.granted => {
            place.granted = true;
            Ok(PLACE)
        }
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
    let key = my_place(ctx);
    loop {
        let place = place_of(&key);
        if !place.queued.is_empty() || place.closed || wait == 0 {
            break;
        }
        place.waiter = Some(crate::tasks::current_task(ctx));
        crate::tasks::park_retry(ctx, false);
    }
    let place = place_of(&key);
    let queued = std::mem::take(&mut place.queued);
    let mut words = Vec::with_capacity(queued.len());
    for owed in queued {
        place.arrivals += 1;
        place.taken.insert(place.arrivals, owed);
        words.push(PLACE | place.arrivals);
    }
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
    let place = place_of(&my_place(ctx));
    let id = unsafe { arrival.read() } & !PLACE;
    let call = &place
        .taken
        .get(&id)
        .unwrap_or_else(|| fault("an Arrival already ended"))
        .call;
    let form = match call.form {
        CallForm::Compute => 0,
        CallForm::Call => 1,
        CallForm::Transact => 2,
        CallForm::Query => 3,
    };
    let (path, fingerprint, code) = (call.path.clone(), call.fingerprint, call.code);
    let (caller, stream, digest) = (call.caller.clone(), call.stream, call.digest);
    let (epoch, position, acknowledged) = (call.epoch, call.position, call.acknowledged);
    let (level, argument) = (call.level.clone(), call.argument.clone());
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
    let place = place_of(&my_place(ctx));
    let id = unsafe { arrival.read() } & !PLACE;
    let owed = place
        .taken
        .remove(&id)
        .unwrap_or_else(|| fault("an Arrival ended twice"));
    end(owed, unsafe { read_end(call_end) });
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
