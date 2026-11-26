use super::*;
use crate::entry::{CLOCK, Grant, hirsel_core_capability_provider};
use crate::process::{Config, MainResult, hirsel_core_run};
use crate::provide::{
    ChannelClock, hirsel_core_clock_advance_to, hirsel_core_clock_idle, hirsel_core_provide_clock,
};
use crate::tasks::*;
use crate::tasks_tests::{layout, value};

#[derive(Clone, Copy)]
struct Advance {
    hand: u64,
    sender: *mut Channel,
}

unsafe fn send(tx: *mut Channel, mut value: u64) {
    // SAFETY: the sender is live and copies this scalar into its queue.
    assert_eq!(
        unsafe { hirsel_core_try_send(tx, (&raw mut value).cast()) },
        0
    );
}

unsafe fn expect_idle(context: *mut Context, hand: u64, expected: u64) {
    // SAFETY: this task holds the hand exclusively.
    let idle = unsafe { hirsel_core_clock_idle(context, hand, 0) };
    assert_eq!((idle.status, idle.written, idle.value), (0, 1, expected));
}

unsafe extern "C" fn advance(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
    // SAFETY: the parent pins the copied hand and sender until join.
    unsafe {
        let a = input.cast::<Advance>().read();
        expect_idle(context, a.hand, 10);
        hirsel_core_clock_advance_to(context, a.hand, 10, 0);
        send(a.sender, 7);
        expect_idle(context, a.hand, 20);
        hirsel_core_clock_advance_to(context, a.hand, 20, 0);
        expect_idle(context, a.hand, 30);
        send(a.sender, 9);
        expect_idle(context, a.hand, 40);
        hirsel_core_clock_advance_to(context, a.hand, 30, 0);
        checkpoint();
        expect_idle(context, a.hand, 40);
        hirsel_core_clock_advance_to(context, a.hand, 40, 0);
    }
    0
}

unsafe extern "C" fn close(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
    // SAFETY: the child owns the last sender and borrows the held clock.
    unsafe {
        let a = input.cast::<Advance>().read();
        expect_idle(context, a.hand, 60);
        hirsel_core_sender_drop(a.sender);
    }
    0
}

#[derive(Clone, Copy)]
struct Cancelled {
    receiver: *mut Channel,
    clock: u64,
}

unsafe extern "C" fn cancelled(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
    // SAFETY: this is the sole receiver until the parent joins.
    unsafe {
        let a = input.cast::<Cancelled>().read();
        let admissions = (*context).current.attempts();
        let mut destination = u64::MAX;
        let answer = hirsel_core_receive_until(
            context,
            a.receiver,
            (&raw mut destination).cast(),
            1,
            a.clock,
            50,
            0,
        );
        assert_eq!((answer.status, destination), (1, u64::MAX));
        assert_eq!((*context).current.attempts(), admissions);
        answer.status
    }
}

unsafe fn expect(
    context: *mut Context,
    rx: *mut Channel,
    clock: u64,
    at: u64,
    kind: u8,
    value: u64,
) {
    // SAFETY: the caller exclusively owns the receiver and result slot.
    unsafe {
        let available = (*context).available();
        let mut destination = u64::MAX;
        let got =
            hirsel_core_receive_until(context, rx, (&raw mut destination).cast(), 1, clock, at, 0);
        assert_eq!((got.kind, got.status, destination), (kind, 0, value));
        assert_eq!((*context).available(), available);
        assert!((*rx).receive_waiters.is_empty());
        if let Some(clock) = ChannelClock::of(&*hirsel_core_capability_provider(context, clock)) {
            assert!(clock.first().is_none());
        }
        assert!(!channel_deadline_registered());
    }
}

unsafe extern "C" fn run(context: *mut Context, config: *const Config, _: *mut MainResult) -> u8 {
    // SAFETY: the run pins the grants. Every child is joined before its input
    // resources are used elsewhere.
    unsafe {
        let clock = hirsel_core_provide_clock(context, 0, 0, 0);
        assert_eq!(clock.status, 0);
        let q = hirsel_core_channel_new(context, 2, value::<u64>(8), 0);
        assert_eq!(q.status, 0);
        let mut advance_input = Advance {
            hand: clock.written,
            sender: q.sender,
        };
        let child = hirsel_core_spawn(
            context,
            0,
            0,
            (&raw mut advance_input).cast(),
            advance,
            layout::<Advance, ()>(0),
            0,
        );
        assert!(!child.task.is_null());

        expect(context, q.receiver, clock.value, 10, 0, 7);
        expect(context, q.receiver, clock.value, 20, 1, u64::MAX);
        send(q.sender, 8);
        send(q.sender, 81);
        expect(context, q.receiver, clock.value, 19, 0, 8);
        expect(context, q.receiver, clock.value, 19, 0, 81);
        expect(context, q.receiver, clock.value, 19, 1, u64::MAX);

        expect(context, q.receiver, clock.value, 30, 0, 9);
        expect(context, q.receiver, clock.value, 40, 1, u64::MAX);
        let joined = hirsel_core_join(context, child.task, ptr::null_mut(), 0);
        assert_eq!((joined.kind, joined.status), (0, 0));

        let mut input = Cancelled {
            receiver: q.receiver,
            clock: clock.value,
        };
        let child = hirsel_core_spawn(
            context,
            0,
            0,
            (&raw mut input).cast(),
            cancelled,
            layout::<Cancelled, ()>(0),
            0,
        );
        assert!(!child.task.is_null());
        expect_idle(context, clock.written, 50);
        hirsel_core_cancel(context, child.task);
        send(q.sender, 100);
        let joined = hirsel_core_join(context, child.task, ptr::null_mut(), 0);
        assert_eq!((joined.kind, joined.status), (2, 0));
        crate::abi::hirsel_core_causes_drop(joined.causes);
        assert!((*q.receiver).receive_waiters.is_empty());
        let clock_cell =
            ChannelClock::of(&*hirsel_core_capability_provider(context, clock.value)).unwrap();
        assert!(
            clock_cell.first().is_none(),
            "the held clock still has a cancelled registration"
        );

        send(q.sender, 101);
        let mut value = 0u64;
        let next = hirsel_core_receive(context, q.receiver, (&raw mut value).cast(), 0);
        assert!(next.transferred);
        assert_eq!(value, 100);
        hirsel_core_sender_drop(q.sender);
        expect(context, q.receiver, clock.value, 39, 0, 101);
        expect(context, q.receiver, clock.value, 39, 2, u64::MAX);
        hirsel_core_receiver_drop(context, q.receiver);
        let q = hirsel_core_channel_new(context, 1, crate::tasks_tests::value::<u64>(8), 0);
        let mut input = Advance {
            hand: clock.written,
            sender: q.sender,
        };
        let child = hirsel_core_spawn(
            context,
            0,
            0,
            (&raw mut input).cast(),
            close,
            layout::<Advance, ()>(0),
            0,
        );
        assert!(!child.task.is_null());
        expect(context, q.receiver, clock.value, 60, 2, u64::MAX);
        let joined = hirsel_core_join(context, child.task, ptr::null_mut(), 0);
        assert_eq!((joined.kind, joined.status), (0, 0));
        hirsel_core_receiver_drop(context, q.receiver);
        crate::host_file::hirsel_core_hostfile_drop(context, clock.written);

        let q = hirsel_core_channel_new(context, 1, crate::tasks_tests::value::<u64>(8), 0);
        let host = crate::entry::hirsel_core_entry_grant(config, CLOCK, 0);
        let start = crate::host_ops::hirsel_core_host_clock(
            context,
            crate::provider::MONO_NS,
            host,
            ptr::null_mut(),
            0,
            0,
        );
        expect(
            context,
            q.receiver,
            host,
            start.value + 40_000_000,
            1,
            u64::MAX,
        );
        hirsel_core_sender_drop(q.sender);
        hirsel_core_receiver_drop(context, q.receiver);
    }
    0
}

#[test]
fn receive_until() {
    let fd = || std::fs::File::open("/dev/null").unwrap().into();
    let mut os = crate::os::OsProvider::new(fd(), fd());
    let provider = os.provider();
    let grant = Grant {
        kind: CLOCK,
        provider: &provider,
        handle: 0,
        terminal: false,
    };
    let config = Config {
        launch: ptr::null(),
        describe_output: None,
        root_limit: 4096,
        args: ptr::null(),
        args_len: 0,
        grants: ptr::null(),
        grants_len: 0,
        provider: ptr::null(),
        state: ptr::null_mut(),
        diagnostic: super::diagnostic,
        describe: None,
        scheduler: SchedulerProvider::default(),
        stop_fd: 2,
        capabilities: &grant,
        capabilities_len: 1,
        close_grants: None,
    };
    // SAFETY: the provider and its grant outlive the run and all its children.
    assert_eq!(unsafe { hirsel_core_run(&config, run) }, 0);
    #[cfg(feature = "record")]
    crate::tape::schedule_cases::channel_deadline::check();
}
