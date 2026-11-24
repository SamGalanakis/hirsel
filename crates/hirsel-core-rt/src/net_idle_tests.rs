use crate::{Context, net::*, net_tests::carrier};
use std::{ptr, time::Duration};

/// An idle wait releases a waiting task's stack only once it has run
/// `RELEASE_DELAY_MS` with nothing to run. A wake that brings no work, between
/// a busy reader's reads, leaves the stack resident; a quiet spell releases it.
#[test]
fn short_idle_wakes_keep_a_busy_reader_stack() {
    crate::net_tests::isolated("short_wake_keeps_stack");
}
const SHORT_WAKE_ROUNDS: usize = 40;
const SHORT_WAKE_WARMUP: usize = 4;
const DEEP_BYTES: usize = 32 << 10;

struct ShortWakeScenario {
    key: u64,
    /// Minor faults touching the deep stack after each read.
    faults: Vec<i64>,
}

fn thread_minor_faults() -> i64 {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    // SAFETY: getrusage writes the whole rusage on success.
    assert_eq!(
        unsafe { libc::getrusage(libc::RUSAGE_THREAD, usage.as_mut_ptr()) },
        0
    );
    unsafe { usage.assume_init() }.ru_minflt
}

/// Writes pages of stack well below the reader's parked frame.
#[inline(never)]
fn touch_deep_stack() {
    let mut deep = [0u8; DEEP_BYTES];
    for offset in (0..DEEP_BYTES).step_by(4096) {
        // SAFETY: in bounds of the local array.
        unsafe { deep.as_mut_ptr().add(offset).write_volatile(1) };
    }
    std::hint::black_box(&mut deep);
}

unsafe extern "C" fn short_wake_reader(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
    // SAFETY: the root pins the scenario until it joins this reader.
    unsafe {
        let scenario = input.cast::<*mut ShortWakeScenario>().read();
        let mut received = 0;
        while received < SHORT_WAKE_ROUNDS + 1 {
            let mut bytes = [0u8; 8];
            let mut b = carrier(&mut bytes, 0);
            let out = hirsel_core_host_recv_bounded(
                context,
                (*scenario).key,
                &mut b,
                u64::MAX,
                0,
                0,
                0,
                1,
            );
            assert_eq!(out.error.tag, 0);
            assert!(b.len > 0);
            received += b.len;
            let before = thread_minor_faults();
            touch_deep_stack();
            (*scenario).faults.push(thread_minor_faults() - before);
        }
    }
    0
}

unsafe extern "C" fn short_wake_root(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
    use crate::tasks::*;
    // SAFETY: run_root pins the scenario through this body.
    unsafe {
        let mut child_input = input.cast::<*mut ShortWakeScenario>().read();
        let child = hirsel_core_spawn(
            context,
            0,
            8192,
            (&raw mut child_input).cast(),
            short_wake_reader,
            crate::tasks_tests::layout::<*mut ShortWakeScenario, ()>(0),
            1,
        );
        assert!(!child.task.is_null());
        let joined = hirsel_core_join(context, child.task, ptr::null_mut(), 2);
        assert_eq!((joined.kind, joined.status), (0, 0));
    }
    0
}

unsafe fn short_wake_law(context: *mut Context) {
    use std::{
        io::Write,
        net::{TcpListener, TcpStream},
    };
    unsafe {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let socket = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let mut peer =
            crate::loopback::accept_from_address(&listener, socket.local_addr().unwrap()).unwrap();
        socket.set_nonblocking(true).unwrap();
        peer.set_nodelay(true).unwrap();
        let key = (*(*context).host_files)
            .sockets
            .insert(socket.into())
            .unwrap();
        let halves = hirsel_core_net_split(context, key, 1);
        let mut scenario = ShortWakeScenario {
            key: halves.value,
            faults: Vec::with_capacity(2 * SHORT_WAKE_ROUNDS),
        };
        // Each read is followed by a wake that names no waiting operation, so
        // the scheduler wakes, finds nothing to run, and waits again.
        let writer = std::thread::spawn(move || {
            for _ in 0..SHORT_WAKE_ROUNDS {
                std::thread::sleep(Duration::from_millis(2));
                peer.write_all(&[1]).unwrap();
                std::thread::sleep(Duration::from_millis(1));
                crate::tasks::hirsel_core_ready(u64::MAX - 1);
            }
            // A quiet spell past the release delay: the stack goes back.
            std::thread::sleep(Duration::from_millis(
                3 * crate::task_heap::RELEASE_DELAY_MS as u64,
            ));
            peer.write_all(&[1]).unwrap();
            peer
        });
        let mut input = &raw mut scenario;
        crate::causes::task_start(&mut *context).unwrap();
        let failed = crate::tasks::run_root(
            context,
            short_wake_root,
            (&raw mut input).cast(),
            ptr::null_mut(),
            size_of::<*mut ShortWakeScenario>(),
            0,
            crate::tasks::SchedulerProvider::default(),
        );
        assert_eq!(failed, 0);
        drop(writer.join().unwrap());
        let (tail, busy) = scenario.faults.split_last().unwrap();
        let busy = &busy[SHORT_WAKE_WARMUP.min(busy.len())..];
        let busy_faults: i64 = busy.iter().sum();
        assert!(
            busy_faults < busy.len() as i64,
            "short wakes released a busy reader's stack: {:?}",
            scenario.faults
        );
        assert!(
            *tail >= 2,
            "a quiet spell kept a waiting reader's stack: {:?}",
            scenario.faults
        );
        hirsel_core_socket_drop(context, halves.value);
        hirsel_core_socket_drop(context, halves.written);
    }
}

/// The socket laws this module owns, run in `net_tests`' isolated child.
pub(crate) unsafe fn socket_law(law: &str, context: *mut Context) {
    match law {
        // SAFETY: the child pins the context and its files through the law.
        "short_wake_keeps_stack" => unsafe { short_wake_law(context) },
        _ => panic!("unknown socket law {law}"),
    }
}
