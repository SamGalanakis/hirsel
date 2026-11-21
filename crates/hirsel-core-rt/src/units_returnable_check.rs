//! The returnable boundary under the interleaving checker (mc-units-receive-park).
//!
//! A unit runs a task scheduler whose client task waits at an owned network
//! boundary in the returnable form: it keeps its frames in its frame record
//! (`hirsel_core_frame_record`), as the compiler lays them out, and gives its
//! stack back when the receive parks. The client is two frames: a lender that
//! owns the receive carrier (a homed slot in its record area) and a callee
//! that borrows it and loops over the bounded receive, as `recv_exact` does.
//! While it waits, a second task reuses the released stack and overwrites all
//! of it. A checker producer thread is the provider's outside readiness: it
//! readies each pending token after a drawn delay, through
//! `hirsel_core_ready`, the provider ABI's own wake.
//!
//! The law: the provider's callback sequence (with the buffer lengths and the
//! paired-send bytes it reads through the request) and the final carrier
//! lengths are the same as for the same client with the boundary disabled
//! (the stackful rows, every frame on the stack). The scripted operations
//! cover a short read that keeps its progress, a resume that is Pending
//! again, a paired send, readiness given inside `issue` (before the task
//! returns its stack), EOF, and a cancellation after issue.
use super::{FLAG, now, served, sleep_producer};
use crate::Context;
use crate::provider::{
    Buffer, IoError, Outcome, Provider, Request, STEP_DONE, STEP_PENDING, Step, WAIT_READY, Wait,
};
use crate::tasks::*;
use std::ptr;
use std::sync::Mutex;
use std::sync::atomic::Ordering;

/// The client's socket key: its read half; `| 1` is the write half.
const KEY: u64 = 0x40;
/// Carrier capacity; the client receives 8, sends, receives 4, then sees EOF.
const CAP: usize = 16;

/// Everything the provider observed, in order, for one run.
static LOG: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// The pending token the producer is to ready (0: none).
static PENDING: super::seam::AtomicU64 = super::seam::AtomicU64::new(0);
/// Operations issued in this run, so each finds its scripted answers.
static OPS: Mutex<Vec<Script>> = Mutex::new(Vec::new());

/// One operation's scripted answers to `resume`, by its issue order.
#[derive(Clone, Copy)]
struct Script {
    /// Readies its own token while `issue` runs.
    ready_in_issue: bool,
    /// Resumes answering Pending before the answer.
    pending_resumes: u8,
    /// Bytes a receive appends (0 is EOF).
    bytes: usize,
    /// Left pending for a cancellation: `issue` tells the root, not the producer.
    cancel: bool,
}
const SCRIPT: [Script; 6] = [
    // recv_exact(8): a short read of 3 after a spurious wake ...
    Script { ready_in_issue: false, pending_resumes: 1, bytes: 3, cancel: false },
    // ... and the rest, with readiness given before the stack is returned.
    Script { ready_in_issue: true, pending_resumes: 0, bytes: 5, cancel: false },
    // send_pair(carrier, tail).
    Script { ready_in_issue: false, pending_resumes: 0, bytes: 0, cancel: false },
    // recv_exact(12): 4 more.
    Script { ready_in_issue: false, pending_resumes: 0, bytes: 4, cancel: false },
    // recv: EOF.
    Script { ready_in_issue: false, pending_resumes: 0, bytes: 0, cancel: false },
    // The second client's receive, cancelled after issue.
    Script { ready_in_issue: false, pending_resumes: 0, bytes: 0, cancel: true },
];

fn log(s: String) {
    LOG.lock().unwrap().push(s);
}

/// The request's operation index: the client passes its issue order as the site.
fn script_of(request: *mut Request) -> (usize, Script) {
    let ops = OPS.lock().unwrap();
    // SAFETY: the provider reads the request it was handed, pinned until delivery.
    let site = unsafe { (*request).site } as usize;
    (site, ops[site])
}

unsafe extern "C" fn p_wait(_: *mut u8, request: *mut Request, _: u64) -> Wait {
    let (op, _) = script_of(request);
    log(format!("W{op}"));
    Wait { kind: WAIT_READY, deadline_ns: 0 }
}
unsafe extern "C" fn p_issue(_: *mut u8, request: *mut Request, token: u64) -> Step {
    let (op, script) = script_of(request);
    // SAFETY: pinned through delivery.
    let len = unsafe { (*request).buffer.len };
    log(format!("I{op} len={len}"));
    if script.ready_in_issue {
        hirsel_core_ready(token);
    } else if script.cancel {
        FLAG[1].store(true, Ordering::SeqCst);
    } else {
        PENDING.store(token, Ordering::SeqCst);
    }
    Step { kind: STEP_PENDING, ..Step::default() }
}
static RESUMES: Mutex<Vec<u8>> = Mutex::new(Vec::new());
unsafe extern "C" fn p_resume(_: *mut u8, request: *mut Request, token: u64) -> Step {
    let (op, script) = script_of(request);
    let seen = {
        let mut r = RESUMES.lock().unwrap();
        let n = r.len().max(op + 1);
        r.resize(n, 0);
        r[op] += 1;
        r[op]
    };
    // SAFETY: the provider owns the request's buffer extent until delivery,
    // and reads the paired-send tail through the network request's leading word.
    unsafe {
        let b = (*request).buffer;
        if seen <= script.pending_resumes {
            log(format!("R{op} pending len={}", b.len));
            PENDING.store(token, Ordering::SeqCst);
            return Step { kind: STEP_PENDING, ..Step::default() };
        }
        let value = if (*request).operation == crate::provider::SEND_PAIR {
            let tail = *(*request).path.cast::<*const Buffer>();
            let prefix = std::slice::from_raw_parts(b.data, b.len);
            let tail = std::slice::from_raw_parts((*tail).data, (*tail).len);
            log(format!("R{op} send {prefix:?} {tail:?}"));
            (prefix.len() + tail.len()) as u64
        } else {
            let k = script.bytes.min(b.capacity - b.len);
            for i in 0..k {
                *b.data.add(b.len + i) = (op * 16 + i) as u8;
            }
            (*request).buffer.len = b.len + k;
            log(format!("R{op} recv {k} len={}", b.len + k));
            k as u64
        };
        Step {
            kind: STEP_DONE,
            outcome: Outcome { value, ..Outcome::default() },
            deadline_ns: 0,
        }
    }
}
unsafe extern "C" fn p_abort(_: *mut u8, request: *mut Request, _: u64, reason: u8) -> Outcome {
    let (op, _) = script_of(request);
    log(format!("A{op} reason={reason}"));
    Outcome { error: IoError { tag: 7, code: 0 }, ..Outcome::default() }
}
unsafe extern "C" fn p_deliver(_: *mut u8, request: *mut Request, outcome: Outcome) -> Outcome {
    let (op, _) = script_of(request);
    // SAFETY: pinned through delivery.
    let len = unsafe { (*request).buffer.len };
    log(format!("D{op} value={} error={} len={len}", outcome.value, outcome.error.tag));
    outcome
}
const PROVIDER: Provider = Provider {
    state: ptr::null_mut(),
    wait: p_wait,
    issue: p_issue,
    resume: p_resume,
    abort: p_abort,
    deliver: p_deliver,
};

/// What the compiler lays out for the two client frames: the lender's site
/// word and homed carrier, then the callee's area (its site word).
#[repr(C)]
struct Area {
    word: u64,
    phase: u64,
    lender: Buffer,
    child: u64,
}
#[repr(C)]
struct Job {
    returnable: bool,
    /// The operation index each phase issues next (the script's key).
    next_op: *mut usize,
    bytes: *mut u8,
    /// Times the client returned its stack.
    parks: *mut u64,
    /// The carrier's length after each phase.
    lens: *mut Vec<usize>,
    /// Set when the client ends.
    done: *mut bool,
}
static TAIL: [u8; 3] = *b"end";

/// A receive or a paired send at the boundary, in the form the job runs.
/// # Safety
/// `word` and `carrier` live in the task's frame record (returnable) or on
/// its stack (stackful).
unsafe fn boundary(
    context: *mut Context,
    job: &Job,
    word: *mut u64,
    carrier: *mut Buffer,
    send: bool,
    max: u64,
) -> Outcome {
    // SAFETY: by the contract; the site carries the script key.
    unsafe {
        if *word == 0 {
            OPS.lock().unwrap().push(SCRIPT[*job.next_op]);
            *job.next_op += 1;
        }
        let site = OPS.lock().unwrap().len() as u64 - 1;
        let tail = Buffer { data: TAIL.as_ptr().cast_mut(), len: 3, capacity: 0 };
        match (job.returnable, send) {
            (true, false) => crate::net::hirsel_core_host_recv_bounded_rec(
                context, word, KEY, carrier, max, 0, 0, 0, site,
            ),
            (true, true) => crate::net::hirsel_core_host_send_pair_rec(
                context, word, KEY | 1, carrier, &tail, 0, 0, 0, site,
            ),
            (false, false) => {
                crate::net::hirsel_core_host_recv_bounded(context, KEY, carrier, max, 0, 0, 0, site)
            }
            (false, true) => {
                crate::net::hirsel_core_host_send_pair(context, KEY | 1, carrier, &tail, 0, 0, 0, site)
            }
        }
    }
}

/// The callee: borrows the lender's carrier and receives until it holds
/// `want` bytes. False: EOF, an error, or (with its word set) a park.
/// # Safety
/// As `boundary`.
unsafe fn recv_exact(context: *mut Context, job: &Job, word: *mut u64, carrier: *mut Buffer, want: usize) -> bool {
    // SAFETY: by the contract.
    unsafe {
        while *word != 0 || (*carrier).len < want {
            let out = boundary(context, job, word, carrier, false, (want - (*carrier).len) as u64);
            if *word != 0 || out.error != IoError::OK || out.status != 0 || out.value == 0 {
                return false;
            }
        }
        true
    }
}

unsafe extern "C" fn client(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
    // SAFETY: the unit's run pins the job through the client's end.
    unsafe {
        let job = &*input.cast::<Job>();
        let mut local = std::mem::MaybeUninit::<Area>::zeroed();
        let a: *mut Area = if job.returnable {
            hirsel_core_frame_record(context, size_of::<Area>() as u64).cast()
        } else {
            local.as_mut_ptr()
        };
        if (*a).word == 0 {
            (*a).lender = Buffer { data: job.bytes, len: 0, capacity: CAP };
        }
        (*a).word = 0;
        loop {
            let phase = (*a).phase;
            let more = match phase {
                0 => recv_exact(context, job, &raw mut (*a).child, &raw mut (*a).lender, 8),
                1 => {
                    let out = boundary(context, job, &raw mut (*a).child, &raw mut (*a).lender, true, 0);
                    (*a).child == 0 && out.error == IoError::OK
                }
                2 => recv_exact(context, job, &raw mut (*a).child, &raw mut (*a).lender, 12),
                3 => recv_exact(context, job, &raw mut (*a).child, &raw mut (*a).lender, 13),
                _ => false,
            };
            if (*a).child != 0 {
                // The callee parked: return through the lender's frame too.
                (*a).word = 1;
                *job.parks += 1;
                return 0;
            }
            (*job.lens).push((*a).lender.len);
            if !more {
                *job.done = true;
                return 0;
            }
            (*a).phase += 1;
        }
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

fn layout(bytes: usize) -> TaskLayout {
    let value = |bytes| ValueLayout { bytes, align: 8, logical: 0, clean: None };
    TaskLayout { input: value(bytes), result: value(0) }
}
unsafe fn join(context: *mut Context, task: *mut Task, logged: bool) {
    let mut unit = 0u8;
    // SAFETY: the parent owns the handle; a failed task's causes are released.
    unsafe {
        let joined = hirsel_core_join(context, task, &mut unit, 1);
        assert_eq!(joined.status, 0);
        if logged {
            log(format!("joined kind={}", joined.kind));
        }
        if joined.kind != 0 {
            drop(crate::causes::Causes::from_raw(joined.causes));
        }
    }
}

/// Run both clients in one form; returns the provider log and carrier lengths.
fn run_form(returnable: bool) -> (Vec<String>, Vec<usize>, Vec<usize>, u64) {
    LOG.lock().unwrap().clear();
    OPS.lock().unwrap().clear();
    RESUMES.lock().unwrap().clear();
    struct Root {
        returnable: bool,
        out: *mut (Vec<usize>, Vec<usize>, u64),
    }
    unsafe extern "C" fn root(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
        // SAFETY: run_root pins Root and its outputs through the run.
        unsafe {
            let root = input.cast::<Root>().read();
            let (lens, cancelled, parks) = &mut *root.out;
            let mut next_op = 0usize;
            let mut bytes = [0u8; CAP];
            let mut done = false;
            let mut job = Job {
                returnable: root.returnable,
                next_op: &mut next_op,
                bytes: bytes.as_mut_ptr(),
                parks,
                lens,
                done: &mut done,
            };
            let client_task = hirsel_core_spawn(
                context, 0, 64, (&mut job as *mut Job).cast(), client, layout(size_of::<Job>()), 0,
            )
            .task;
            // While the client waits, the root keeps running a fresh task that
            // takes the stock's most recent stack (the one the returnable
            // client gave back) and overwrites it. Each run is a dispatcher
            // pass, where the outside provider gets its turn.
            while !*job.done {
                scribble_turn(context);
            }
            join(context, client_task, true);
            // The second client: cancelled after its receive is issued.
            next_op = 5;
            let mut bytes2 = [0u8; CAP];
            let mut parks2 = 0;
            let mut done2 = false;
            let mut job2 = Job {
                returnable: root.returnable,
                next_op: &mut next_op,
                bytes: bytes2.as_mut_ptr(),
                parks: &mut parks2,
                lens: cancelled,
                done: &mut done2,
            };
            let second = hirsel_core_spawn(
                context, 0, 64, (&mut job2 as *mut Job).cast(), client, layout(size_of::<Job>()), 0,
            )
            .task;
            while !FLAG[1].load(Ordering::SeqCst) {
                scribble_turn(context);
            }
            FLAG[1].store(false, Ordering::SeqCst);
            hirsel_core_cancel(context, second);
            join(context, second, true);
            *parks += parks2;
        }
        0
    }
    let mut out = (Vec::new(), Vec::new(), 0u64);
    let mut context = Context::new(1 << 20);
    let mut files = crate::host_file::Files::default();
    files.connected.insert(KEY, (crate::net::Address::default(), PROVIDER));
    context.host_files = &mut files;
    crate::causes::task_start(&mut context).unwrap();
    let mut input = Root { returnable, out: &mut out };
    // SAFETY: Root, its outputs and the context outlive the run.
    let failed = unsafe {
        run_root(
            &mut context,
            root,
            (&mut input as *mut Root).cast(),
            ptr::null_mut(),
            size_of::<Root>(),
            0,
            SchedulerProvider::default(),
        )
    };
    assert_eq!(failed, 0);
    let log = std::mem::take(&mut *LOG.lock().unwrap());
    (log, out.0, out.1, out.2)
}

/// Spawn and join one scribbler: the root waits, the dispatcher runs it.
unsafe fn scribble_turn(context: *mut Context) {
    // SAFETY: the root spawns into its own context and joins at once.
    unsafe {
        let s = hirsel_core_spawn(context, 0, 0, ptr::null_mut(), scribbler, layout(0), 0).task;
        assert!(!s.is_null());
        join(context, s, false);
    }
}

pub(super) fn body(_: u32) -> u8 {
    let stackful = run_form(false);
    let returnable = run_form(true);
    FLAG[0].store(true, Ordering::SeqCst);
    assert!(returnable.3 > 0, "the returnable client never returned its stack");
    assert_eq!(stackful.3, 0);
    assert_eq!(
        (&returnable.0, &returnable.1, &returnable.2),
        (&stackful.0, &stackful.1, &stackful.2),
        "the returnable form's provider sequence and carrier lengths differ from the stackful form's"
    );
    served(0, "the returnable client matched the stackful one");
    0
}

/// The provider's outside readiness: ready each pending token after a delay.
pub(super) fn producer() {
    loop {
        let token = PENDING.swap(0, Ordering::SeqCst);
        if token != 0 {
            sleep_producer(now() + super::draw(0, 2_000));
            hirsel_core_ready(token);
            continue;
        }
        if FLAG[0].load(Ordering::SeqCst) {
            return;
        }
        sleep_producer(now() + 300);
    }
}
