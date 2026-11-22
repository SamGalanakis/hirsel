//! CONC-15/17 through the emitters' entry points: a root task and a helper
//! unit, the helper on its own scheduler, exchanging items with clients.
//! A round trip is a request from the root and the helper's answer.
use super::*;
use crate::tasks::{Join, SchedulerProvider, ValueLayout, run_root};

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
    }
}
/// The helper's input: the root's client.
const INPUT_OWNERS: [[u64; 4]; 1] = [[0, CLIENT, 0, 0]];
fn input() -> Cross {
    Cross {
        bytes: 8,
        owners: INPUT_OWNERS.as_ptr(),
        count: 1,
    }
}
fn plain(bytes: u64) -> Cross {
    Cross {
        bytes,
        owners: ptr::null(),
        count: 0,
    }
}

unsafe fn send(c: *mut Context, to: *mut ClientH, v: u64, back: *mut ClientH) -> u8 {
    let mut x = Item { v, back };
    // SAFETY: a live client and an item of the connection's layout.
    let o = unsafe { hirsel_core_conn_send(c, to, (&raw mut x).cast(), 0, true) };
    assert_eq!(o.status, 0);
    o.outcome
}
unsafe fn receive(c: *mut Context, from: *mut InboxH) -> Option<Item> {
    let mut x = Item {
        v: 0,
        back: ptr::null_mut(),
    };
    // SAFETY: a live inbox and a writable item.
    let o = unsafe { hirsel_core_conn_receive(c, from, (&raw mut x).cast(), 0, true) };
    match o.code_and_requested {
        0 => Some(x),
        1 => None,
        n => panic!("receive outcome {n}"),
    }
}

/// The helper: answers each `v` with `v + 1` until its inbox closes, and
/// returns how many it answered.
unsafe extern "C" fn echo(c: *mut Context, out: *mut u8, input: *mut u8) -> u8 {
    // SAFETY: the input is the root's client; out is a u64.
    unsafe {
        let root = input.cast::<*mut ClientH>().read();
        let i = item();
        let made = hirsel_core_conn_new(c, 1, 64, &i, 16, 0);
        assert!(!made.inbox.is_null());
        assert_eq!(send(c, root, 0, made.client), 0);
        let mut n = 0u64;
        while let Some(x) = receive(c, made.inbox) {
            assert_eq!(send(c, root, x.v + 1, ptr::null_mut()), 0);
            n += 1;
        }
        hirsel_core_client_drop(c, root);
        hirsel_core_inbox_drop(c, made.inbox);
        out.cast::<u64>().write(n);
        0
    }
}

static ROUNDS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

unsafe extern "C" fn root(c: *mut Context, out: *mut u8, _: *mut u8) -> u8 {
    // SAFETY: a root task with a u64 [rounds, ns] destination.
    unsafe {
        let rounds = ROUNDS.load(std::sync::atomic::Ordering::Relaxed);
        let i = item();
        let made = hirsel_core_conn_new(c, 1, 64, &i, 16, 0);
        let before = (*c).available();
        let mut x = made.client;
        let layout = TaskLayout {
            input: ValueLayout {
                bytes: 8,
                align: 8,
                logical: 8,
                clean: None,
            },
            result: ValueLayout {
                bytes: 8,
                align: 8,
                logical: 8,
                clean: None,
            },
        };
        let (xi, r) = (input(), plain(8));
        let spawned =
            hirsel_core_spawn_unit(c, 0, 4096, (&raw mut x).cast(), echo, &layout, &xi, &r, 0);
        assert!(!spawned.task.is_null());
        // CONC-15: the task charge 256 + size(R) + 32 + n.
        assert_eq!(before - (*c).available(), 256 + 8 + 32 + 4096);
        let hello = receive(c, made.inbox).unwrap();
        assert_eq!(hello.v, 0);
        let helper = hello.back;
        let start = std::time::Instant::now();
        for k in 0..rounds {
            assert_eq!(send(c, helper, k, ptr::null_mut()), 0);
            assert_eq!(receive(c, made.inbox).unwrap().v, k + 1);
        }
        let ns = start.elapsed().as_nanos() as u64;
        hirsel_core_client_drop(c, helper);
        let mut answered = 0u64;
        let j: Join = tasks::hirsel_core_join(c, spawned.task, (&raw mut answered).cast(), 0);
        assert_eq!((j.kind, j.status), (0, 0));
        assert_eq!(answered, rounds);
        // The helper's client dropped at its end: the inbox reads Closed.
        assert!(receive(c, made.inbox).is_none());
        hirsel_core_inbox_drop(c, made.inbox);
        out.cast::<[u64; 2]>().write([answered, ns]);
        0
    }
}

/// Run `rounds` round trips; returns the helper's answer count and the
/// timed nanoseconds. Every byte the run admitted must be back at its end.
pub fn run(rounds: u64) -> [u64; 2] {
    ROUNDS.store(rounds, std::sync::atomic::Ordering::Relaxed);
    let mut context = Context::new(1 << 20);
    causes::task_start(&mut context).unwrap();
    let before = context.available();
    let mut out = [0u64; 2];
    let mut unit = 0u8;
    // SAFETY: the root's destination lives until the run ends.
    let failed = unsafe {
        run_root(
            &mut context,
            root,
            &mut unit,
            (&raw mut out).cast(),
            0,
            16,
            SchedulerProvider::default(),
        )
    };
    assert_eq!(failed, 0);
    // Every charge the run made came back: the helper's and both connections'.
    assert_eq!(context.available(), before);
    out
}

/// `units_lang <thread|pool> <rounds>`: prints `ns=<per round trip>`.
#[doc(hidden)]
pub fn pingpong(pool: bool, rounds: u64) -> u8 {
    let once = || {
        let [n, ns] = run(rounds);
        assert_eq!(n, rounds);
        eprintln!("units_lang n={n} ns={}", ns / n.max(1));
        0
    };
    if !pool {
        return once();
    }
    let w = crate::units::workers().expect("set HIRSEL_UNTIED to the worker count");
    // SAFETY: lengths, masks, descriptors and nullable pointers; the root
    // reads none of them.
    let config: crate::entry::OsConfig = unsafe { std::mem::zeroed() };
    crate::units::launch(1, w, &config, &|_| once())
}
