//! Round trips between pool units 0 and 1 over two connections, through the
//! entry points, on the untied pool (`HIRSEL_UNTIED=<workers>`). Unit 0 sends
//! a block of `size` bytes to unit 1, which receives it and sends the same
//! value back. A ready receiver polls its port; a parked one waits, so every
//! crossing wakes it.
use super::abi::*;
use super::conn::Client;
use std::{
    hint::spin_loop,
    mem::MaybeUninit,
    ptr::null_mut,
    sync::atomic::{AtomicPtr, Ordering},
    time::Instant,
};

static PEER: [AtomicPtr<Client>; 2] = [const { AtomicPtr::new(null_mut()) }; 2];

/// Run `warm` then `n` timed round trips; prints
/// `conn_<ready|parked> n=<n> ns=<total> check=<n>` to stderr from unit 0.
#[doc(hidden)]
pub fn conn_pingpong(parked: bool, warm: u64, n: u64, size: u64) -> u8 {
    assert!(
        std::env::var("HIRSEL_UNTIED").is_ok_and(|w| w.parse::<usize>().is_ok_and(|w| w >= 1)),
        "set HIRSEL_UNTIED to the worker count"
    );
    // SAFETY: every field is a length, a mask, a descriptor or a nullable
    // pointer; the bench's units read none of them.
    let config: crate::entry::OsConfig = unsafe { std::mem::zeroed() };
    super::super::launch(2, &config, |_| {
        // SAFETY: runs on pool units 0 and 1, one body each.
        unsafe { body(parked, warm, n, size) }
    })
}

/// # Safety
/// Called once on each of pool units 0 and 1.
unsafe fn body(parked: bool, warm: u64, n: u64, size: u64) -> u8 {
    // SAFETY: a running pool unit.
    let me = unsafe { (*super::super::cur()).id } as usize;
    let mut v = MaybeUninit::uninit();
    let (mut inbox, mut client) = (null_mut(), null_mut());
    // SAFETY: the entry points' contracts hold: `u` is this pool unit's,
    // the slots are local, the handles are live until the end.
    unsafe {
        let u = hirsel_unit_root(1 << 30);
        assert!(hirsel_unit_conn_new(u, 4, 8, 4 * size, &mut inbox, &mut client, null_mut()));
        PEER[me].store(client, Ordering::Release);
        let peer = loop {
            let p = PEER[1 - me].load(Ordering::Acquire);
            if !p.is_null() {
                break p;
            }
            super::super::maybe_yield();
        };
        let get = |v: &mut MaybeUninit<_>| {
            loop {
                let t = if parked {
                    hirsel_unit_wait(u, null_mut())
                } else {
                    hirsel_unit_take(u, null_mut())
                };
                match t {
                    TOOK_ITEM => break,
                    TOOK_NONE => {
                        spin_loop();
                        super::super::maybe_yield();
                    }
                    other => panic!("bench: unexpected arrival {other}"),
                }
            }
            assert_eq!(hirsel_unit_receive(u, inbox, v, null_mut()), RECV_OK);
        };
        let put = |v: &mut MaybeUninit<_>| {
            assert_eq!(hirsel_unit_send(u, peer, v, false, null_mut()), SENT_ACCEPTED);
        };
        let total = warm + n;
        if me == 0 {
            assert!(hirsel_unit_alloc(u, size, &mut v, null_mut()));
            let mut started = Instant::now();
            for i in 0..total {
                if i == warm {
                    started = Instant::now();
                }
                put(&mut v);
                get(&mut v);
            }
            let ns = started.elapsed().as_nanos();
            let mode = if parked { "parked" } else { "ready" };
            eprintln!("conn_{mode} n={n} ns={ns} check={n}");
            let mut cause = MaybeUninit::uninit();
            assert!(!hirsel_unit_drop(u, &mut v, &mut cause));
        } else {
            for _ in 0..total {
                get(&mut v);
                put(&mut v);
            }
        }
        // The connections, contexts and handles are left to process exit:
        // retiring them needs both units to seal, clean and take the last
        // release, which the measurement does not exercise.
    }
    0
}
