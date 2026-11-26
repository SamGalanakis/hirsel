use super::super::plan::{HostEnvelope, ReactorChoice};
use super::*;
use std::sync::atomic::{AtomicU32, AtomicUsize};

const CAPACITY: &str = "HIRSEL_TEST_UNIT_DIRECTORY_CAPACITY";

#[test]
fn unit_ids_past_32k_and_directory_bounded() {
    for capacity in [64, 33_001] {
        let outcome = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "units::directory::tests::run_directory_case",
                "--ignored",
                "--nocapture",
            ])
            .env(CAPACITY, capacity.to_string())
            .output()
            .unwrap();
        assert!(
            outcome.status.success()
                && String::from_utf8_lossy(&outcome.stdout).contains("1 passed"),
            "{outcome:?}"
        );
    }
}

#[test]
#[ignore = "invoked in fresh pools by the directory law"]
fn run_directory_case() {
    let capacity: u32 = std::env::var(CAPACITY).unwrap().parse().unwrap();
    let mut host = HostEnvelope::UNLIMITED;
    if capacity == 64 {
        // Fit exactly 64 live units by the physical accounting formula.
        let page = host.page_bytes;
        let commit = 64 * 1024;
        let fixed = 3 * commit + page + 256 * size_of::<hirsel_core_net::ready::Event>();
        let unit = commit + page + size_of::<Unit>();
        host.memory_limit = Some((fixed + 64 * unit) as u64);
    }
    let mut plan = UntiedPlan::derive(1, 1, 288, host, ReactorChoice::Readiness).unwrap();
    if capacity > 64 {
        assert!(
            plan.max_units > 33_001,
            "logical charge must not cap replay units"
        );
        // Hold only the selected number of records in the high-water case.
        plan.max_units = capacity;
    }
    assert_eq!(plan.max_units, capacity);
    let pollers = hirsel_core_net::slot_readiness_pollers(1, here).unwrap();
    // SAFETY: the fixture supplies a Rust root and needs no capability.
    let config: crate::entry::OsConfig = unsafe { std::mem::zeroed() };
    let action = |_: &crate::entry::OsConfig| {
        let owner = running().unwrap();
        let remain = Arc::new(AtomicUsize::new(capacity as usize - 1));
        let highest = Arc::new(AtomicU32::new(0));
        let directory = &pool().units;
        assert!(
            directory.slot(1024).is_none(),
            "only the root's chunk is live"
        );
        for _ in 1..capacity {
            let remain = remain.clone();
            let highest = highest.clone();
            assert!(
                spawn_job(Box::new(move || {
                    highest.fetch_max(current(), Ordering::Relaxed);
                    if remain.fetch_sub(1, Ordering::AcqRel) == 1 {
                        wake(owner);
                    }
                }))
                .is_ok()
            );
        }
        assert!(matches!(
            spawn_job(Box::new(|| panic!("the refused helper ran"))),
            Err(SpawnErr::Exhausted { live, max_units, physical })
                if live == capacity as usize && max_units == capacity && physical == (capacity == 64)
        ));
        assert_eq!(LIVE.load(Ordering::SeqCst), capacity as usize);
        assert!(directory.slot(capacity).is_none());
        while remain.load(Ordering::Acquire) != 0 {
            park(None);
        }
        assert_eq!(highest.load(Ordering::Relaxed), capacity - 1);
        assert_eq!(LIVE.load(Ordering::SeqCst), 1);

        let reply = Reply::new();
        struct Pinned(*const Reply);
        // SAFETY: the root keeps the reply live across the spawn and wait.
        unsafe impl Send for Pinned {}
        let pinned = Pinned(&reply);
        assert!(
            spawn_job(Box::new(move || {
                let pinned = pinned;
                assert!(current() < capacity);
                // SAFETY: the root awaits this sole completion.
                unsafe { Reply::complete(pinned.0) }
            }))
            .is_ok()
        );
        reply.wait();
        0
    };
    assert_eq!(
        launch::launch_planned(plan, pollers, &config, &action, 0).unwrap(),
        0
    );
}
