//! Foreign-call oracles: float canonicalization, helper parking and handle release order.
use super::*;

#[test]
fn foreign_host_call_canonicalizes_nan_and_restores_floats() {
    use crate::foreign::{RESULT_F32, RESULT_F64, hirsel_core_foreign_stack, invoke};

    unsafe extern "C" fn poison(args: *mut u64) -> u64 {
        let here = 0u8;
        assert!(crate::fiber::room(1 << 20).is_ok());
        // SAFETY: the fixture lends one pinned argument word.
        unsafe { args.write(std::hint::black_box(&here as *const u8).addr() as u64) };
        #[cfg(target_arch = "x86_64")]
        {
            let mxcsr: u32 = 0xdfc0;
            let x87: u16 = 0x0f7f;
            // SAFETY: both words are readable and use supported control bits.
            unsafe {
                core::arch::asm!(
                    "ldmxcsr [{mxcsr}]",
                    "fldcw [{x87}]",
                    mxcsr = in(reg) &mxcsr,
                    x87 = in(reg) &x87,
                    options(nostack, preserves_flags, readonly),
                );
            }
        }
        #[cfg(target_arch = "aarch64")]
        // SAFETY: FPCR is writable in userspace. These bits change rounding and flushing.
        unsafe {
            core::arch::asm!("msr fpcr, {fpcr}", fpcr = in(reg) 0x0180_0000u64);
        }
        0xfff0_789a_ff80_1234
    }

    unsafe extern "C" fn main(
        _: *mut crate::Context,
        _: *const Config,
        result: *mut MainResult,
    ) -> u8 {
        let mut word = 0;
        // SAFETY: the fixture pins its argument and result. The thunk is bounded
        // and returns without retaining the argument or switching control.
        unsafe {
            *result = MainResult::default();
            assert_eq!(
                hirsel_core_foreign_stack(poison, &mut word, RESULT_F64),
                0x7ff8_0000_0000_0000
            );
            assert!((&raw const word).addr().abs_diff(word as usize) >= 1 << 20);
        }
        0
    }

    let mut stderr = vec![];
    let config = config(std::ptr::null(), 0, &mut stderr);
    // SAFETY: the fixture pins its configuration through the root task.
    assert_eq!(unsafe { hirsel_core_run(&config, main) }, 0);
    assert!(stderr.is_empty());

    // Drive the helper-side call form on a native thread. Reading its control
    // words after return detects restoration even without a stack switch.
    let mut word = 0;
    // SAFETY: this test thread has a native stack and lends the thunk one word.
    assert_eq!(
        unsafe { invoke(poison, &mut word, RESULT_F32) },
        0x7fc0_0000
    );
    #[cfg(target_arch = "x86_64")]
    {
        let mut mxcsr = 0u32;
        let mut x87 = 0u16;
        // SAFETY: both destinations are writable and fit their control word.
        unsafe {
            core::arch::asm!(
                "stmxcsr [{mxcsr}]",
                "fnstcw [{x87}]",
                mxcsr = in(reg) &raw mut mxcsr,
                x87 = in(reg) &raw mut x87,
                options(nostack, preserves_flags),
            );
        }
        assert_eq!((mxcsr, x87), (0x1f80, 0x037f));
    }
    #[cfg(target_arch = "aarch64")]
    {
        let fpcr: u64;
        // SAFETY: FPCR is readable in userspace.
        unsafe { core::arch::asm!("mrs {fpcr}, fpcr", fpcr = out(reg) fpcr) };
        assert_eq!(fpcr, 0);
    }
}

#[test]
fn foreign_helper_parks_the_issuer_and_delivers_its_writes() {
    use crate::{foreign::hirsel_core_foreign_wait, tasks::*};
    use std::sync::{Condvar, Mutex};

    type Gate = (Mutex<bool>, Condvar);

    unsafe extern "C" fn open(_: *mut crate::Context, _: *mut u8, input: *mut u8) -> u8 {
        // SAFETY: the input holds the parent-owned gate pointer. Its only
        // mutation is under the mutex, and the parent joins before dropping it.
        let gate = unsafe { &**input.cast::<*const Gate>() };
        *gate.0.lock().unwrap() = true;
        gate.1.notify_one();
        0
    }

    unsafe extern "C" fn mark(args: *mut u64) -> u64 {
        // SAFETY: the call lends three initialized argument words. The
        // borrowed gate remains pinned and only its mutex-protected bit changes.
        unsafe {
            assert_ne!(args.read(), libc::pthread_self() as u64);
            let gate = &*std::ptr::with_exposed_provenance::<Gate>(args.add(1).read() as usize);
            drop(
                gate.1
                    .wait_while(gate.0.lock().unwrap(), |open| !*open)
                    .unwrap(),
            );
            args.add(2).write(0xabab_abab);
        }
        4
    }

    unsafe extern "C" fn main(
        context: *mut crate::Context,
        _: *const Config,
        result: *mut MainResult,
    ) -> u8 {
        let gate = Gate::default();
        let mut gate_pointer = &raw const gate;
        let mut count = 0;
        let mut unit = 0u8;
        let input = ValueLayout {
            bytes: 8,
            align: 8,
            logical: 8,
            clean: None,
        };
        let output = ValueLayout {
            bytes: 0,
            align: 1,
            logical: 0,
            clean: None,
        };
        // SAFETY: both borrowed slots and the gate outlive the helper and the
        // child. The task exclusively owns its result slot and child handle.
        unsafe {
            *result = MainResult::default();
            let child = hirsel_core_spawn(
                context,
                0,
                0,
                (&raw mut gate_pointer).cast(),
                open,
                TaskLayout {
                    input,
                    result: output,
                },
                1,
            );
            assert!(!child.task.is_null());
            let mut words = [
                libc::pthread_self() as u64,
                gate_pointer.expose_provenance() as u64,
                0,
            ];
            assert_eq!(
                hirsel_core_foreign_wait(context, mark, words.as_mut_ptr(), 0, &mut count, 2),
                0
            );
            assert_eq!((count, words[2]), (4, 0xabab_abab));
            let joined = hirsel_core_join(context, child.task, &mut unit, 3);
            assert_eq!((joined.kind, joined.status), (0, 0));
        }
        0
    }

    let mut stderr = Vec::new();
    let config = config(std::ptr::null(), 0, &mut stderr);
    // SAFETY: config and stderr stay live through the run and helper shutdown.
    assert_eq!(
        unsafe { hirsel_core_run_with_foreign(&config, main, true) },
        0
    );
    assert!(stderr.is_empty());
}

#[test]
fn foreign_handles_retire_ordinals_and_release_remaining_owners_in_reverse_order() {
    use crate::foreign::{Call, Kind, hirsel_core_foreign_call};
    use std::ptr;
    use std::sync::atomic::{AtomicU64, Ordering};

    static CELLS: [u8; 3] = [1, 2, 3];
    static RELEASES: AtomicU64 = AtomicU64::new(0);
    static KIND: Kind = Kind {
        release,
        waits: true,
        contained: ptr::null(),
    };

    unsafe extern "C" fn make(args: *mut u64) -> u64 {
        // SAFETY: this thunk's one scalar carries a fixture-owned cell address.
        unsafe { args.read() }
    }

    unsafe extern "C" fn release(args: *mut u64) -> u64 {
        // SAFETY: runtime preflight resolves the ordinal to a static fixture cell.
        let value = unsafe { *ptr::with_exposed_provenance::<u8>(args.add(1).read() as usize) };
        let previous = RELEASES.load(Ordering::Relaxed);
        RELEASES.store(previous * 10 + u64::from(value), Ordering::Relaxed);
        9
    }

    unsafe extern "C" fn main(
        context: *mut crate::Context,
        _: *const Config,
        result: *mut MainResult,
    ) -> u8 {
        let scalar = [crate::foreign::Parameter {
            kind: ptr::null(),
            dir: false,
        }];
        let handle = [crate::foreign::Parameter {
            kind: &raw const KIND,
            dir: false,
        }];
        let maker = Call {
            thunk: make,
            parameters: scalar.as_ptr(),
            count: 1,
            new_kind: &KIND,
            waits: false,
            release: false,
            result_tag: 0,
        };
        let closer = Call {
            thunk: release,
            parameters: handle.as_ptr(),
            count: 1,
            new_kind: ptr::null(),
            waits: true,
            release: true,
            result_tag: 0,
        };
        let mut word = 0;
        let cell_address = |i: usize| (&raw const CELLS[i]).expose_provenance() as u64;
        // SAFETY: the fixture pins every descriptor, argument, and result
        // for its call, and the kind and the cells outlive run-end release.
        unsafe {
            *result = MainResult::default();
            for i in 0..3 {
                let mut argument = [cell_address(i), 0];
                assert_eq!(
                    hirsel_core_foreign_call(context, &maker, &mut argument, &mut word, 16),
                    0
                );
                assert_eq!(word, (i + 1) as u64);
            }
            let mut argument = [2, 0];
            assert_eq!(
                hirsel_core_foreign_call(context, &closer, &mut argument, &mut word, 16),
                0
            );
            assert_eq!(word, 9);
            let mut argument = [cell_address(1), 0];
            assert_eq!(
                hirsel_core_foreign_call(context, &maker, &mut argument, &mut word, 16),
                0
            );
            assert_eq!(word, 4);
            let mut alias = [cell_address(2), 0];
            hirsel_core_foreign_call(context, &maker, &mut alias, &mut word, 17)
        }
    }

    RELEASES.store(0, Ordering::Relaxed);
    let mut stderr = Vec::new();
    let config = config(ptr::null(), 0, &mut stderr);
    // SAFETY: config, diagnostics and the fixture's cells outlive the whole run.
    assert_eq!(
        unsafe { hirsel_core_run_with_foreign(&config, main, true) },
        101
    );
    assert_eq!(stderr, b"Fail(\"foreign pointer already held\") at 17\n");
    assert_eq!(RELEASES.load(Ordering::Relaxed), 2231);
}
