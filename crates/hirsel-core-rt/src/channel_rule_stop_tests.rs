//! FAIL-15 full counters stop the process before cleanup or join delivery.
use super::*;
use crate::{process::*, tasks::*};
use std::process::Command;
#[derive(Clone, Copy)]
enum Stop {
    Rc,
    Arc,
    Sender,
}
struct Witness;
impl Drop for Witness {
    fn drop(&mut self) {
        // SAFETY: static diagnostic; a run stop must never reach this destructor.
        unsafe {
            libc::write(2, b"CLEANUP\n".as_ptr().cast(), 8);
        }
    }
}
unsafe extern "C" fn inert(_: *mut Context, _: *mut u8) -> u8 {
    0
}
fn scalar<T>() -> ValueLayout {
    ValueLayout {
        bytes: size_of::<T>(),
        align: align_of::<T>(),
        logical: 0,
        drop: inert,
    }
}
unsafe extern "C" fn child_body(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
    let _cleanup = Witness;
    // SAFETY: each child owns its input and fresh node/channel. Symbolic count
    // setup supplies a valid full u64 count without performing 2^64 operations.
    unsafe {
        match input.cast::<Stop>().read() {
            Stop::Rc | Stop::Arc => {
                let atomic = matches!(input.cast::<Stop>().read(), Stop::Arc);
                let node = crate::block::Block::graph(&*context, Some(16), 16, 8)
                    .unwrap()
                    .into_raw();
                crate::counted::initialize(node, atomic);
                node.cast::<u64>().as_ptr().write(u64::MAX);
                crate::counted::share(node, atomic);
            }
            Stop::Sender => {
                let queue = hirsel_core_channel_new(context, 1, scalar::<()>(), 1);
                (*queue.sender).senders = u64::MAX;
                hirsel_core_channel_sender(queue.sender);
            }
        }
        // Stop must not become a successful task result observed by its joiner.
        libc::write(2, b"CHILD RETURNED\n".as_ptr().cast(), 15);
    }
    0
}
unsafe extern "C" fn entry(context: *mut Context, cfg: *const Config, _: *mut MainResult) -> u8 {
    let _cleanup = Witness;
    // SAFETY: child input is a copied enum; parent owns its handle through join.
    unsafe {
        let mut stop = (*cfg).state.cast::<Stop>().read();
        let child = hirsel_core_spawn(
            context,
            64,
            (&mut stop as *mut Stop).cast(),
            child_body,
            TaskLayout {
                input: scalar::<Stop>(),
                result: scalar::<()>(),
            },
        );
        let mut unit = 0u8;
        hirsel_core_join(context, child.task, &mut unit, 5);
        libc::write(2, b"JOINER\n".as_ptr().cast(), 7);
    }
    0
}
unsafe extern "C" fn diagnostic(_: *mut u8, p: *const u8, n: usize) {
    // SAFETY: diagnostic borrows its complete initialized byte range.
    unsafe {
        libc::write(2, p.cast(), n);
    }
}
unsafe extern "C" fn grants_closed(_: *mut u8) {
    // SAFETY: static teardown marker; a run stop must skip it as well.
    unsafe {
        libc::write(2, b"GRANTS\n".as_ptr().cast(), 7);
    }
}
fn run_child(mut stop: Stop) {
    let config = Config {
        root_limit: 4096,
        args: std::ptr::null(),
        args_len: 0,
        grants: std::ptr::null(),
        grants_len: 0,
        provider: std::ptr::null(),
        state: (&mut stop as *mut Stop).cast(),
        diagnostic,
        describe: None,
        scheduler: SchedulerProvider::default(),
        stop_fd: 2,
        capabilities: std::ptr::null(),
        capabilities_len: 0,
        close_grants: Some(grants_closed),
    };
    // SAFETY: input/config stay pinned through the run; entry never returns on a stop.
    let status = unsafe { hirsel_core_run(&config, entry) };
    panic!("run stop returned {status}");
}
#[test]
#[ignore = "symbolic stop child, invoked by named parent"]
fn rc_overflow_child() {
    run_child(Stop::Rc)
}
#[test]
#[ignore = "symbolic stop child, invoked by named parent"]
fn arc_overflow_child() {
    run_child(Stop::Arc)
}
#[test]
#[ignore = "symbolic stop child, invoked by named parent"]
fn sender_overflow_child() {
    run_child(Stop::Sender)
}
fn expect_stop(child: &str, diagnostic: &[u8]) {
    let result = Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", child, "--test-threads=1"])
        .output()
        .unwrap();
    assert_eq!(
        result.status.code(),
        Some(102),
        "{child}: {}\n{}",
        result.status,
        String::from_utf8_lossy(&result.stderr)
    );
    // Exact line also excludes cleanup, grant closure, join completion and causes.
    assert_eq!(result.stderr, diagnostic, "{child}");
}
#[test]
fn fail15_full_counters_stop_without_cleanup_cause_or_join_delivery() {
    for (child, message) in [
        (
            "channel::rule_stop_tests::rc_overflow_child",
            b"run stop: CountOverflow\n".as_slice(),
        ),
        (
            "channel::rule_stop_tests::arc_overflow_child",
            b"run stop: CountOverflow\n",
        ),
        (
            "channel::rule_stop_tests::sender_overflow_child",
            b"run stop: ChannelSenders\n",
        ),
    ] {
        expect_stop(child, message);
    }
}
