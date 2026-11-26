//! Returnable connection waits and joins under the interleaving checker
//! (mc-units-park-waits).
//!
//! A unit's client task is a returnable root, as the compiler lays one out:
//! its state is in its frame record, and every wait is a `_rec` row that
//! gives the stack back when it parks. Its peer is a real helper unit
//! (`spawn_unit`) on another worker, so the wakes are the runtime's own: the
//! helper's sends, receives and end reach the client's unit through its port.
//! The client receives the helper's hello, sends two items into the helper's
//! one-slot inbox (the second waits), receives two answers, joins a child
//! task of its own unit, drops its client so the helper ends, then joins the
//! helper and sees its inbox close. The unit's root task spawns and joins
//! scribblers meanwhile, which take the returned stack and overwrite it.
//!
//! The helper acts only once the client has reached a given wait. In the
//! returnable form the client marks a wait as reached after the row returned
//! parked, so each of the receive, send, task join and helper join really
//! returns its stack, and the helper's wake can land while the client is still
//! returning it. In the stackful form it marks the wait before the call.
//!
//! The law: what the client observes at each wait (outcome, item, joined
//! kind and result, and its context's available bytes after the wait), and the
//! root's available bytes at the end, equal the stackful form's; the root's
//! bytes are all back. A lost wake leaves the client parked (the liveness law
//! fails); one delivered twice runs a task twice.

use super::{FLAG, served};
use crate::Context;
use crate::tasks::*;
use crate::units_lang::{
    ClientH, Cross, InboxH, SpawnEntry, hirsel_core_client_drop, hirsel_core_conn_new,
    hirsel_core_conn_receive, hirsel_core_conn_receive_rec, hirsel_core_conn_send,
    hirsel_core_conn_send_rec, hirsel_core_inbox_drop, hirsel_core_spawn_unit,
};
use hirsel_core_layout::cross::CLIENT;
use std::ptr;
use std::sync::Mutex;
use std::sync::atomic::Ordering;

/// `{ v: u64, back: Client[Item] }`, the back client possibly null.
#[repr(C)]
#[derive(Clone, Copy)]
struct Item {
    v: u64,
    back: *mut ClientH,
}
const ITEM_OWNERS: [[u64; 4]; 1] = [[8, CLIENT, 0, 0]];
fn item() -> Cross {
    Cross {
        bytes: 16,
        owners: ITEM_OWNERS.as_ptr(),
        count: 1,
        at: 0,
        len: 16,
    }
}
const INPUT_OWNERS: [[u64; 4]; 1] = [[0, CLIENT, 0, 0]];
fn helper_input() -> Cross {
    Cross {
        bytes: 8,
        owners: INPUT_OWNERS.as_ptr(),
        count: 1,
        at: 0,
        len: 8,
    }
}
fn plain(bytes: u64) -> Cross {
    Cross {
        bytes,
        owners: ptr::null(),
        count: 0,
        at: 0,
        len: bytes,
    }
}
fn layout(input: usize, result: usize) -> TaskLayout {
    let value = |bytes| ValueLayout {
        bytes,
        align: 8,
        logical: bytes as u64,
        clean: None,
    };
    TaskLayout {
        input: value(input),
        result: value(result),
    }
}

/// The client's waits, by the phase that makes them.
const RECV_HELLO: u64 = 1;
const SEND_ONE: u64 = 2;
const SEND_TWO: u64 = 3;
const RECV_ONE: u64 = 4;
const RECV_TWO: u64 = 5;
const JOIN_CHILD: u64 = 7;
const JOIN_HELPER: u64 = 9;
const RECV_CLOSED: u64 = 10;
const WAITS: usize = 11;
/// The waits a hand-off can land on: each one's wake comes from the helper.

/// The furthest wait the client has reached (see the module docs).
static REACHED: super::seam::AtomicU64 = super::seam::AtomicU64::new(0);
/// The wait the client last parked at with its stack given back.
static PARKED: super::seam::AtomicU64 = super::seam::AtomicU64::new(0);
/// What the client observed, in order.
static LOG: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// Per wait: returned stacks.
static PARKS: Mutex<[u32; WAITS]> = Mutex::new([0; WAITS]);

fn log(s: String) {
    super::say(format!("client {s}"));
    LOG.lock().unwrap().push(s);
}
fn reach(phase: u64) {
    if REACHED.load(Ordering::SeqCst) < phase {
        REACHED.store(phase, Ordering::SeqCst);
    }
}
/// The helper's wait for the client to reach `phase`. Each load is a
/// scheduling point of the checker; the helper is a task, so it never leaves
/// its unit here (a unit yield from inside a task is refused).
fn after(phase: u64) {
    while REACHED.load(Ordering::SeqCst) < phase {}
}

/// The client's frame, homed in its record (returnable) or on its stack.
#[repr(C)]
struct Area {
    word: u64,
    phase: u64,
    child: u64,
    inbox: *mut InboxH,
    to_helper: *mut ClientH,
    helper: *mut Task,
    kid: *mut Task,
    item: Item,
    out: u64,
    /// The client handed to the helper as its input (taken by the spawn).
    give: *mut ClientH,
}
#[repr(C)]
struct Job {
    returnable: bool,
    done: *mut bool,
}

unsafe extern "C" fn kid(_: *mut Context, out: *mut u8, _: *mut u8) -> u8 {
    // SAFETY: a u64 result.
    unsafe { out.cast::<u64>().write(70) };
    0
}

/// The helper: hello, then answers each `v` with `v + 10` once the client is
/// at the matching wait, until its inbox closes.
unsafe extern "C" fn helper(c: *mut Context, out: *mut u8, input: *mut u8) -> u8 {
    // SAFETY: the input is the client's client; out is a u64.
    unsafe {
        let back = input.cast::<*mut ClientH>().read();
        let i = item();
        let made = hirsel_core_conn_new(c, 1, 64, &i, 16, 0);
        assert!(!made.inbox.is_null());
        after(RECV_HELLO);
        let mut hello = Item {
            v: 0,
            back: made.client,
        };
        let sent = hirsel_core_conn_send(c, back, (&raw mut hello).cast(), 0, true);
        assert_eq!((sent.outcome, sent.status), (0, 0));
        let mut n = 0u64;
        for at in [SEND_TWO, RECV_TWO, JOIN_HELPER] {
            after(at);
            let mut x = Item {
                v: 0,
                back: ptr::null_mut(),
            };
            let got = hirsel_core_conn_receive(c, made.inbox, (&raw mut x).cast(), 0, true);
            if got.code_and_requested != 0 {
                break;
            }
            let mut answer = Item {
                v: x.v + 10,
                back: ptr::null_mut(),
            };
            let sent = hirsel_core_conn_send(c, back, (&raw mut answer).cast(), 0, true);
            assert_eq!((sent.outcome, sent.status), (0, 0));
            n += 1;
        }
        hirsel_core_client_drop(c, back);
        hirsel_core_inbox_drop(c, made.inbox);
        out.cast::<u64>().write(n);
        0
    }
}

/// Note a wait's outcome, or its park: the stack goes back through this frame.
/// # Safety
/// `a` is the client's live area.
unsafe fn waited(context: *mut Context, a: *mut Area, job: &Job, what: String) -> bool {
    // SAFETY: by the contract.
    unsafe {
        let phase = (*a).phase;
        if job.returnable && (*a).child != 0 {
            super::say(format!("client parked returning at {phase}"));
            PARKS.lock().unwrap()[phase as usize] += 1;
            PARKED.store(phase, Ordering::SeqCst);
            reach(phase);
            return false;
        }
        reach(phase);
        // The unit's open waits: each settled wait has closed its own.
        let waits = (*crate::tasks::lang_unit()).waits;
        log(format!(
            "{phase}: {what} available={} waits={waits}",
            (*context).available()
        ));
        true
    }
}

/// Mark a wait reached before a stackful call.
fn entering(job: &Job, a: *mut Area) {
    // SAFETY: the client's live area.
    let phase = unsafe { (*a).phase };
    if !job.returnable {
        PARKED.store(phase, Ordering::SeqCst);
        reach(phase);
    }
}

unsafe extern "C" fn client(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
    // SAFETY: the unit's run pins the job through the client's end; the
    // area is the record's (returnable) or this frame's (stackful).
    unsafe {
        let job = &*input.cast::<Job>();
        let c = context;
        let mut local = std::mem::MaybeUninit::<Area>::zeroed();
        let a: *mut Area = if job.returnable {
            hirsel_core_frame_record(c, size_of::<Area>() as u64, 0, 1).cast()
        } else {
            local.as_mut_ptr()
        };
        // As the compiler's variants do: a fresh start sets its state.
        if (*a).word == 0 {
            (*a).phase = 0;
            (*a).child = 0;
        }
        (*a).word = 0;
        let recv = |a: *mut Area| {
            entering(job, a);
            let o = if job.returnable {
                hirsel_core_conn_receive_rec(
                    c,
                    &raw mut (*a).child,
                    (*a).inbox,
                    (&raw mut (*a).item).cast(),
                    1,
                )
            } else {
                hirsel_core_conn_receive(c, (*a).inbox, (&raw mut (*a).item).cast(), 1, true)
            };
            o.code_and_requested
        };
        let send = |a: *mut Area, v: u64| {
            entering(job, a);
            if (*a).child == 0 {
                (*a).item = Item {
                    v,
                    back: ptr::null_mut(),
                };
            }
            let o = if job.returnable {
                hirsel_core_conn_send_rec(
                    c,
                    &raw mut (*a).child,
                    (*a).to_helper,
                    (&raw mut (*a).item).cast(),
                    1,
                )
            } else {
                hirsel_core_conn_send(c, (*a).to_helper, (&raw mut (*a).item).cast(), 1, true)
            };
            (o.outcome, o.status)
        };
        let join = |a: *mut Area, t: *mut Task| {
            entering(job, a);
            let o = if job.returnable {
                hirsel_core_join_rec(c, &raw mut (*a).child, t, (&raw mut (*a).out).cast(), 1)
            } else {
                hirsel_core_join(c, t, (&raw mut (*a).out).cast(), 1)
            };
            (o.kind, o.status)
        };
        loop {
            match (*a).phase {
                0 => {
                    let i = item();
                    let made = hirsel_core_conn_new(c, 1, 64, &i, 16, 0);
                    (*a).inbox = made.inbox;
                    (*a).give = made.client;
                    let (xi, r) = (helper_input(), plain(8));
                    let entry = SpawnEntry {
                        bridge: helper,
                        adapter: 0,
                        layout: layout(8, 8),
                        x: xi,
                        r,
                    };
                    let spawned =
                        hirsel_core_spawn_unit(c, 0, 4096, (&raw mut (*a).give).cast(), &entry, 0);
                    assert!(
                        !spawned.task.is_null(),
                        "spawn_unit refused: requested {} available {} status {} (conn {:?} {})",
                        spawned.requested,
                        spawned.available,
                        spawned.status,
                        made.client,
                        made.status
                    );
                    (*a).helper = spawned.task;
                    log(format!("0: spawned available={}", (*c).available()));
                }
                RECV_HELLO => {
                    let o = recv(a);
                    if !waited(c, a, job, format!("hello {o} v={}", (*a).item.v)) {
                        break;
                    }
                    (*a).to_helper = (*a).item.back;
                }
                SEND_ONE | SEND_TWO => {
                    let o = send(a, (*a).phase);
                    if !waited(c, a, job, format!("sent {o:?}")) {
                        break;
                    }
                }
                RECV_ONE | RECV_TWO => {
                    let o = recv(a);
                    if !waited(c, a, job, format!("answer {o} v={}", (*a).item.v)) {
                        break;
                    }
                }
                6 => {
                    let l = layout(0, 8);
                    (*a).kid = hirsel_core_spawn(c, 0, 0, ptr::null_mut(), kid, l, 0).task;
                    assert!(!(*a).kid.is_null());
                }
                // The helper ends once this client is dropped, after the
                // child join, so its charge comes back at the same point of
                // the client's observations in both forms.
                8 => hirsel_core_client_drop(c, (*a).to_helper),
                JOIN_CHILD | JOIN_HELPER => {
                    let t = if (*a).phase == JOIN_CHILD {
                        (*a).kid
                    } else {
                        (*a).helper
                    };
                    let o = join(a, t);
                    if !waited(c, a, job, format!("joined {o:?} out={}", (*a).out)) {
                        break;
                    }
                }
                RECV_CLOSED => {
                    let o = recv(a);
                    if !waited(c, a, job, format!("closed {o}")) {
                        break;
                    }
                    hirsel_core_inbox_drop(c, (*a).inbox);
                    *job.done = true;
                    return 0;
                }
                _ => unreachable!(),
            }
            (*a).phase += 1;
            (*a).child = 0;
        }
        // A wait parked: return through the root's frame too.
        (*a).word = 1;
        0
    }
}

/// Overwrite this task's whole committed stack below a margin.
#[inline(never)]
fn scribble() {
    let mut fill = [0x5au8; 48 << 10];
    std::hint::black_box(&mut fill);
}
unsafe extern "C" fn scribbler(_: *mut Context, _: *mut u8, _: *mut u8) -> u8 {
    scribble();
    0
}
unsafe fn join_plain(context: *mut Context, task: *mut Task) {
    let mut unit = 0u8;
    // SAFETY: the parent owns the handle.
    unsafe {
        let joined = hirsel_core_join(context, task, &mut unit, 0);
        assert_eq!((joined.kind, joined.status), (0, 0));
    }
}

/// Run the client in one form; returns its log and the root's bytes before
/// and after.
fn run_form(returnable: bool) -> (Vec<String>, u64, u64) {
    super::say(format!("form returnable={returnable}"));
    LOG.lock().unwrap().clear();
    REACHED.store(0, Ordering::SeqCst);
    PARKED.store(0, Ordering::SeqCst);
    unsafe extern "C" fn root(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
        // SAFETY: run_root pins the form and its flags through the run.
        unsafe {
            let returnable = input.cast::<bool>().read();
            let mut done = false;
            let mut job = Job {
                returnable,
                done: &mut done,
            };
            let l = layout(size_of::<Job>(), 0);
            let task =
                hirsel_core_spawn(context, 0, 64 << 10, (&raw mut job).cast(), client, l, 0).task;
            assert!(!task.is_null());
            while !job.done.read_volatile() {
                let s =
                    hirsel_core_spawn(context, 0, 0, ptr::null_mut(), scribbler, layout(0, 0), 0);
                join_plain(context, s.task);
            }
            join_plain(context, task);
        }
        0
    }
    let mut context = Context::new(1 << 20);
    crate::causes::task_start(&mut context).unwrap();
    let before = context.available();
    let mut form = returnable;
    // SAFETY: the form and the context outlive the run.
    let failed = unsafe {
        run_root(
            &mut context,
            root,
            (&raw mut form).cast(),
            ptr::null_mut(),
            size_of::<bool>(),
            0,
            SchedulerProvider::default(),
        )
    };
    assert_eq!(failed, 0);
    let after = context.available();
    (std::mem::take(&mut *LOG.lock().unwrap()), before, after)
}

fn compare(stackful: &(Vec<String>, u64, u64), returnable: &(Vec<String>, u64, u64)) {
    assert_eq!(stackful.1, stackful.2, "the stackful run kept bytes");
    assert_eq!(returnable.1, returnable.2, "the returnable run kept bytes");
    assert_eq!(
        returnable.0, stackful.0,
        "the returnable client observed other outcomes, items or charges than the stackful one"
    );
}

fn reset() {
    *PARKS.lock().unwrap() = [0; WAITS];
}

pub(super) fn body(_: u32) -> u8 {
    reset();
    let stackful = run_form(false);
    assert_eq!(*PARKS.lock().unwrap(), [0; WAITS]);
    let returnable = run_form(true);
    FLAG[0].store(true, Ordering::SeqCst);
    let parks = *PARKS.lock().unwrap();
    for at in [RECV_HELLO, SEND_TWO, RECV_TWO, JOIN_CHILD, JOIN_HELPER] {
        assert!(
            parks[at as usize] > 0,
            "wait {at} never returned its stack: {parks:?}"
        );
    }
    compare(&stackful, &returnable);
    served(
        0,
        "every returnable wait gave its stack back and matched the stackful client",
    );
    0
}
