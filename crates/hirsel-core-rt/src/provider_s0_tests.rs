//! The four-state provider ABI (REPLAY-SPEC section 3) through the scheduler.
//! A recording provider notes, at every callback, whether a task is current
//! and how many scheduler events have been emitted. The oracle is section 3's
//! state table: wait, resume and abort run at the wake outside any task; issue
//! and deliver run in the task; deliver runs once per operation.
use crate::{
    Context, causes,
    provider::{
        ABORT_CANCEL, ABORT_DEADLINE, Buffer, Outcome, Provider, Request, STEP_DONE, STEP_PENDING,
        Step, WAIT_PARK, WAIT_UNTIL, Wait, hirsel_core_host,
    },
    tasks::*,
};
use std::{
    ptr,
    sync::atomic::{AtomicU64, Ordering},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cb {
    Wait,
    Issue,
    Resume,
    Abort(u8),
    Deliver,
}
#[derive(Clone, Copy, Debug)]
struct Call {
    cb: Cb,
    /// The request's site names the task's job in these tests.
    site: u64,
    token: u64,
    in_task: bool,
    events: usize,
}
/// What each callback answers, by job site and the call's ordinal for that
/// site and callback (0 is the first call).
type Answer<T> = fn(&mut Rec, u64, u64, usize) -> T;
struct Rec {
    calls: Vec<Call>,
    events: usize,
    wait: Answer<Wait>,
    issue: Answer<Step>,
    resume: Answer<Step>,
    /// The program-visible result of each job: (site, outcome, then
    /// check_cancel's answer right after the operation).
    results: Vec<(u64, Outcome, u8)>,
}
const ABORTED: u64 = 0xab07;
const TIMED_OUT: u64 = 0x7173;
fn ready(_: &mut Rec, _: u64, _: u64, _: usize) -> Wait {
    Wait::default()
}
fn done(value: u64) -> Step {
    Step {
        kind: STEP_DONE,
        outcome: Outcome {
            value,
            ..Outcome::default()
        },
        deadline_ns: 0,
    }
}
fn done0(_: &mut Rec, site: u64, _: u64, _: usize) -> Step {
    done(site)
}
fn pending(deadline_ns: u64) -> Step {
    Step {
        kind: STEP_PENDING,
        outcome: Outcome::default(),
        deadline_ns,
    }
}
fn never(_: &mut Rec, _: u64, _: u64, _: usize) -> Step {
    unreachable!("this provider never answers Pending")
}
impl Rec {
    fn new(wait: Answer<Wait>, issue: Answer<Step>, resume: Answer<Step>) -> Self {
        Self {
            calls: vec![],
            events: 0,
            wait,
            issue,
            resume,
            results: vec![],
        }
    }
    fn note(&mut self, cb: Cb, request: *mut Request, token: u64) -> usize {
        // SAFETY: the driver passes its pinned request.
        let site = unsafe { (*request).site };
        let nth = self
            .calls
            .iter()
            .filter(|c| c.site == site && c.cb == cb)
            .count();
        self.calls.push(Call {
            cb,
            site,
            token,
            in_task: crate::tasks::in_task(),
            events: self.events,
        });
        nth
    }
    fn of(&self, site: u64, cb: Cb) -> Vec<Call> {
        self.calls
            .iter()
            .filter(|c| c.site == site && c.cb == cb)
            .copied()
            .collect()
    }
    fn count(&self, site: u64, cb: Cb) -> usize {
        self.of(site, cb).len()
    }
    fn token(&self, site: u64) -> u64 {
        self.calls.iter().find(|c| c.site == site).unwrap().token
    }
    fn result(&self, site: u64) -> (Outcome, u8) {
        let (_, outcome, check) = *self.results.iter().find(|r| r.0 == site).unwrap();
        (outcome, check)
    }
    fn provider(&mut self) -> Provider {
        Provider {
            state: (self as *mut Self).cast(),
            wait: p_wait,
            issue: p_issue,
            resume: p_resume,
            abort: p_abort,
            deliver: p_deliver,
        }
    }
    /// Section 3: wait, resume and abort run outside any task; issue and
    /// deliver run in it.
    fn assert_contexts(&self) {
        for call in &self.calls {
            let in_task = matches!(call.cb, Cb::Issue | Cb::Deliver);
            assert_eq!(call.in_task, in_task, "{call:?}");
        }
    }
}
unsafe fn rec<'a>(state: *mut u8) -> &'a mut Rec {
    // SAFETY: every test pins its Rec for the whole run on one thread.
    unsafe { &mut *state.cast::<Rec>() }
}
unsafe extern "C" fn p_wait(state: *mut u8, request: *mut Request, token: u64) -> Wait {
    // SAFETY: see rec.
    unsafe {
        let r = rec(state);
        let nth = r.note(Cb::Wait, request, token);
        (r.wait)(r, (*request).site, token, nth)
    }
}
unsafe extern "C" fn p_issue(state: *mut u8, request: *mut Request, token: u64) -> Step {
    // SAFETY: see rec.
    unsafe {
        let r = rec(state);
        let nth = r.note(Cb::Issue, request, token);
        (r.issue)(r, (*request).site, token, nth)
    }
}
unsafe extern "C" fn p_resume(state: *mut u8, request: *mut Request, token: u64) -> Step {
    // SAFETY: see rec.
    unsafe {
        let r = rec(state);
        let nth = r.note(Cb::Resume, request, token);
        (r.resume)(r, (*request).site, token, nth)
    }
}
unsafe extern "C" fn p_abort(
    state: *mut u8,
    request: *mut Request,
    token: u64,
    why: u8,
) -> Outcome {
    // SAFETY: see rec.
    unsafe {
        rec(state).note(Cb::Abort(why), request, token);
    }
    Outcome {
        value: if why == ABORT_CANCEL {
            ABORTED
        } else {
            TIMED_OUT
        },
        ..Outcome::default()
    }
}
unsafe extern "C" fn p_deliver(state: *mut u8, request: *mut Request, outcome: Outcome) -> Outcome {
    // SAFETY: see rec.
    unsafe {
        rec(state).note(Cb::Deliver, request, 0);
    }
    outcome
}
unsafe extern "C" fn count_event(state: *mut u8, _: Event) {
    // SAFETY: see rec.
    unsafe { rec(state).events += 1 }
}
unsafe extern "C" fn lifo(_: *mut u8, count: usize) -> usize {
    count - 1
}

#[derive(Clone, Copy)]
struct Job {
    rec: *mut Rec,
    site: u64,
    operation: u8,
}
fn request(operation: u8, site: u64) -> Request {
    Request {
        operation,
        mode: 0,
        handle: 0,
        path: ptr::null(),
        path_len: 0,
        buffer: Buffer {
            data: ptr::null_mut(),
            len: 0,
            capacity: 0,
        },
        site,
    }
}
/// One provider operation, then a cancellation point. Fails when either
/// observed a cancellation request.
unsafe extern "C" fn job(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
    // SAFETY: the job is copied into the task; its Rec outlives the run.
    unsafe {
        let job = input.cast::<Job>().read();
        let provider = (*job.rec).provider();
        let mut request = request(job.operation, job.site);
        let outcome = hirsel_core_host(context, &provider, &mut request);
        let check = hirsel_core_check_cancel(context, job.site + 1000);
        (*job.rec).results.push((job.site, outcome, check));
        u8::from(outcome.status != 0 || check != 0)
    }
}
unsafe extern "C" fn inert(_: *mut Context, _: *mut u8) -> u8 {
    0
}
fn job_layout() -> TaskLayout {
    let value = |bytes| ValueLayout {
        bytes,
        align: 8,
        logical: 0,
        drop: inert,
    };
    TaskLayout {
        input: value(size_of::<Job>()),
        result: value(0),
    }
}
unsafe fn spawn(context: *mut Context, rec: *mut Rec, site: u64, operation: u8) -> *mut Task {
    let mut input = Job {
        rec,
        site,
        operation,
    };
    // SAFETY: the job is copied into the child.
    let task = unsafe {
        hirsel_core_spawn(
            context,
            64,
            (&mut input as *mut Job).cast(),
            job,
            job_layout(),
        )
        .task
    };
    assert!(!task.is_null());
    task
}
/// Join a job; true when it failed. A failed job's causes are released.
unsafe fn join(context: *mut Context, task: *mut Task) -> bool {
    // SAFETY: the parent owns the handle.
    unsafe {
        let mut unit = 0u8;
        let joined = hirsel_core_join(context, task, &mut unit, 1);
        assert_eq!(joined.status, 0);
        if joined.kind == 0 {
            return false;
        }
        drop(causes::Causes::from_raw(joined.causes));
        true
    }
}
/// Give every runnable task a turn: the root's poll allowance expires.
unsafe fn yield_turn(context: *mut Context) {
    for _ in 0..POLLS_PER_TURN {
        // SAFETY: the root polls its own context.
        assert_eq!(unsafe { hirsel_core_check_cancel(context, 0) }, 0);
    }
}
struct Root {
    rec: *mut Rec,
    body: unsafe fn(*mut Context, *mut Rec),
}
unsafe extern "C" fn root(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
    // SAFETY: run pins Root and its Rec.
    unsafe {
        let root = input.cast::<Root>().read();
        (root.body)(context, root.rec);
    }
    0
}
/// Run `body` as the root task with `rec` as provider and event sink.
fn run(rec: &mut Rec, lifo_choice: bool, body: unsafe fn(*mut Context, *mut Rec)) {
    let mut context = Context::new(1 << 20);
    causes::task_start(&mut context).unwrap();
    let state: *mut Rec = rec;
    let mut input = Root { rec: state, body };
    let mut result = 0u8;
    // SAFETY: Root, Rec and the destination outlive the run.
    let failed = unsafe {
        run_root(
            &mut context,
            root,
            (&mut input as *mut Root).cast(),
            &mut result,
            size_of::<Root>(),
            0,
            SchedulerProvider {
                state: state.cast(),
                event: Some(count_event),
                choose: if lifo_choice { Some(lifo) } else { None },
            },
        )
    };
    assert_eq!(failed, 0);
    rec.assert_contexts();
}
const READ: u8 = crate::provider::READ;

// ---- (b): W, cancelled ----

fn park(_: &mut Rec, _: u64, _: u64, _: usize) -> Wait {
    Wait {
        kind: WAIT_PARK,
        deadline_ns: 0,
    }
}
/// (b) A cancellation request in W fails the operation Cancelled; issue is
/// never called and nothing is delivered.
#[test]
fn s0_b_cancel_in_w_fails_cancelled_without_issue() {
    unsafe fn body(context: *mut Context, rec: *mut Rec) {
        unsafe {
            let task = spawn(context, rec, 1, READ);
            yield_turn(context);
            assert_eq!((*rec).count(1, Cb::Wait), 1, "the job parked in W");
            hirsel_core_cancel(context, task);
            assert!(join(context, task));
        }
    }
    let mut rec = Rec::new(park, never, never);
    run(&mut rec, false, body);
    let (outcome, _) = rec.result(1);
    assert_eq!(outcome.status, 1, "Cancelled");
    assert_eq!(rec.count(1, Cb::Issue), 0);
    assert_eq!(rec.count(1, Cb::Deliver), 0);
    // The request's wake re-ran wait, outside the task, then failed it.
    assert_eq!(rec.count(1, Cb::Wait), 2);
}

// ---- (c): I ----

fn pending_forever(_: &mut Rec, _: u64, _: u64, _: usize) -> Step {
    pending(0)
}
/// (c) Pending, then a cancellation request: abort(Cancel) gives Aborted, the
/// request stays pending for the next check_cancel, deliver runs once.
#[test]
fn s0_c_pending_then_cancel_aborts_and_the_request_stays() {
    unsafe fn body(context: *mut Context, rec: *mut Rec) {
        unsafe {
            let task = spawn(context, rec, 1, READ);
            yield_turn(context);
            assert_eq!((*rec).count(1, Cb::Issue), 1, "the job parked in I");
            hirsel_core_cancel(context, task);
            assert!(join(context, task));
        }
    }
    let mut rec = Rec::new(ready, pending_forever, never);
    run(&mut rec, false, body);
    let (outcome, check) = rec.result(1);
    assert_eq!((outcome.status, outcome.value), (0, ABORTED));
    assert_ne!(check, 0, "the next check_cancel observes the request");
    assert_eq!(rec.count(1, Cb::Abort(ABORT_CANCEL)), 1);
    assert_eq!(rec.count(1, Cb::Resume), 0);
    assert_eq!(rec.count(1, Cb::Deliver), 1);
}
fn pending_1ms(_: &mut Rec, _: u64, _: u64, _: usize) -> Step {
    pending(crate::os::monotonic_ns() + 1_000_000)
}
/// (c) Pending with a deadline: abort(Deadline) gives TimedOut, no request is
/// left, deliver runs once.
#[test]
fn s0_c_pending_deadline_times_out() {
    unsafe fn body(context: *mut Context, rec: *mut Rec) {
        unsafe {
            let task = spawn(context, rec, 1, READ);
            assert!(!join(context, task));
        }
    }
    let mut rec = Rec::new(ready, pending_1ms, never);
    run(&mut rec, false, body);
    let (outcome, check) = rec.result(1);
    assert_eq!((outcome.status, outcome.value, check), (0, TIMED_OUT, 0));
    assert_eq!(rec.count(1, Cb::Abort(ABORT_DEADLINE)), 1);
    assert_eq!(rec.count(1, Cb::Deliver), 1);
}
fn pending_readied_elsewhere(_: &mut Rec, _: u64, token: u64, _: usize) -> Step {
    // Another thread readies the operation after it has parked.
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(2));
        hirsel_core_ready(token);
    });
    pending(0)
}
fn resume_done(_: &mut Rec, _: u64, _: u64, _: usize) -> Step {
    done(42)
}
/// (c) Pending, then hirsel_core_ready from another thread: resume gives
/// Done, deliver runs once.
#[test]
fn s0_c_ready_then_resume_delivers_done() {
    unsafe fn body(context: *mut Context, rec: *mut Rec) {
        unsafe {
            let task = spawn(context, rec, 1, READ);
            assert!(!join(context, task));
        }
    }
    let mut rec = Rec::new(ready, pending_readied_elsewhere, resume_done);
    run(&mut rec, false, body);
    let (outcome, check) = rec.result(1);
    assert_eq!((outcome.status, outcome.value, check), (0, 42, 0));
    assert_eq!(rec.count(1, Cb::Resume), 1);
    assert_eq!(rec.count(1, Cb::Deliver), 1);
}
fn pending_then_ready_twice(_: &mut Rec, _: u64, token: u64, _: usize) -> Step {
    hirsel_core_ready(token);
    pending(0)
}
fn resume_pending_once(_: &mut Rec, _: u64, token: u64, nth: usize) -> Step {
    if nth == 0 {
        hirsel_core_ready(token);
        pending(0)
    } else {
        done(7)
    }
}
/// Resume answering Pending again stays in I; a ready given from inside issue
/// or resume, before the operation parks, is taken after it parks.
#[test]
fn s0_resume_pending_again_stays_in_i() {
    unsafe fn body(context: *mut Context, rec: *mut Rec) {
        unsafe {
            let task = spawn(context, rec, 1, READ);
            assert!(!join(context, task));
        }
    }
    let mut rec = Rec::new(ready, pending_then_ready_twice, resume_pending_once);
    run(&mut rec, false, body);
    assert_eq!(rec.result(1).0.value, 7);
    assert_eq!(rec.count(1, Cb::Resume), 2);
    assert_eq!(rec.count(1, Cb::Deliver), 1);
}

// ---- F1: callbacks at the wake, outside any task ----

fn ready_both_on_second_issue(rec: &mut Rec, _: u64, _: u64, _: usize) -> Step {
    let issued: Vec<u64> = rec
        .calls
        .iter()
        .filter(|c| c.cb == Cb::Issue)
        .map(|c| c.token)
        .collect();
    if issued.len() == 2 {
        // Woken in issue order: first, then second.
        hirsel_core_ready(issued[0]);
        hirsel_core_ready(issued[1]);
    }
    pending(0)
}
/// F1: resume runs at the wake, before any pop, outside any task. Two jobs are
/// woken in one order and, with a last-first choice, popped in the other.
#[test]
fn s0_f1_resume_runs_at_the_wake_not_at_the_pop() {
    unsafe fn body(context: *mut Context, rec: *mut Rec) {
        unsafe {
            let a = spawn(context, rec, 1, READ);
            let b = spawn(context, rec, 2, READ);
            assert!(!join(context, a));
            assert!(!join(context, b));
        }
    }
    let mut rec = Rec::new(ready, ready_both_on_second_issue, done0);
    run(&mut rec, true, body);
    let issue_order: Vec<u64> = rec
        .calls
        .iter()
        .filter(|c| c.cb == Cb::Issue)
        .map(|c| c.site)
        .collect();
    let resumes: Vec<Call> = rec
        .calls
        .iter()
        .filter(|c| c.cb == Cb::Resume)
        .copied()
        .collect();
    let delivers: Vec<u64> = rec
        .calls
        .iter()
        .filter(|c| c.cb == Cb::Deliver)
        .map(|c| c.site)
        .collect();
    let resume_order: Vec<u64> = resumes.iter().map(|c| c.site).collect();
    assert_eq!(resume_order, issue_order, "resumed in wake order");
    let mut popped = issue_order.clone();
    popped.reverse();
    assert_eq!(delivers, popped, "delivered in pop order");
    assert_eq!(
        resumes[0].events, resumes[1].events,
        "no task ran between the two wakes"
    );
    assert!(resumes.iter().all(|c| !c.in_task));
}

// ---- F2: a token names one operation ----

/// Jobs 1 and 2 park at their first wait; every other wait is Ready.
fn park_first(_: &mut Rec, site: u64, _: u64, nth: usize) -> Wait {
    if nth == 0 && site != 9 {
        Wait {
            kind: WAIT_PARK,
            deadline_ns: 0,
        }
    } else {
        Wait::default()
    }
}
/// F2: a ready wakes only its own operation. Unknown and finished tokens are
/// ignored, and two readies of one token in one turn are one wake.
#[test]
fn s0_f2_ready_wakes_only_its_token() {
    unsafe fn body(context: *mut Context, rec: *mut Rec) {
        unsafe {
            let done_task = spawn(context, rec, 9, crate::provider::MONO_NS);
            yield_turn(context);
            let a = spawn(context, rec, 1, READ);
            let b = spawn(context, rec, 2, READ);
            yield_turn(context);
            assert!(!join(context, done_task));
            let r = &*rec;
            assert_eq!((r.count(1, Cb::Wait), r.count(2, Cb::Wait)), (1, 1));
            // Unknown and finished tokens wake nothing.
            hirsel_core_ready(u64::MAX - 1);
            hirsel_core_ready((*rec).token(9));
            yield_turn(context);
            assert_eq!(
                ((*rec).count(1, Cb::Wait), (*rec).count(2, Cb::Wait)),
                (1, 1)
            );
            // Two readies of a's token in one turn: one wake, and b sleeps on.
            hirsel_core_ready((*rec).token(1));
            hirsel_core_ready((*rec).token(1));
            assert!(!join(context, a));
            assert_eq!(
                ((*rec).count(1, Cb::Wait), (*rec).count(2, Cb::Wait)),
                (2, 1)
            );
            hirsel_core_ready((*rec).token(2));
            assert!(!join(context, b));
        }
    }
    let mut rec = Rec::new(park_first, done0, never);
    run(&mut rec, false, body);
    assert_eq!(rec.count(2, Cb::Wait), 2);
    for site in [1, 2, 9] {
        assert_eq!(rec.count(site, Cb::Deliver), 1);
    }
}

// ---- C at entry, for every kind ----

/// C: a pending request fails every cancellable operation Cancelled at entry
/// with no provider call; wall_ms and mono_ns are not cancellation points
/// (CONC-7): they run through wait, issue and deliver, and the request stays
/// for the next check_cancel. One task per kind, each cancelled before it runs.
#[test]
fn s0_entry_cancel_checks_every_cancellable_kind() {
    unsafe fn body(context: *mut Context, rec: *mut Rec) {
        unsafe {
            let tasks: Vec<_> = (0..=crate::provider::RANDOM)
                .map(|operation| spawn(context, rec, 100 + u64::from(operation), operation))
                .collect();
            for &task in &tasks {
                hirsel_core_cancel(context, task);
            }
            for task in tasks {
                assert!(join(context, task));
            }
        }
    }
    let mut rec = Rec::new(ready, done0, never);
    run(&mut rec, false, body);
    for operation in 0..=crate::provider::RANDOM {
        let site = 100 + u64::from(operation);
        let clock = operation == crate::provider::WALL_MS || operation == crate::provider::MONO_NS;
        let (outcome, check) = rec.result(site);
        assert_eq!(outcome.status, u8::from(!clock), "kind {operation}");
        assert_eq!(check != 0, clock, "kind {operation}");
        let calls = rec.calls.iter().filter(|c| c.site == site).count();
        assert_eq!(calls, if clock { 3 } else { 0 }, "kind {operation}");
    }
}

// ---- W timers ----

static UNTIL_AT: AtomicU64 = AtomicU64::new(0);
fn until_once(_: &mut Rec, _: u64, _: u64, nth: usize) -> Wait {
    if nth == 0 {
        Wait {
            kind: WAIT_UNTIL,
            deadline_ns: UNTIL_AT.load(Ordering::Relaxed),
        }
    } else {
        Wait::default()
    }
}
fn until_case(at: u64) {
    unsafe fn body(context: *mut Context, rec: *mut Rec) {
        unsafe {
            let task = spawn(context, rec, 1, READ);
            assert!(!join(context, task));
        }
    }
    UNTIL_AT.store(at, Ordering::Relaxed);
    let mut rec = Rec::new(until_once, done0, never);
    run(&mut rec, false, body);
    // The timer's wake re-ran wait, which answered Ready.
    assert_eq!(rec.count(1, Cb::Wait), 2);
    assert_eq!(rec.count(1, Cb::Deliver), 1);
}
/// Until a deadline in the past, or equal to now, wakes at the next turn and
/// re-runs wait; it never issues without that second wait.
#[test]
fn s0_until_past_and_now_rerun_wait() {
    until_case(1);
    until_case(crate::os::monotonic_ns());
    until_case(crate::os::monotonic_ns() + 1_000_000);
}
/// Until u64::MAX never fires, a Park never readied and a Pending never
/// resumed stay parked, and the other tasks run; cancellation then ends them.
#[test]
fn s0_never_woken_operations_leave_other_tasks_running() {
    fn forever_wait(_: &mut Rec, site: u64, _: u64, _: usize) -> Wait {
        match site {
            1 => Wait {
                kind: WAIT_UNTIL,
                deadline_ns: u64::MAX,
            },
            2 => Wait {
                kind: WAIT_PARK,
                deadline_ns: 0,
            },
            _ => Wait::default(),
        }
    }
    fn forever_issue(_: &mut Rec, site: u64, _: u64, _: usize) -> Step {
        if site == 3 { pending(0) } else { done(site) }
    }
    unsafe fn body(context: *mut Context, rec: *mut Rec) {
        unsafe {
            let parked: Vec<_> = (1..=3)
                .map(|site| spawn(context, rec, site, READ))
                .collect();
            yield_turn(context);
            for site in 4..=6 {
                let peer = spawn(context, rec, site, READ);
                assert!(!join(context, peer));
            }
            for &task in &parked {
                hirsel_core_cancel(context, task);
            }
            for task in parked {
                assert!(join(context, task));
            }
        }
    }
    let mut rec = Rec::new(forever_wait, forever_issue, never);
    run(&mut rec, false, body);
    assert_eq!(rec.result(1).0.status, 1);
    assert_eq!(rec.result(2).0.status, 1);
    assert_eq!(rec.result(3).0.value, ABORTED);
    assert_eq!(rec.count(1, Cb::Issue) + rec.count(2, Cb::Issue), 0);
}
static EQUAL_AT: AtomicU64 = AtomicU64::new(0);
/// Many equal deadlines all fire in one turn: every second wait runs before
/// any of those tasks runs again.
#[test]
fn s0_equal_deadlines_wake_together() {
    fn equal_wait(_: &mut Rec, _: u64, _: u64, nth: usize) -> Wait {
        if nth == 0 {
            Wait {
                kind: WAIT_UNTIL,
                deadline_ns: EQUAL_AT.load(Ordering::Relaxed),
            }
        } else {
            Wait::default()
        }
    }
    unsafe fn body(context: *mut Context, rec: *mut Rec) {
        unsafe {
            let tasks: Vec<_> = (1..=8)
                .map(|site| spawn(context, rec, site, READ))
                .collect();
            for task in tasks {
                assert!(!join(context, task));
            }
        }
    }
    EQUAL_AT.store(crate::os::monotonic_ns() + 2_000_000, Ordering::Relaxed);
    let mut rec = Rec::new(equal_wait, done0, never);
    run(&mut rec, false, body);
    let second: Vec<Call> = rec
        .calls
        .iter()
        .filter(|c| c.cb == Cb::Wait)
        .skip(8)
        .copied()
        .collect();
    assert_eq!(second.len(), 8);
    let first_issue = rec.calls.iter().position(|c| c.cb == Cb::Issue).unwrap();
    let last_wait = rec.calls.iter().rposition(|c| c.cb == Cb::Wait).unwrap();
    assert!(last_wait < first_issue, "all eight woke in one turn");
    for site in 1..=8 {
        assert_eq!(rec.count(site, Cb::Deliver), 1);
    }
}
/// Readiness and a deadline due in the same turn: readiness wins (the turn
/// takes readies, then requests, then timers), so resume runs and abort never.
#[test]
fn s0_readiness_beats_a_deadline_due_at_the_same_turn() {
    fn due_and_ready(_: &mut Rec, _: u64, token: u64, _: usize) -> Step {
        hirsel_core_ready(token);
        pending(crate::os::monotonic_ns())
    }
    unsafe fn body(context: *mut Context, rec: *mut Rec) {
        unsafe {
            let task = spawn(context, rec, 1, READ);
            assert!(!join(context, task));
        }
    }
    let mut rec = Rec::new(ready, due_and_ready, done0);
    run(&mut rec, false, body);
    assert_eq!(rec.count(1, Cb::Resume), 1);
    assert_eq!(rec.count(1, Cb::Abort(ABORT_DEADLINE)), 0);
    assert_eq!(rec.result(1).0.value, 1);
}
/// Pending with a deadline, then two cancellation requests before it: one
/// abort decides the outcome, and deliver runs once.
#[test]
fn s0_pending_cancel_then_deadline_is_one_abort() {
    unsafe fn body(context: *mut Context, rec: *mut Rec) {
        unsafe {
            let task = spawn(context, rec, 1, READ);
            yield_turn(context);
            hirsel_core_cancel(context, task);
            hirsel_core_cancel(context, task);
            assert!(join(context, task));
        }
    }
    let mut rec = Rec::new(ready, pending_1ms, never);
    run(&mut rec, false, body);
    let aborts = rec.count(1, Cb::Abort(ABORT_CANCEL)) + rec.count(1, Cb::Abort(ABORT_DEADLINE));
    assert_eq!(aborts, 1);
    assert_eq!(rec.count(1, Cb::Deliver), 1);
}
