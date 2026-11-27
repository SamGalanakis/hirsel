use super::*;

#[test]
fn plan_derivation_and_envelope_refusal() {
    let unbounded = HostEnvelope::UNLIMITED;
    let derive = |host, choice| UntiedPlan::derive(1, 4, 64 * MIB as u64, host, choice);
    let plan = derive(unbounded, ReactorChoice::IoUring).unwrap();
    assert_eq!(plan.max_units, 0xff_ffff);
    assert_eq!(plan.logical_units_hint, 233_017);
    assert_eq!(plan.fixed_threads, 17);
    assert_eq!(plan.max_host_threads, 32);
    assert_eq!(plan.pinned_ring_bytes, 96 * KIB);
    assert_eq!(plan.retained_bytes, 8 * MIB);
    assert_eq!(plan.reclaim_bytes, 2 * MIB);

    let constrained = HostEnvelope {
        page_bytes: 16 * KIB,
        address_space_limit: Some(32 * MIB as u64),
        thread_limit: Some(24),
        memlock_limit: Some(768 * KIB as u64),
        ..unbounded
    };
    let plan = derive(constrained, ReactorChoice::IoUring).unwrap();
    assert_eq!(plan.fixed_threads, 13);
    assert!(plan.fixed_threads + 4 + plan.max_host_threads <= 24);
    assert_eq!(plan.reactor.iowq_bounded, 1);
    assert_eq!(plan.reactor.iowq_unbounded, 1);
    assert_eq!(plan.pinned_ring_bytes, 384 * KIB);
    assert_eq!(plan.task_stack_reserve, MIB);
    for bytes in [
        plan.worker_stack_bytes,
        plan.host_stack_bytes,
        plan.memory_stack_bytes,
        plan.unit_stack_bytes,
        plan.task_stack_reserve,
    ] {
        assert_eq!(bytes % (16 * KIB), 0);
    }

    let no_ring = HostEnvelope {
        memlock_limit: Some(32 * KIB as u64),
        ..unbounded
    };
    let fallback = derive(no_ring, ReactorChoice::Auto).unwrap();
    assert!(fallback.reactor.force_epoll);
    assert_eq!(fallback.fixed_threads, 5);
    assert_eq!(fallback.pinned_ring_bytes, 0);
    assert!(
        derive(no_ring, ReactorChoice::IoUring)
            .unwrap_err()
            .contains("RLIMIT_MEMLOCK")
    );

    for (host, reason) in [
        (
            HostEnvelope {
                thread_limit: Some(9),
                ..unbounded
            },
            "pids.max/RLIMIT_NPROC",
        ),
        (
            HostEnvelope {
                memory_limit: Some(128 * KIB as u64),
                ..unbounded
            },
            "memory.max/RLIMIT_DATA",
        ),
        (
            HostEnvelope {
                address_space_limit: Some(MIB as u64),
                ..unbounded
            },
            "RLIMIT_AS",
        ),
    ] {
        assert!(
            derive(host, ReactorChoice::Auto)
                .unwrap_err()
                .contains(reason)
        );
    }

    let outcome = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "units::plan::tests::refuse_tight_nproc",
            "--ignored",
        ])
        .env("HIRSEL_UNTIED", "16")
        .env_remove(REACTOR_ENV)
        .output()
        .unwrap();
    assert_eq!(
        outcome.status.code(),
        Some(i32::from(crate::process::NOT_STARTED)),
        "{outcome:?}"
    );
    assert!(String::from_utf8_lossy(&outcome.stderr).contains("pids.max/RLIMIT_NPROC"));
}

#[test]
#[ignore = "runs with a private process limit under plan_derivation_and_envelope_refusal"]
fn refuse_tight_nproc() {
    // SAFETY: setrlimit reads the initialized pair. This is a subprocess.
    let limit = libc::rlimit {
        rlim_cur: 16,
        rlim_max: 16,
    };
    assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NPROC, &limit) }, 0);
    // SAFETY: all pointers and operations are absent. Startup must refuse
    // the thread budget before the callback can see any other inputs.
    let mut config: crate::entry::OsConfig = unsafe { std::mem::zeroed() };
    config.stderr_fd = 2;
    config.root_limit = 64 * MIB as u64;
    let workers = crate::units::workers().unwrap();
    std::process::exit(i32::from(crate::units::launch(
        1,
        workers,
        &config,
        &|_| panic!("a root ran under an impossible startup envelope"),
    )));
}
