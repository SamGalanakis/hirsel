//! The four-state provider ABI (REPLAY-SPEC section 3) through the scheduler.
//! A recording provider notes, at every callback, whether a task is current
//! and how many scheduler events have been emitted. The oracle is section 3's
//! state table: wait, resume and abort run at the wake outside any task; issue
//! and deliver run in the task; deliver runs once per operation.
use crate::schedule_script::ScheduleScript;
use crate::{
    Context, causes,
    provider::{
        ABORT_CANCEL, ABORT_DEADLINE, Buffer, Outcome, Provider, Request, STEP_DONE, STEP_PENDING,
        Step, WAIT_PARK, WAIT_UNTIL, Wait, hirsel_core_host,
    },
    tasks::*,
};
use std::sync::Arc;
use std::{
    ptr,
    sync::atomic::{AtomicU64, Ordering},
};

#[test]
fn flow_step_bounds_a_private_cancel_loop_and_preserves_the_owner_request() {
    use crate::flow::{Failure, Halt, Pull, Reference, StageCharge};

    for finish in [false, true] {
        let mut owner = Context::new(4096);
        causes::task_start(&mut owner).unwrap();
        let start = owner.available();
        let counted = Arc::new(AtomicU64::new(0));
        let calls = Arc::clone(&counted);
        let mut flow = Reference::<u64>::default();
        let sink = flow.sink().unwrap();
        let stage = flow
            .stage(
                &owner,
                sink,
                1,
                StageCharge {
                    output: 8,
                    working: 0,
                    state: 0,
                    item_steps: 1000,
                },
                move |stage, _, _| {
                    loop {
                        stage.steps -= 1;
                        // SAFETY: the stage context is private to this item invocation.
                        if stage.steps <= 0
                            && unsafe { crate::steps::hirsel_core_step_out(stage, 0) } != 0
                        {
                            return Err(Halt::OutOfTime);
                        }
                        assert_eq!(unsafe { hirsel_core_check_cancel(stage, 0) }, 0);
                        calls.fetch_add(1, Ordering::Relaxed);
                    }
                },
            )
            .unwrap();
        let port = flow.port(&owner, stage, 1, 8).unwrap();
        flow.push(port, 1, 1).unwrap();
        owner.cancel_requested.store(true, Ordering::Release);
        assert_eq!(flow.next(sink), Ok(Pull::Dropped(0, 1)));
        assert_eq!(counted.load(Ordering::Relaxed), 1000);
        if finish {
            assert_eq!(
                flow.finish(),
                Err(Failure {
                    stage: 0,
                    key: 1,
                    halt: Halt::OutOfTime
                })
            );
        } else {
            drop(flow);
        }
        assert_eq!(owner.available(), start);
        // SAFETY: the fixture owns the owner's context and its prepaid primary.
        assert_eq!(unsafe { hirsel_core_check_cancel(&mut owner, 1) }, 1);
        assert_eq!(
            owner.causes.iter().next().unwrap().kind(),
            causes::CANCELLED
        );
    }
}

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
    /// Every scheduler event, in order.
    log: Vec<Event>,
    wait: Answer<Wait>,
    issue: Answer<Step>,
    resume: Answer<Step>,
    /// The program-visible result of each job: (site, outcome, then
    /// check_cancel's answer right after the operation).
    results: Vec<(u64, Outcome, u8)>,
    stacks: Vec<Option<usize>>,
    timer_scenario: Option<TimerScenario>,
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
            log: vec![],
            wait,
            issue,
            resume,
            results: vec![],
            stacks: vec![],
            timer_scenario: None,
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
unsafe extern "C" fn count_event(state: *mut u8, event: Event) {
    // SAFETY: see rec.
    unsafe {
        let r = rec(state);
        r.events += 1;
        r.log.push(event);
    }
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
fn job_layout() -> TaskLayout {
    let value = |bytes| ValueLayout {
        bytes,
        align: 8,
        logical: 0,
        clean: None,
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
            0,
            64,
            (&mut input as *mut Job).cast(),
            job,
            job_layout(),
            0,
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
    let mut files = crate::host_file::Files::default();
    context.host_files = &mut files;
    causes::task_start(&mut context).unwrap();
    let state: *mut Rec = rec;
    let mut input = Root { rec: state, body };
    let mut result = 0u8;
    // SAFETY: Root, Rec and the destination outlive the run.
    let mut execute = || unsafe {
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
                ..SchedulerProvider::default()
            },
        )
    };
    let failed = if lifo_choice {
        Arc::new(ScheduleScript::last()).install(execute)
    } else {
        execute()
    };
    assert_eq!(failed, 0);
    rec.assert_contexts();
    // Run teardown closes every file still live, outside any task, as
    // hirsel_core_run does after main.
    // SAFETY: the run has ended; the registry's files are sole-owned.
    unsafe { files.close_all(&mut context) };
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
/// (c) for a sync: an issued SYNC is not cancellable. The request waits for
/// the sync's own result (never abort, never Aborted), and stays pending for
/// the next check_cancel.
#[test]
fn s0_c_an_issued_sync_delivers_its_result_through_a_cancel() {
    unsafe fn body(context: *mut Context, rec: *mut Rec) {
        unsafe {
            let task = spawn(context, rec, 1, crate::provider::SYNC);
            yield_turn(context);
            let issued = (*rec)
                .calls
                .iter()
                .find(|c| c.site == 1 && c.cb == Cb::Issue);
            let token = issued.expect("the job parked in I").token;
            // The cancel is taken before the ready, at the same turn.
            hirsel_core_cancel(context, task);
            hirsel_core_ready(token);
            assert!(join(context, task));
        }
    }
    let mut rec = Rec::new(ready, pending_forever, done0);
    run(&mut rec, false, body);
    let (outcome, check) = rec.result(1);
    assert_eq!(
        (outcome.status, outcome.value),
        (0, 1),
        "the sync's own result"
    );
    assert_ne!(check, 0, "the next check_cancel observes the request");
    assert_eq!(rec.count(1, Cb::Abort(ABORT_CANCEL)), 0);
    assert_eq!(rec.count(1, Cb::Resume), 1);
    assert_eq!(rec.count(1, Cb::Deliver), 1);
}
/// End a sibling while the issued operation is parked, then request the
/// provider task's end. Only its abort followed by END can release its stack.
#[test]
fn s0_pending_frame_keeps_its_stack_until_abort() {
    fn issue(_: &mut Rec, site: u64, _: u64, _: usize) -> Step {
        if site == 1 { pending(0) } else { done(site) }
    }
    unsafe fn body(context: *mut Context, rec: *mut Rec) {
        // SAFETY: the root owns each child handle and pins the recording provider.
        unsafe {
            let waiting = spawn(context, rec, 1, READ);
            let first = task_stack(waiting);
            yield_turn(context);
            let parked = task_stack(waiting);
            (*rec).stacks.extend([first, parked]);
            if parked.is_none() {
                // Stop the control before it could resume on the released stack.
                return;
            }
            let sibling = spawn(context, rec, 2, READ);
            (*rec).stacks.push(task_stack(sibling));
            assert!(!join(context, sibling));
            hirsel_core_cancel(context, waiting);
            assert!(join(context, waiting));
            let after_abort = spawn(context, rec, 3, READ);
            (*rec).stacks.push(task_stack(after_abort));
            assert!(!join(context, after_abort));
        }
    }
    let mut rec = Rec::new(ready, issue, never);
    run(&mut rec, false, body);
    let first = rec.stacks[0].unwrap();
    assert_eq!(
        rec.stacks[1],
        Some(first),
        "the pending frame owns its stack"
    );
    assert_ne!(
        rec.stacks[2],
        Some(first),
        "the sibling cannot take that stack"
    );
    assert_eq!(rec.stacks[3], Some(first), "END makes the stack reusable");
    assert_eq!(rec.result(1).0.value, ABORTED);
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
    hirsel_core_ready(token);
    pending(0)
}
fn resume_pending_once(_: &mut Rec, _: u64, _: u64, nth: usize) -> Step {
    if nth == 0 { pending(0) } else { done(7) }
}
/// A duplicate in this turn cannot resume the re-armed operation. The same
/// token readied in a later turn is a new probe and can deliver its result.
#[test]
fn s0_resume_pending_again_stays_in_i() {
    unsafe fn body(context: *mut Context, rec: *mut Rec) {
        unsafe {
            let task = spawn(context, rec, 1, READ);
            yield_turn(context);
            assert_eq!((*rec).count(1, Cb::Resume), 1);
            assert_eq!((*rec).count(1, Cb::Deliver), 0);
            hirsel_core_ready((*rec).token(1));
            assert!(!join(context, task));
        }
    }
    let mut rec = Rec::new(ready, pending_then_ready_twice, resume_pending_once);
    run(&mut rec, false, body);
    assert_eq!(rec.result(1).0.value, 7);
    assert_eq!(rec.count(1, Cb::Resume), 2);
    assert_eq!(rec.count(1, Cb::Deliver), 1);
}

/// Two driver turns capture the same arm. The second wins and rearms on
/// Pending while the first is paused. Only the new arm can produce DONE.
#[test]
fn an_old_readiness_cannot_resume_a_rearmed_operation() {
    use crate::schedule_script::PausePoint;
    use std::sync::{Barrier, Mutex};

    const TOKEN: u64 = 47;
    let arms = Mutex::new(WaitArms::default());
    arms.lock().unwrap().insert(TOKEN, ());
    let taken = Arc::new(Barrier::new(2));
    let rearmed = Arc::new(Barrier::new(2));
    let script = ScheduleScript::new(0).with_pause_hook({
        let taken = taken.clone();
        let rearmed = rearmed.clone();
        move |point| {
            assert_eq!(point, PausePoint::ReadinessTaken { token: TOKEN });
            taken.wait();
            rearmed.wait();
        }
    });
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let mut ready = Vec::new();
            arms.lock().unwrap().capture([TOKEN, TOKEN], &mut ready);
            script.pause(PausePoint::ReadinessTaken { token: TOKEN });
            assert_eq!(ready.len(), 1);
            assert!(arms.lock().unwrap().take_captured(ready[0]).is_none());
        });
        taken.wait();
        let resumed = {
            let mut arms = arms.lock().unwrap();
            let mut ready = Vec::new();
            arms.capture([TOKEN], &mut ready);
            let resumed = ready.first().and_then(|&arm| arms.take_captured(arm));
            // The winning resume answered Pending.
            arms.insert(TOKEN, ());
            resumed
        };
        rearmed.wait();
        assert_eq!(resumed, Some(()));
    });
    let mut arms = arms.lock().unwrap();
    let mut ready = Vec::new();
    arms.capture([TOKEN], &mut ready);
    let produced = arms
        .take_captured(ready.pop().unwrap())
        .map(|()| done(7).outcome.value);
    assert_eq!(produced, Some(7));
    arms.capture([TOKEN], &mut ready);
    assert!(ready.is_empty());
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
struct TimerScenario {
    seed: u64,
    deadline: u64,
    expected_expiry: Vec<u64>,
}

#[test]
fn s0_expiry_follows_deadlines_and_fifo_arm_order_after_rearms_and_cancels() {
    fn arm(rec: &mut Rec, _: u64, _: u64, _: usize) -> Step {
        pending(rec.timer_scenario.as_ref().unwrap().deadline)
    }

    unsafe fn body(context: *mut Context, rec: *mut Rec) {
        // SAFETY: the root owns every handle, and Rec is pinned for callbacks.
        unsafe {
            const SLOTS: usize = 256;
            const STEPS: usize = 2048;
            // Host time stays below this range. The test supplies each expiry
            // instant directly to the same wake pass the scheduler runs.
            let mut now = u64::MAX - 8192;
            let mut slots = [None; SLOTS];
            let mut armed = Vec::<(usize, u64)>::new();
            let mut next_site = 1;
            let mut random = (*rec).timer_scenario.as_ref().unwrap().seed;

            for step in 0..=STEPS {
                random = random
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let slot = (random >> 32) as usize % SLOTS;
                let action = (random >> 40) % 32;
                if step == STEPS || (step >= SLOTS && action == 31) {
                    now = if step == STEPS { u64::MAX } else { now + 2 };
                    let mut due: Vec<_> = armed
                        .iter()
                        .copied()
                        .filter(|&(_, deadline)| deadline <= now)
                        .collect();
                    armed.retain(|&(_, deadline)| deadline > now);
                    // Iteration starts in arm order. Stable sort preserves
                    // that order for ties without sharing the runtime key.
                    due.sort_by_key(|&(_, deadline)| deadline);
                    checkpoint_at(now);
                    for (slot, _) in due {
                        let (task, site) = slots[slot].take().unwrap();
                        (*rec)
                            .timer_scenario
                            .as_mut()
                            .unwrap()
                            .expected_expiry
                            .push(site);
                        assert!(!join(context, task));
                    }
                } else if step >= SLOTS && action >= 28 {
                    if let Some((task, _)) = slots[slot].take() {
                        armed.retain(|&(other, _)| other != slot);
                        hirsel_core_cancel(context, task);
                        assert!(join(context, task));
                    }
                } else {
                    let slot = if step < SLOTS { step } else { slot };
                    let deadline = now + 1 + (random >> 45) % 8;
                    (*rec).timer_scenario.as_mut().unwrap().deadline = deadline;
                    if let Some((_, site)) = slots[slot] {
                        armed.retain(|&(other, _)| other != slot);
                        hirsel_core_ready((*rec).token(site));
                    } else {
                        let task = spawn(context, rec, next_site, READ);
                        slots[slot] = Some((task, next_site));
                        next_site += 1;
                    }
                    armed.push((slot, deadline));
                    checkpoint();
                }
            }
        }
    }

    for seed in [1, 23, 4567] {
        let mut rec = Rec::new(ready, arm, arm);
        rec.timer_scenario = Some(TimerScenario {
            seed,
            deadline: 0,
            expected_expiry: Vec::new(),
        });
        run(&mut rec, false, body);
        let expected = &rec.timer_scenario.as_ref().unwrap().expected_expiry;
        let actual: Vec<_> = rec
            .calls
            .iter()
            .filter(|call| call.cb == Cb::Abort(ABORT_DEADLINE))
            .map(|call| call.site)
            .collect();
        assert_eq!(actual.len(), expected.len(), "seed {seed}");
        for (index, (actual, expected)) in actual.iter().zip(expected).enumerate() {
            assert_eq!(actual, expected, "seed {seed}, expiry {index}");
        }
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

// ---- cancellation against a later ready ----

/// A ready takes effect at the scheduler turn that takes it (section 3 step
/// 3: the wake's moment); a cancellation request at its CANCEL event, which a
/// task emits before that turn. So a request made before the turn precedes
/// every ready taken at it, wherever the ready came from, and the order is a
/// function of the recorded events. A cancel then a ready of a job parked in
/// W: Cancelled, never issued.
#[test]
fn s0_cancel_precedes_a_later_ready_in_w() {
    unsafe fn body<const READY_FIRST: bool>(context: *mut Context, rec: *mut Rec) {
        unsafe {
            let task = spawn(context, rec, 1, READ);
            yield_turn(context);
            assert_eq!((*rec).count(1, Cb::Wait), 1);
            let token = (*rec).token(1);
            if READY_FIRST {
                hirsel_core_ready(token);
            }
            hirsel_core_cancel(context, task);
            if !READY_FIRST {
                hirsel_core_ready(token);
            }
            assert!(join(context, task));
        }
    }
    for body in [
        body::<false> as unsafe fn(*mut Context, *mut Rec),
        body::<true>,
    ] {
        let mut rec = Rec::new(park_first, done0, never);
        run(&mut rec, false, body);
        assert_eq!(rec.count(1, Cb::Issue), 0, "a cancelled W never issues");
        assert_eq!(rec.count(1, Cb::Deliver), 0);
        assert_eq!(rec.result(1).0.status, 1, "Cancelled");
    }
}
/// The same in I: abort(Cancel) decides the outcome, resume never runs.
#[test]
fn s0_cancel_precedes_a_later_ready_in_i() {
    unsafe fn body<const READY_FIRST: bool>(context: *mut Context, rec: *mut Rec) {
        unsafe {
            let task = spawn(context, rec, 1, READ);
            yield_turn(context);
            assert_eq!((*rec).count(1, Cb::Issue), 1);
            let token = (*rec).token(1);
            if READY_FIRST {
                hirsel_core_ready(token);
            }
            hirsel_core_cancel(context, task);
            if !READY_FIRST {
                hirsel_core_ready(token);
            }
            assert!(join(context, task));
        }
    }
    for body in [
        body::<false> as unsafe fn(*mut Context, *mut Rec),
        body::<true>,
    ] {
        let mut rec = Rec::new(ready, pending_forever, done0);
        run(&mut rec, false, body);
        assert_eq!(rec.count(1, Cb::Abort(ABORT_CANCEL)), 1);
        assert_eq!(rec.count(1, Cb::Resume), 0);
        assert_eq!(rec.result(1).0.value, ABORTED);
    }
}

/// Once a WAKE has moved W to ready-to-issue, cancellation can first take
/// effect at the next cancellation point, after the operation's delivery.
#[test]
fn s0_cancel_after_taken_wake_allows_issue() {
    unsafe fn body(context: *mut Context, rec: *mut Rec) {
        unsafe {
            let task = spawn(context, rec, 1, READ);
            yield_turn(context);
            hirsel_core_ready((*rec).token(1));
            checkpoint();
            assert_eq!((*rec).count(1, Cb::Wait), 2);
            assert_eq!((*rec).count(1, Cb::Issue), 0);
            hirsel_core_cancel(context, task);
            assert!(join(context, task));
        }
    }
    let mut rec = Rec::new(park_first, done0, never);
    run(&mut rec, false, body);
    assert_eq!(rec.count(1, Cb::Issue), 1);
    assert_eq!(rec.count(1, Cb::Deliver), 1);
    let (outcome, check) = rec.result(1);
    assert_eq!((outcome.status, outcome.value, check), (0, 1, 1));
}

// ---- what a cancelled operation consumed ----

const FILE_SITE: u64 = 77;
fn park_file_op(_: &mut Rec, site: u64, _: u64, nth: usize) -> Wait {
    if site == FILE_SITE && nth == 0 {
        Wait {
            kind: WAIT_PARK,
            deadline_ns: 0,
        }
    } else {
        Wait::default()
    }
}
fn pend_file_op(_: &mut Rec, site: u64, _: u64, _: usize) -> Step {
    if site == FILE_SITE {
        pending(0)
    } else {
        done(42)
    }
}
#[derive(Clone, Copy)]
struct FileJob {
    rec: *mut Rec,
    operation: u8,
}
/// Open a file through the recording provider, then run one file operation on
/// it (site 77). Returns the operation's status.
unsafe extern "C" fn file_job(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
    // SAFETY: as job.
    unsafe {
        let job = input.cast::<FileJob>().read();
        let provider = (*job.rec).provider();
        let mut open = request(crate::provider::OPEN, 1);
        let opened = crate::host_file::hirsel_core_host_file_open(context, &provider, &mut open);
        assert_eq!(opened.status, 0);
        let mut op = request(job.operation, FILE_SITE);
        let outcome = crate::host_file::hirsel_core_host_file(context, opened.value, &mut op);
        (*job.rec).results.push((FILE_SITE, outcome, 0));
        1
    }
}
/// Two children use the same inherited table while both files are live.
#[test]
fn s0_children_keep_distinct_host_file_slots() {
    unsafe extern "C" fn open_child(context: *mut Context, result: *mut u8, input: *mut u8) -> u8 {
        // The child owns its input and result, and the parent pins the provider.
        unsafe {
            let job = input.cast::<Job>().read();
            let provider = (*job.rec).provider();
            let mut open = request(job.operation, job.site);
            let outcome =
                crate::host_file::hirsel_core_host_file_open(context, &provider, &mut open);
            result.cast::<u64>().write(outcome.value);
            outcome.status
        }
    }
    unsafe fn body(context: *mut Context, rec: *mut Rec) {
        unsafe {
            let mut layout = job_layout();
            layout.result.bytes = size_of::<u64>();
            let children = [41, 42].map(|site| {
                let mut job = Job {
                    rec,
                    site,
                    operation: crate::provider::OPEN,
                };
                hirsel_core_spawn(
                    context,
                    0,
                    64,
                    (&mut job as *mut Job).cast(),
                    open_child,
                    layout,
                    0,
                )
                .task
            });
            for (site, child) in [41, 42].into_iter().zip(children) {
                let mut value = 0u64;
                let joined = hirsel_core_join(context, child, (&mut value as *mut u64).cast(), 1);
                assert_eq!(joined.status, 0);
                assert_eq!(joined.kind, 0);
                (*rec).results.push((
                    site,
                    Outcome {
                        value,
                        ..Outcome::default()
                    },
                    0,
                ));
            }
        }
    }
    let mut rec = Rec::new(ready, done0, never);
    run(&mut rec, false, body);
    assert_ne!(rec.result(41).0.value, rec.result(42).0.value);
}

/// An edge wakes only its direction. A batch collected before closing a
/// socket cannot wake a waiter on the replacement in the same slot.
#[test]
fn s0_net_rearm_ignores_stale_directional_readiness() {
    use std::os::unix::net::UnixStream;

    let (socket, _peer) = UnixStream::pair().unwrap();
    let mut sockets = hirsel_core_net::Sockets::default();
    let key = sockets.insert(socket.into()).unwrap();
    sockets.arm(key, false, 101).unwrap();
    let first = sockets.arm(key, true, 201).unwrap();
    let mut ready = Vec::new();
    sockets.dispatch(first, hirsel_core_net::sys::EPOLLIN as u32, |t| {
        ready.push(t)
    });
    assert_eq!(ready, [101]);
    sockets.dispatch(first, hirsel_core_net::sys::EPOLLOUT as u32, |t| {
        ready.push(t)
    });
    assert_eq!(ready, [101, 201]);

    sockets.release(key);
    let (socket, _peer) = UnixStream::pair().unwrap();
    let replacement = sockets.insert(socket.into()).unwrap();
    let second = sockets.arm(replacement, false, 102).unwrap();
    assert_eq!(first as u32, second as u32, "the registry reuses the slot");
    sockets.dispatch(first, hirsel_core_net::sys::EPOLLIN as u32, |t| {
        ready.push(t)
    });
    assert_eq!(ready, [101, 201], "the old edge must not wake a new reader");
    sockets.dispatch(second, hirsel_core_net::sys::EPOLLIN as u32, |t| {
        ready.push(t)
    });
    assert_eq!(ready, [101, 201, 102]);
}

/// Every closes of the opened file: the cleanup and teardown closes are site 0,
/// the program's own close is site 77.
fn closes(rec: &Rec) -> usize {
    rec.calls
        .iter()
        .filter(|c| c.cb == Cb::Issue && c.site != 1 && (c.site == 0 || c.site == FILE_SITE))
        .count()
}
/// Run `operation` on an opened file in a child, cancel the child while the
/// operation waits (W) or is issued (I), and join it.
fn cancelled_file_op(operation: u8, issued: bool) -> Rec {
    unsafe fn body(context: *mut Context, rec: *mut Rec) {
        unsafe {
            let job = FileJob {
                rec,
                operation: OPERATION.get(),
            };
            let mut input = job;
            let task = hirsel_core_spawn(
                context,
                0,
                64,
                (&mut input as *mut FileJob).cast(),
                file_job,
                TaskLayout {
                    input: ValueLayout {
                        bytes: size_of::<FileJob>(),
                        align: 8,
                        logical: 0,
                        clean: None,
                    },
                    result: ValueLayout {
                        bytes: 0,
                        align: 8,
                        logical: 0,
                        clean: None,
                    },
                },
                0,
            )
            .task;
            yield_turn(context);
            assert_eq!(
                (*rec).count(FILE_SITE, Cb::Wait),
                1,
                "the operation is parked"
            );
            hirsel_core_cancel(context, task);
            join(context, task);
        }
    }
    OPERATION.set(operation);
    let mut rec = if issued {
        Rec::new(ready, pend_file_op, never)
    } else {
        Rec::new(park_file_op, done0, never)
    };
    run(&mut rec, false, body);
    rec
}
thread_local! { static OPERATION: std::cell::Cell<u8> = const { std::cell::Cell::new(0) }; }
/// A cancelled W never issues the operation, and never loses what it
/// consumed: the file is closed exactly once, by the consuming close's
/// shielded cleanup or by teardown (core.md: close consumes the file and
/// always ends its lock). Counting provider, every file operation kind.
#[test]
fn s0_cancelled_w_file_operations_close_the_file_once() {
    use crate::provider::{CLOSE, SYNC, TRY_LOCK, UNLOCK, WRITE};
    for operation in [READ, WRITE, CLOSE, TRY_LOCK, UNLOCK, SYNC] {
        let rec = cancelled_file_op(operation, false);
        assert_eq!(
            rec.count(FILE_SITE, Cb::Issue),
            0,
            "kind {operation}: not issued"
        );
        assert_eq!(
            rec.result(FILE_SITE).0.status,
            1,
            "kind {operation}: Cancelled"
        );
        assert_eq!(closes(&rec), 1, "kind {operation}: closed once");
    }
}
/// An issued close aborted by cancellation belongs to its provider: abort
/// decides it, and the runtime closes nothing more (no second close).
#[test]
fn s0_aborted_close_is_not_closed_again() {
    let rec = cancelled_file_op(crate::provider::CLOSE, true);
    assert_eq!(rec.count(FILE_SITE, Cb::Abort(ABORT_CANCEL)), 1);
    assert_eq!(closes(&rec), 1, "only the program's own close was issued");
}
// ---- the first wait: direct call against the scheduler switch ----

unsafe extern "C" fn nothing(_: *mut Context, _: *mut u8, _: *mut u8) -> u8 {
    0
}
/// One scheduler turn for the root: a root poll yields only to a runnable
/// peer, so join an empty task; the turn takes every pending wake.
unsafe fn tick(context: *mut Context) {
    // SAFETY: the root spawns and joins its own child.
    unsafe {
        let mut unit = 0u8;
        let layout = TaskLayout {
            input: ValueLayout {
                bytes: 0,
                align: 1,
                logical: 0,
                clean: None,
            },
            result: ValueLayout {
                bytes: 0,
                align: 1,
                logical: 0,
                clean: None,
            },
        };
        let task =
            hirsel_core_spawn(context, 0, 64, (&mut unit as *mut u8), nothing, layout, 0).task;
        assert!(!task.is_null());
        assert!(!join(context, task));
    }
}

/// What a program and its provider observed: every callback with its context
/// and event position (tokens renamed by first use, since the counter is
/// process-wide), every scheduler event in order, and every job's result.
type Trace = (
    Vec<(Cb, u64, usize, bool, usize)>,
    Vec<Event>,
    Vec<(u64, u64, u8, u8)>,
);
fn trace(rec: &Rec) -> Trace {
    let mut seen = vec![];
    let calls = rec
        .calls
        .iter()
        .map(|c| {
            let token = match c.cb {
                Cb::Deliver => usize::MAX,
                _ => match seen.iter().position(|&t| t == c.token) {
                    Some(at) => at,
                    None => {
                        seen.push(c.token);
                        seen.len() - 1
                    }
                },
            };
            (c.cb, c.site, token, c.in_task, c.events)
        })
        .collect();
    let results = rec
        .results
        .iter()
        .map(|&(site, o, check)| (site, o.value, o.status, check))
        .collect();
    (calls, rec.log.clone(), results)
}
/// The first wait made by the task itself, with no task current, is the call
/// the scheduler would make right after the task parks (tasks.rs,
/// provider_operation): no task runs, no event is emitted and no wake is taken
/// between entry and that call. Oracle: the scheduler-switch path, kept behind
/// a test-only switch. For programs of two or more tasks, with every wait
/// answer at entry (Ready, Until past, Park), a ready taken in W and in I, a
/// cancellation in W, in I and at entry, and both pop orders, the provider's
/// view (each callback's context and event position) and the event order are
/// identical.
#[test]
fn s0_direct_entry_wait_matches_the_scheduler_switch() {
    fn mixed_wait(_: &mut Rec, site: u64, _: u64, nth: usize) -> Wait {
        match (site, nth) {
            (2, 0) | (4, _) | (6, 0) => Wait {
                kind: WAIT_PARK,
                deadline_ns: 0,
            },
            (3, 0) => Wait {
                kind: WAIT_UNTIL,
                deadline_ns: 1,
            },
            _ => Wait::default(),
        }
    }
    fn mixed_issue(_: &mut Rec, site: u64, _: u64, _: usize) -> Step {
        match site {
            2 | 5 | 6 => pending(0),
            _ => done(site),
        }
    }
    /// Ready at entry, Park then a ready in W and in I, Until(past), Park
    /// cancelled in W, Pending cancelled in I, a clock, and a job cancelled
    /// before it ran.
    unsafe fn mixed(context: *mut Context, rec: *mut Rec) {
        unsafe {
            let tasks: Vec<_> = [1, 2, 3, 4, 5, 6]
                .into_iter()
                .map(|site| spawn(context, rec, site, READ))
                .collect();
            let clock = spawn(context, rec, 9, crate::provider::MONO_NS);
            let early = spawn(context, rec, 7, READ);
            hirsel_core_cancel(context, early);
            while (*rec).count(6, Cb::Wait) == 0 || (*rec).count(5, Cb::Issue) == 0 {
                tick(context);
            }
            // A ready is dropped while its operation is queued, so ready
            // until the wake is seen.
            let ready_until = |site: u64, cb: Cb, n: usize| {
                while (*rec).count(site, cb) < n {
                    hirsel_core_ready((*rec).token(site));
                    tick(context);
                }
            };
            ready_until(6, Cb::Issue, 1);
            ready_until(2, Cb::Issue, 1);
            ready_until(2, Cb::Resume, 1);
            hirsel_core_cancel(context, tasks[3]);
            hirsel_core_cancel(context, tasks[4]);
            ready_until(6, Cb::Resume, 1);
            for task in tasks {
                join(context, task);
            }
            join(context, clock);
            join(context, early);
        }
    }
    /// Every job parks at entry and is readied in one order.
    unsafe fn readied(context: *mut Context, rec: *mut Rec) {
        unsafe {
            let tasks: Vec<_> = (1..=4)
                .map(|site| spawn(context, rec, site, READ))
                .collect();
            tick(context);
            for site in [3, 1, 4, 2] {
                hirsel_core_ready((*rec).token(site));
            }
            for task in tasks {
                assert!(!join(context, task));
            }
        }
    }
    /// Two jobs whose second issue readies both (F1): woken in one order,
    /// popped, with last-first choice, in the other. Driven by joins only.
    unsafe fn joined(context: *mut Context, rec: *mut Rec) {
        unsafe {
            let a = spawn(context, rec, 1, READ);
            let b = spawn(context, rec, 2, READ);
            assert!(!join(context, a));
            assert!(!join(context, b));
        }
    }
    /// (wait, issue, resume, program, pop orders). A yielding root under
    /// last-first choice is popped again at once, so programs that ready
    /// from the root run first-in-first-out only.
    type Program = (
        Answer<Wait>,
        Answer<Step>,
        Answer<Step>,
        unsafe fn(*mut Context, *mut Rec),
        &'static [bool],
    );
    let programs: [Program; 4] = [
        (mixed_wait, mixed_issue, done0, mixed, &[false]),
        (park_first, done0, never, readied, &[false]),
        (ready, done0, never, readied_ready, &[false, true]),
        (
            ready,
            ready_both_on_second_issue,
            done0,
            joined,
            &[false, true],
        ),
    ];
    /// Ready everywhere: no task parks at entry on the direct path.
    unsafe fn readied_ready(context: *mut Context, rec: *mut Rec) {
        unsafe {
            let tasks: Vec<_> = (1..=4)
                .map(|site| spawn(context, rec, site, READ))
                .collect();
            tick(context);
            for task in tasks {
                assert!(!join(context, task));
            }
        }
    }
    for (at, &(wait, issue, resume, body, orders)) in programs.iter().enumerate() {
        for &lifo_choice in orders {
            // Each half also returns its first waits, (direct, by the
            // scheduler), so each is seen to take its own path.
            let traced = |switch: bool| {
                SCHEDULER_ENTRY.set(switch);
                FIRST_WAITS.set((0, 0));
                let mut rec = Rec::new(wait, issue, resume);
                run(&mut rec, lifo_choice, body);
                SCHEDULER_ENTRY.set(false);
                (trace(&rec), FIRST_WAITS.get())
            };
            let (switched, (direct_waits, scheduler_waits)) = traced(true);
            assert!(switched.0.len() >= 8, "program {at} made its calls");
            assert!(
                scheduler_waits > 0,
                "program {at}: the scheduler made the first waits"
            );
            assert_eq!(direct_waits, 0, "program {at}");
            let (direct, waits) = traced(false);
            assert_eq!(direct, switched, "program {at}, lifo {lifo_choice}");
            assert_eq!(
                waits,
                (scheduler_waits, 0),
                "program {at}: the task made them"
            );
        }
    }
}

// ---- shields: a finalizer or shielded region observes no cancellation ----

/// How a shielded job is shielded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shield {
    /// A shielded region (CONC-8, tasks.md:140).
    Region,
    /// A finalizer (FAIL-11, failure.md:173; CONC-7, tasks.md:129).
    Finalizer,
    /// A region inside a region; the inner one ends between the operations.
    Nested,
}
/// When the parent requests the child's cancellation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum When {
    /// Before the child runs, so before its shield starts.
    Before,
    /// While the first shielded operation is parked (in W or I).
    Parked,
    /// After the operation's resume answered Done, before its deliver.
    Delivering,
    /// The parent drops its handle while the child's operation is parked:
    /// failure cleanup, which cancels and waits shielded (CONC-8).
    Dropped,
}
#[derive(Clone, Copy)]
struct ShieldJob {
    rec: *mut Rec,
    shield: Shield,
    operations: u64,
}
/// The first cancellation point after the shield.
const AFTER: u64 = 10;
/// Operations at sites 1..=operations under the shield, then one at AFTER and
/// a check_cancel. Fails when either observed the request.
unsafe extern "C" fn shielded_job(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
    // SAFETY: the job is copied into the task; its Rec outlives the run.
    unsafe {
        let job = input.cast::<ShieldJob>().read();
        let provider = (*job.rec).provider();
        let depth = if job.shield == Shield::Finalizer {
            &raw mut (*context).finalizing
        } else {
            &raw mut (*context).cancel_shield
        };
        *depth += 1;
        for site in 1..=job.operations {
            let inner = job.shield == Shield::Nested && site == 1;
            if inner {
                (*context).cancel_shield += 1;
            }
            let mut req = request(READ, site);
            let outcome = hirsel_core_host(context, &provider, &mut req);
            (*job.rec).results.push((site, outcome, 0));
            if inner {
                (*context).cancel_shield -= 1;
            }
        }
        *depth -= 1;
        let mut req = request(READ, AFTER);
        let outcome = hirsel_core_host(context, &provider, &mut req);
        let check = hirsel_core_check_cancel(context, AFTER + 1000);
        (*job.rec).results.push((AFTER, outcome, check));
        u8::from(outcome.status != 0 || check != 0)
    }
}
thread_local! {
    static SHIELD_CASE: std::cell::Cell<(Shield, u64, When)> =
        const { std::cell::Cell::new((Shield::Region, 1, When::Parked)) };
    static SHIELD_TARGET: std::cell::Cell<*mut Task> = const { std::cell::Cell::new(ptr::null_mut()) };
}
/// The peer of When::Delivering: its operation is readied just before the
/// shielded one, so it runs at the turn that took both readies, after the
/// shielded operation's resume answered Done and before its deliver, and
/// requests the cancellation then.
unsafe extern "C" fn canceller(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
    // SAFETY: as job; the target outlives this peer, which the parent joins first.
    unsafe {
        let job = input.cast::<Job>().read();
        let provider = (*job.rec).provider();
        let mut req = request(READ, job.site);
        hirsel_core_host(context, &provider, &mut req);
        let rec = &*job.rec;
        assert_eq!(
            (rec.count(1, Cb::Resume), rec.count(1, Cb::Deliver)),
            (1, 0),
            "between Done and deliver"
        );
        hirsel_core_cancel(context, SHIELD_TARGET.get());
        0
    }
}
const SHIELD_WAKE_NS: u64 = 20_000_000;
/// W: the first wait of a shielded operation is Until 20 ms on, then Ready.
fn shield_w_wait(_: &mut Rec, site: u64, _: u64, nth: usize) -> Wait {
    if site < AFTER && nth == 0 {
        Wait {
            kind: WAIT_UNTIL,
            deadline_ns: crate::os::monotonic_ns() + SHIELD_WAKE_NS,
        }
    } else {
        Wait::default()
    }
}
/// I: issue is Pending and another thread readies it 20 ms on.
fn shield_i_issue(_: &mut Rec, _: u64, token: u64, _: usize) -> Step {
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_nanos(SHIELD_WAKE_NS));
        hirsel_core_ready(token);
    });
    pending(0)
}
/// I with the step's own deadline 20 ms on.
fn shield_deadline_issue(_: &mut Rec, _: u64, _: u64, _: usize) -> Step {
    pending(crate::os::monotonic_ns() + SHIELD_WAKE_NS)
}
unsafe fn shield_parent(context: *mut Context, rec: *mut Rec) {
    // SAFETY: the parent owns the child's handle; Rec outlives the run.
    unsafe {
        let (shield, operations, when) = SHIELD_CASE.get();
        let mut job = ShieldJob {
            rec,
            shield,
            operations,
        };
        let layout = TaskLayout {
            input: ValueLayout {
                bytes: size_of::<ShieldJob>(),
                align: 8,
                logical: 0,
                clean: None,
            },
            result: ValueLayout {
                bytes: 0,
                align: 8,
                logical: 0,
                clean: None,
            },
        };
        let task = hirsel_core_spawn(
            context,
            0,
            64,
            (&mut job as *mut ShieldJob).cast(),
            shielded_job,
            layout,
            0,
        )
        .task;
        assert!(!task.is_null());
        if when == When::Before {
            hirsel_core_cancel(context, task);
        }
        // The first shielded operation parks, in W after its wait or in I
        // after its issue.
        while (*rec).count(1, Cb::Wait) == 0 {
            tick(context);
        }
        assert_eq!((*rec).count(1, Cb::Deliver), 0);
        let parked = (*rec).calls.len();
        match when {
            When::Before => {}
            When::Parked => {
                hirsel_core_cancel(context, task);
                tick(context);
                // The request changed nothing: no wait, no abort, no issue
                // skipped.
                assert_eq!((*rec).calls.len(), parked, "{:?}", (*rec).calls);
            }
            When::Delivering => {
                let mut peer = Job {
                    rec,
                    site: 50,
                    operation: READ,
                };
                let peer = hirsel_core_spawn(
                    context,
                    0,
                    64,
                    (&mut peer as *mut Job).cast(),
                    canceller,
                    job_layout(),
                    0,
                )
                .task;
                while (*rec).count(50, Cb::Issue) == 0 {
                    tick(context);
                }
                SHIELD_TARGET.set(task);
                hirsel_core_ready((*rec).token(50));
                hirsel_core_ready((*rec).token(1));
                assert!(!join(context, peer));
            }
            When::Dropped => {
                assert_eq!(hirsel_core_task_drop(context, task), 0);
                return;
            }
        }
        assert!(join(context, task), "the point after the shield fails");
    }
}
/// A cancellation request does not reach an operation that a finalizer or a
/// shielded region is parked in (CONC-7, tasks.md:129; FAIL-11,
/// failure.md:173; CONC-8, tasks.md:140): whether made before the shield,
/// while the operation is in W or I, between its Done and its deliver, or by a
/// failing parent dropping the handle, it wakes nothing, aborts nothing and
/// skips no issue; every shielded operation, of one or two, under a nested
/// shield too, delivers its own outcome. The operation's own deadline is not
/// cancellation: it still times out. The request takes effect at the first
/// cancellation point after the shield, which fails Cancelled with no provider
/// call, exactly once (the check after it sees nothing).
#[test]
fn s0_a_shield_holds_a_cancellation_until_after_it() {
    let w = (
        shield_w_wait as Answer<Wait>,
        done0 as Answer<Step>,
        never as Answer<Step>,
    );
    let i = (
        ready as Answer<Wait>,
        shield_i_issue as Answer<Step>,
        done0 as Answer<Step>,
    );
    let held = (
        ready as Answer<Wait>,
        pending_forever as Answer<Step>,
        done0 as Answer<Step>,
    );
    let deadline = (
        ready as Answer<Wait>,
        shield_deadline_issue as Answer<Step>,
        never as Answer<Step>,
    );
    let mut cases = vec![];
    for phase in ["W", "I"] {
        let answers = if phase == "W" { w } else { i };
        for (shield, operations) in [
            (Shield::Region, 1),
            (Shield::Finalizer, 1),
            (Shield::Finalizer, 2),
            (Shield::Nested, 2),
        ] {
            for when in [When::Before, When::Parked] {
                cases.push((phase, answers, shield, operations, when));
            }
        }
        for operations in [1, 2] {
            cases.push((phase, answers, Shield::Finalizer, operations, When::Dropped));
        }
    }
    for shield in [Shield::Region, Shield::Finalizer] {
        cases.push(("I", held, shield, 1, When::Delivering));
        cases.push(("deadline", deadline, shield, 1, When::Parked));
    }
    for (phase, (wait, issue, resume), shield, operations, when) in cases {
        let case = format!("{phase} {shield:?} x{operations} {when:?}");
        SHIELD_CASE.set((shield, operations, when));
        let mut rec = Rec::new(wait, issue, resume);
        run(&mut rec, false, shield_parent);
        for site in 1..=operations {
            let (outcome, _) = rec.result(site);
            assert_eq!(rec.count(site, Cb::Abort(ABORT_CANCEL)), 0, "{case}");
            assert_eq!(rec.count(site, Cb::Issue), 1, "{case}");
            assert_eq!(rec.count(site, Cb::Deliver), 1, "{case}");
            if phase == "deadline" {
                assert_eq!(rec.count(site, Cb::Abort(ABORT_DEADLINE)), 1, "{case}");
                assert_eq!((outcome.status, outcome.value), (0, TIMED_OUT), "{case}");
            } else {
                assert_eq!((outcome.status, outcome.value), (0, site), "{case}");
            }
        }
        let (after, check) = rec.result(AFTER);
        assert_eq!((after.status, check), (1, 0), "{case}: Cancelled, once");
        assert_eq!(
            rec.calls.iter().filter(|c| c.site == AFTER).count(),
            0,
            "{case}"
        );
    }
}

// ---- the OS provider's own callbacks: by identity, by pointer, forwarded ----

/// One step of an OS-provider job.
#[derive(Clone, Copy)]
enum OsStep {
    Out(&'static [u8]),
    Clock(u8),
    /// SLEEP_UNTIL this many nanoseconds after a MONO_NS read; 0 is a deadline
    /// already past.
    Sleep(u64),
    /// Open a file named for the job's site, write these bytes, close it.
    File(&'static [u8]),
}
#[derive(Clone, Copy)]
struct OsJob {
    site: u64,
    steps: &'static [OsStep],
}
/// A job's operation as the program sees it: site, operation, status, error
/// tag and code, count written, and the value (zero for the clock reads).
type OsResult = (u64, u8, u8, u8, u32, u64, u64);
thread_local! {
    /// The provider the jobs call, the granted directory, and their results.
    static OS_RUN: std::cell::Cell<(*const Provider, u64, *mut Vec<OsResult>)> =
        const { std::cell::Cell::new((ptr::null(), 0, ptr::null_mut())) };
}
unsafe extern "C" fn os_job(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
    // SAFETY: the job is copied into the task; the run pins provider and results.
    unsafe {
        let job = input.cast::<OsJob>().read();
        let (provider, directory, results) = OS_RUN.get();
        let mut failed = false;
        let mut op = |mut request: Request| {
            let outcome = hirsel_core_host(context, provider, &mut request);
            let op = request.operation;
            let clock = op == crate::provider::MONO_NS || op == crate::provider::WALL_MS;
            let value = if clock { 0 } else { outcome.value };
            let e = outcome.error;
            (*results).push((
                job.site,
                op,
                outcome.status,
                e.tag,
                e.code,
                outcome.written,
                value,
            ));
            failed |= outcome.status != 0;
            outcome
        };
        let bytes = |mut request: Request, data: &[u8]| {
            request.buffer = Buffer {
                data: data.as_ptr().cast_mut(),
                len: data.len(),
                capacity: data.len(),
            };
            request
        };
        for step in job.steps {
            match *step {
                OsStep::Out(data) => {
                    op(bytes(request(crate::provider::STDOUT, job.site), data));
                }
                OsStep::Clock(operation) => {
                    op(request(operation, job.site));
                }
                OsStep::Sleep(after) => {
                    let deadline = if after == 0 {
                        0
                    } else {
                        op(request(crate::provider::MONO_NS, job.site)).value + after
                    };
                    let deadline = deadline.to_ne_bytes();
                    op(bytes(
                        request(crate::provider::SLEEP_UNTIL, job.site),
                        &deadline,
                    ));
                }
                OsStep::File(data) => {
                    let name = format!("f{}", job.site);
                    let mut open = request(crate::provider::OPEN, job.site);
                    (open.mode, open.handle) = (1, directory);
                    (open.path, open.path_len) = (name.as_ptr(), name.len());
                    let opened = op(open);
                    assert_eq!(opened.status, 0, "open {name}");
                    let file = opened.value;
                    let mut write = bytes(request(crate::provider::WRITE, job.site), data);
                    write.handle = file;
                    op(write);
                    let mut close = request(crate::provider::CLOSE, job.site);
                    close.handle = file;
                    op(close);
                }
            }
        }
        u8::from(failed)
    }
}
unsafe fn spawn_os(context: *mut Context, site: u64, steps: &'static [OsStep]) -> *mut Task {
    let mut input = OsJob { site, steps };
    let value = |bytes| ValueLayout {
        bytes,
        align: 8,
        logical: 0,
        clean: None,
    };
    let layout = TaskLayout {
        input: value(size_of::<OsJob>()),
        result: value(0),
    };
    // SAFETY: the job is copied into the child.
    let task = unsafe {
        let input = (&mut input as *mut OsJob).cast();
        hirsel_core_spawn(context, 0, 64, input, os_job, layout, 0).task
    };
    assert!(!task.is_null());
    task
}
const OS_ONCE: u64 = 1;
const OS_LATER: u64 = 2;
const OS_CANCELLED: u64 = 3;
/// Every OS operation kind the jobs reach, with sleeps already due, due
/// later (Until, then a second wait) and cancelled while waiting.
unsafe fn os_root(context: *mut Context, _: *mut Rec) {
    use crate::provider::{MONO_NS, WALL_MS};
    const ONCE: &[OsStep] = &[
        OsStep::Out(b"ab"),
        OsStep::Clock(MONO_NS),
        OsStep::Out(b"cd"),
        OsStep::Sleep(0),
        OsStep::Clock(WALL_MS),
        OsStep::Out(b"ef"),
    ];
    const LATER: &[OsStep] = &[
        OsStep::Sleep(2_000_000),
        OsStep::Out(b"gh"),
        OsStep::File(b"file"),
    ];
    const CANCELLED: &[OsStep] = &[OsStep::Sleep(3_600_000_000_000)];
    // SAFETY: the root owns every handle it spawns.
    unsafe {
        let once = spawn_os(context, OS_ONCE, ONCE);
        let later = spawn_os(context, OS_LATER, LATER);
        let cancelled = spawn_os(context, OS_CANCELLED, CANCELLED);
        tick(context);
        tick(context);
        hirsel_core_cancel(context, cancelled);
        assert!(!join(context, once));
        assert!(!join(context, later));
        assert!(join(context, cancelled));
    }
}
/// A provider that forwards every callback to the one it wraps.
unsafe fn inner<'a>(state: *mut u8) -> &'a Provider {
    // SAFETY: the wrapper's state is the wrapped provider, pinned for the run.
    unsafe { &*state.cast::<Provider>() }
}
unsafe extern "C" fn fw_wait(state: *mut u8, request: *mut Request, token: u64) -> Wait {
    // SAFETY: forwarded.
    unsafe { (inner(state).wait)(inner(state).state, request, token) }
}
unsafe extern "C" fn fw_issue(state: *mut u8, request: *mut Request, token: u64) -> Step {
    // SAFETY: forwarded.
    unsafe { (inner(state).issue)(inner(state).state, request, token) }
}
unsafe extern "C" fn fw_resume(state: *mut u8, request: *mut Request, token: u64) -> Step {
    // SAFETY: forwarded.
    unsafe { (inner(state).resume)(inner(state).state, request, token) }
}
unsafe extern "C" fn fw_abort(
    state: *mut u8,
    request: *mut Request,
    token: u64,
    why: u8,
) -> Outcome {
    // SAFETY: forwarded.
    unsafe { (inner(state).abort)(inner(state).state, request, token, why) }
}
unsafe extern "C" fn fw_deliver(
    state: *mut u8,
    request: *mut Request,
    outcome: Outcome,
) -> Outcome {
    // SAFETY: forwarded.
    unsafe { (inner(state).deliver)(inner(state).state, request, outcome) }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OsForm {
    /// The OS provider, its own callbacks called directly.
    Direct,
    /// The OS provider, every callback called through its pointer.
    Pointer,
    /// A forwarding wrapper over the OS provider.
    Wrapper,
}
/// What one run of `os_root` showed: the OS callbacks as called, the jobs'
/// results, the scheduler's events (kind, task), stdout and the file written.
#[derive(Debug, PartialEq, Eq)]
struct OsSeen {
    calls: Vec<crate::os::probe::Call>,
    results: Vec<OsResult>,
    events: Vec<(u8, u64)>,
    stdout: Vec<u8>,
    file: Vec<u8>,
}
fn os_case(form: OsForm) -> (OsSeen, usize) {
    let scratch = tempfile::tempdir().unwrap();
    let root = scratch.path();
    let mut os = Box::new(crate::os::OsProvider::new(
        std::fs::File::create(root.join("stdout")).unwrap().into(),
        std::fs::File::create(root.join("stderr")).unwrap().into(),
    ));
    let directory = os.grant(root).unwrap();
    let mut own = os.provider();
    let wrapper = Provider {
        state: (&mut own as *mut Provider).cast(),
        wait: fw_wait,
        issue: fw_issue,
        resume: fw_resume,
        abort: fw_abort,
        deliver: fw_deliver,
    };
    let provider: *const Provider = if form == OsForm::Wrapper {
        &wrapper
    } else {
        &own
    };
    let mut results = vec![];
    OS_RUN.set((provider, directory, &mut results));
    OS_DIRECT.set(form != OsForm::Pointer);
    OS_DIRECT_CALLS.set(0);
    crate::os::probe::CALLS.set(Some(vec![]));
    let mut rec = Rec::new(ready, done0, never);
    run(&mut rec, false, os_root);
    let calls = crate::os::probe::CALLS.take().unwrap();
    let direct = OS_DIRECT_CALLS.get();
    OS_DIRECT.set(true);
    OS_RUN.set((ptr::null(), 0, ptr::null_mut()));
    drop(os);
    let seen = OsSeen {
        calls,
        results,
        events: rec.log.iter().map(|e| (e.kind, e.task)).collect(),
        stdout: std::fs::read(root.join("stdout")).unwrap(),
        file: std::fs::read(root.join(format!("f{OS_LATER}"))).unwrap(),
    };
    (seen, direct)
}
/// The OS provider's own `wait` and `deliver` are selected by identity and
/// called directly; that may change the call form only. The same jobs run with
/// the direct calls, with every callback through its pointer, and through a
/// forwarding wrapper: the callbacks are called alike (order, operation, site,
/// task current or not, answer), and the results, the scheduler's events and
/// the bytes written are the same. The oracle for each run is section 3: wait
/// outside any task, issue and deliver in it; a sleep due later waits Until,
/// then Ready, then issues and delivers once; a sleep cancelled in W waits
/// again at the cancellation and is never issued. The count of direct calls shows which form each run took.
#[test]
fn s0_os_callbacks_by_identity_match_the_pointer_calls() {
    use crate::os::probe::{DELIVER, ISSUE, WAIT};
    use crate::provider::SLEEP_UNTIL;
    let (direct, direct_calls) = os_case(OsForm::Direct);
    for &(callback, operation, site, in_task, answer) in &direct.calls {
        assert_eq!(in_task, callback != WAIT, "{callback} {operation} {site}");
        if callback == WAIT && operation != SLEEP_UNTIL {
            assert_eq!(answer, u64::from(crate::provider::WAIT_READY));
        }
    }
    let sleeps = |site| {
        direct
            .calls
            .iter()
            .filter(|c| c.1 == SLEEP_UNTIL && c.2 == site)
            .map(|c| (c.0, c.4))
            .collect::<Vec<_>>()
    };
    let until = u64::from(WAIT_UNTIL);
    assert_eq!(
        sleeps(OS_LATER),
        [(WAIT, until), (WAIT, 0), (ISSUE, 0), (DELIVER, 0)]
    );
    // Section 3 step 3: the cancellation request is a wake, and `wait` runs
    // again at it; the operation then fails Cancelled, never issued.
    assert_eq!(sleeps(OS_CANCELLED), [(WAIT, until), (WAIT, until)]);
    assert_eq!(sleeps(OS_ONCE), [(WAIT, 0), (ISSUE, 0), (DELIVER, 0)]);
    let issued = direct.calls.iter().filter(|c| c.0 == ISSUE).count();
    let delivered = direct.calls.iter().filter(|c| c.0 == DELIVER).count();
    assert_eq!((issued, delivered), (direct.results.len() - 1, issued));
    assert_eq!(direct.stdout, b"abcdefgh");
    assert_eq!(direct.file, b"file");
    assert!(direct_calls > 0, "the identity selected the direct calls");
    let (pointer, pointer_calls) = os_case(OsForm::Pointer);
    assert_eq!(pointer_calls, 0, "the switch forced the pointer calls");
    assert_eq!(pointer, direct);
    let (wrapper, wrapper_calls) = os_case(OsForm::Wrapper);
    assert_eq!(wrapper_calls, 0, "a wrapper is never the OS provider");
    assert_eq!(wrapper, direct);
}

#[test]
fn two_workers_discard_stale_readiness_after_pending_rearm() {
    use crate::schedule_script::PausePoint;
    use std::time::{Duration, Instant};

    const TARGET: u64 = 1;
    const SENTINEL: u64 = 2;
    #[derive(Default)]
    struct State {
        sentinel: AtomicU64,
        captures: AtomicU64,
        resumes: AtomicU64,
        delivered: AtomicU64,
    }

    fn wait_for(value: &AtomicU64) -> u64 {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let n = value.load(Ordering::Acquire);
            if n != 0 {
                return n;
            }
            assert!(Instant::now() < deadline, "the other worker did not run");
            std::thread::yield_now();
        }
    }

    unsafe extern "C" fn wait(_: *mut u8, _: *mut Request, _: u64) -> Wait {
        Wait::default()
    }
    unsafe extern "C" fn issue(state: *mut u8, request: *mut Request, token: u64) -> Step {
        // SAFETY: the fixture retains the shared atomic state and live request.
        unsafe {
            let state = &*state.cast::<State>();
            if (*request).site == SENTINEL {
                state.sentinel.store(token, Ordering::Release);
            } else {
                // The two notifications are captured together. The sentinel
                // hook runs after the dispatcher has applied the stale input.
                hirsel_core_ready(token);
                hirsel_core_ready(state.sentinel.load(Ordering::Acquire));
            }
        }
        pending(0)
    }
    unsafe extern "C" fn resume(state: *mut u8, request: *mut Request, _: u64) -> Step {
        // SAFETY: as issue.
        unsafe {
            if (*request).site == SENTINEL {
                return done(2);
            }
            let state = &*state.cast::<State>();
            match state.resumes.fetch_add(1, Ordering::Release) {
                0 => pending(0),
                1 => done(7),
                _ => unreachable!("the target already finished"),
            }
        }
    }
    unsafe extern "C" fn deliver(state: *mut u8, request: *mut Request, value: Outcome) -> Outcome {
        // SAFETY: as issue.
        unsafe {
            if (*request).site == TARGET {
                (*state.cast::<State>())
                    .delivered
                    .fetch_add(1, Ordering::Relaxed);
            }
        }
        value
    }

    unsafe extern "C" fn sentinel(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
        // SAFETY: the supervisor retains the fixture until join.
        unsafe {
            let provider = input.cast::<Provider>().read();
            let outcome = hirsel_core_host(context, &provider, &mut request(READ, SENTINEL));
            assert_eq!((outcome.status, outcome.value), (0, 2));
        }
        0
    }
    unsafe extern "C" fn root(context: *mut Context, out: *mut u8, input: *mut u8) -> u8 {
        // SAFETY: root owns the child handle and result destination.
        unsafe {
            let mut provider = input.cast::<Provider>().read();
            let child = hirsel_core_spawn(
                context,
                0,
                0,
                (&raw mut provider).cast(),
                sentinel,
                crate::tasks_tests::layout::<Provider, ()>(0),
                11,
            )
            .task;
            assert!(!child.is_null());
            wait_for(&(*provider.state.cast::<State>()).sentinel);
            let outcome = hirsel_core_host(context, &provider, &mut request(READ, TARGET));
            assert_eq!(outcome.status, 0);
            out.cast::<u64>().write(outcome.value);
            let joined = hirsel_core_join(context, child, ptr::null_mut(), 12);
            assert_eq!((joined.kind, joined.status), (0, 0));
        }
        0
    }

    let state = Arc::new(State::default());
    let target = Arc::new(AtomicU64::new(0));
    let script = Arc::new(ScheduleScript::new(0).with_pause_hook({
        let state = state.clone();
        let target = target.clone();
        move |point| {
            let PausePoint::ReadinessTaken { token } = point else {
                return;
            };
            if token == state.sentinel.load(Ordering::Acquire) {
                assert_eq!(state.resumes.load(Ordering::Acquire), 1);
                hirsel_core_ready(target.load(Ordering::Acquire));
            } else if state.captures.fetch_add(1, Ordering::Relaxed) == 0 {
                target.store(token, Ordering::Release);
                hirsel_core_ready(token);
                wait_for(&state.resumes);
            }
        }
    }));
    let provider = Provider {
        state: Arc::as_ptr(&state).cast_mut().cast(),
        wait,
        issue,
        resume,
        abort: crate::provider::immediate::abort,
        deliver,
    };
    let (result, _) =
        script.install(|| with_workers(2, || crate::tasks_tests::run(root, provider)));
    assert_eq!(result, 7);
    assert_eq!(state.captures.load(Ordering::Relaxed), 3);
    assert_eq!(state.resumes.load(Ordering::Relaxed), 2);
    assert_eq!(state.delivered.load(Ordering::Relaxed), 1);
}
