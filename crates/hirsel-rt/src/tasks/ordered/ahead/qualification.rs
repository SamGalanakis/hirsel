//! Saved-frame acceptance witnesses; atomic probes observe physical execution only.
use super::*;
use crate::tasks::*;
use std::sync::{Arc, atomic::AtomicU32};

fn scalar_environment() -> crate::Layout {
    crate::Layout {
        magic: crate::LAYOUT_MAGIC,
        version: crate::LAYOUT_VERSION,
        size: 16,
        align: 8,
        finalizer: None,
        steps: std::ptr::null(),
        step_count: 0,
        field_count: 0,
        flags: 0,
        logical: 0,
        logical_count: std::ptr::null(),
        scan: std::ptr::null(),
        scan_words: 0,
    }
}

unsafe extern "C" fn unused(_: *mut TaskCtx, _: *mut crate::Header, _: u64, _: u64) -> u8 {
    panic!("refused metadata must not enter either child body");
}

#[test]
fn unsupported_metadata_refuses_before_scope_or_environment_access() {
    for width in [1, 2, 4] {
        for (version, flags) in [
            (0, 0),
            (7, 0),
            (1, 1),
            (2, 1),
            (3, 1),
            (4, 1),
            (5, 1),
            (1, u32::MAX),
        ] {
            let mut run = crate::host::BodyRun::new(width, true);
            unsafe { run.use_physical_dispatch() };
            let control = run.control();
            let outcome = run.run(
                |ctx| unsafe {
                    super::super::engine::enable(
                        ctx,
                        crate::ordered::Policy::admit(&Default::default(), None),
                    );
                },
                move |ctx| unsafe {
                    let info = SpawnAheadInfo {
                        version,
                        flags,
                        entry: Some(unused),
                    };
                    hirsel_rt_v2_task_spawn_ahead(
                        ctx,
                        0,
                        0,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        std::ptr::null(),
                        unused,
                        std::ptr::null_mut(),
                        0,
                        &info,
                    )
                },
            );
            assert_eq!(
                outcome.ended,
                Err(crate::stop::Stop::Refused(
                    "unsupported spawn-ahead metadata version or flags".into()
                )),
            );

            while control.status().reclamation_pending != 0 {
                std::thread::yield_now();
            }
            assert!(control.status().finished);
        }
    }
}

#[test]
#[cfg(unix)]
fn version_dispatch_does_not_overread_legacy_or_unsupported_prefixes() {
    unsafe extern "C" fn legacy_child(
        _: *mut TaskCtx,
        _: *mut crate::Header,
        _: u64,
        _: u64,
    ) -> u8 {
        crate::STATUS_OK
    }
    unsafe {
        let page = libc::sysconf(libc::_SC_PAGESIZE) as usize;
        let mapping = libc::mmap(
            std::ptr::null_mut(),
            page * 2,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        );
        assert_ne!(mapping, libc::MAP_FAILED);
        assert_eq!(
            libc::mprotect(mapping.cast::<u8>().add(page).cast(), page, libc::PROT_NONE),
            0
        );
        for version in [1, 7] {
            // The next byte is inaccessible. v1 has exactly its original16
            // bytes; unsupported metadata has only its8-byte dispatch prefix.
            let bytes = if version == 1 { 16 } else { 8 };
            let info = mapping
                .cast::<u8>()
                .add(page - bytes)
                .cast::<SpawnAheadInfo>();
            std::ptr::addr_of_mut!((*info).version).write(version);
            std::ptr::addr_of_mut!((*info).flags).write(0);
            if version == 1 {
                std::ptr::addr_of_mut!((*info).entry).write(None);
            }
            // The mapping outlives run; pass its address through the Send closure.
            let info_address = info as usize;
            let layout = scalar_environment();
            let layouts = [&layout as *const crate::Layout];
            let mut run = crate::host::BodyRun::new(1, false);
            run.config.layouts = layouts.as_ptr();
            run.config.layout_count = 1;
            run.use_physical_dispatch();
            let outcome = run.run(
                |ctx| {
                    super::super::engine::enable(
                        ctx,
                        crate::ordered::Policy::admit(&Default::default(), None),
                    )
                },
                move |ctx| {
                    let info = info_address as *const SpawnAheadInfo;
                    let mut scope = 0;
                    if version == 1 {
                        assert_eq!(
                            hirsel_rt_v2_task_scope_enter(
                                ctx,
                                std::ptr::null_mut(),
                                &mut scope,
                                &0
                            ),
                            crate::STATUS_OK
                        );
                    }
                    let environment = if version == 1 {
                        // TaskSpawn has always required an owned environment.
                        // Keep metadata at the exact v1 page boundary.
                        crate::alloc::hirsel_rt_v2_alloc(
                            ctx,
                            2,
                            16,
                            8,
                            crate::alloc::DOMAIN_HEAP,
                            0,
                        )
                    } else {
                        std::ptr::null_mut()
                    };
                    let mut child = 0;
                    let status = hirsel_rt_v2_task_spawn_ahead(
                        ctx,
                        0,
                        0,
                        std::ptr::null_mut(),
                        &mut child,
                        &scope,
                        legacy_child,
                        environment,
                        0,
                        info,
                    );
                    assert_eq!(status, crate::STATUS_OK);
                    hirsel_rt_v2_task_scope_exit(ctx, 0, 0, &scope, 0)
                },
            );
            if version == 1 {
                assert_eq!(outcome.ended, Ok(crate::STATUS_OK));
            } else {
                assert_eq!(
                    outcome.ended,
                    Err(crate::stop::Stop::Refused(
                        "unsupported spawn-ahead metadata version or flags".into()
                    ))
                );
            }
        }
        assert_eq!(libc::munmap(mapping, page * 2), 0);
    }
}
