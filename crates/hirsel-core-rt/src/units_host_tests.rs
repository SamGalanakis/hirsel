use super::*;
use crate::units::{
    Job, here,
    launch::launch_planned,
    now, park,
    plan::{HostEnvelope, ReactorChoice, UntiedPlan},
    running, spawn_job, wake,
};
use std::{
    ffi::CString,
    fs::{File, OpenOptions},
    os::unix::ffi::OsStrExt,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

const CHILD: &str = "HIRSEL_TEST_HOST_POOL_CASE";

#[test]
fn host_pool_bounded_and_system_lane_progress() {
    let run = |case| {
        std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "units::host_calls::tests::run_host_pool_case",
                "--ignored",
                "--nocapture",
            ])
            .env(CHILD, case)
            .env("HIRSEL_XSTATS", "1")
            .output()
            .unwrap()
    };
    let pool = run("pool");
    assert!(
        pool.status.success() && String::from_utf8_lossy(&pool.stdout).contains("1 passed"),
        "{pool:?}"
    );
    #[cfg(feature = "record")]
    {
        let replay = run("verdicts");
        assert_eq!(replay.status.code(), Some(1), "{replay:?}");
        let stderr = String::from_utf8_lossy(&replay.stderr);
        assert_eq!(
            stderr
                .lines()
                .filter(|line| line.starts_with("diverged root."))
                .count(),
            64
        );
    }
}

#[test]
#[ignore = "invoked by the host-pool law in a fresh process"]
fn run_host_pool_case() {
    let case = std::env::var(CHILD).unwrap();
    let mut plan = UntiedPlan::derive(
        1,
        1,
        64 << 20,
        HostEnvelope::UNLIMITED,
        ReactorChoice::Readiness,
    )
    .unwrap();
    plan.max_host_threads = 6;
    plan.reserved_system_threads = 2;
    plan.host_thread_capacity = 16;
    let pollers = hirsel_core_net::slot_readiness_pollers(1, here).unwrap();
    // SAFETY: the fixture uses its own Rust bodies and filesystem resources.
    let config: crate::entry::OsConfig = unsafe { std::mem::zeroed() };
    let action = |_: &crate::entry::OsConfig| {
        #[cfg(feature = "record")]
        if case == "verdicts" {
            replay_verdicts();
        }
        assert_eq!(case, "pool");
        exercise_pool();
        0
    };
    assert_eq!(
        launch_planned(plan, pollers, &config, &action, 0).unwrap(),
        0
    );
}

fn spawn(job: impl FnOnce() + Send + 'static) {
    assert!(spawn_job(Box::new(job) as Job).is_ok());
}

fn until(mut ready: impl FnMut() -> bool) {
    while !ready() {
        park(Some(now() + 1_000_000));
    }
}

fn exercise_pool() {
    let owner = running().unwrap();
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let started = Arc::new(AtomicUsize::new(0));
    for _ in 0..4 {
        let (started, gate) = (started.clone(), gate.clone());
        spawn(move || {
            off_slot(HostCall::Bounded, move || {
                started.fetch_add(1, Ordering::Release);
                wake(owner);
                drop(
                    gate.1
                        .wait_while(gate.0.lock().unwrap(), |open| !*open)
                        .unwrap(),
                );
            })
        });
    }
    until(|| started.load(Ordering::Acquire) == 4);
    let drained = Arc::new(AtomicUsize::new(0));
    for _ in 0..20 {
        let drained = drained.clone();
        spawn(move || {
            off_slot(HostCall::Bounded, move || {
                drained.fetch_add(1, Ordering::Release);
            })
        });
    }
    until(|| pool().helpers.state.lock().unwrap().lanes[0].jobs.len() == 20);
    assert_eq!(off_slot(HostCall::System, || 42), 42);
    assert_eq!(drained.load(Ordering::Acquire), 0);
    assert_eq!(pool().helpers.state.lock().unwrap().lanes[0].live, 4);
    assert!(pool().helpers.report().0 <= 6);

    let directory = tempfile::tempdir().unwrap();
    let fifo = directory.path().join("peer");
    let path = CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // SAFETY: the path is live, terminated, and inside the fixture directory.
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    let readers = Arc::new(AtomicUsize::new(0));
    for _ in 0..4 {
        let (readers, fifo) = (readers.clone(), fifo.clone());
        spawn(move || {
            let _reader = off_slot(HostCall::PeerWait, move || File::open(fifo).unwrap());
            readers.fetch_add(1, Ordering::Release);
        });
    }
    until(|| pool().helpers.state.lock().unwrap().peers == 4);
    let writer = off_slot(HostCall::PeerWait, || {
        OpenOptions::new().write(true).open(&fifo).unwrap()
    });
    until(|| readers.load(Ordering::Acquire) == 4);
    drop(writer);
    assert_eq!(drained.load(Ordering::Acquire), 0);

    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
    until(|| drained.load(Ordering::Acquire) == 20);
    let state = pool().helpers.state.lock().unwrap();
    assert_eq!(state.queued(), 0);
    assert_eq!(state.peers, 0);
    assert!(
        (9..=10).contains(&state.peak_live),
        "record every live peer"
    );
    assert!(state.peak_queued >= 20);
}

#[cfg(feature = "record")]
fn replay_verdicts() -> ! {
    use crate::tape::units::verdicts;
    for i in 0..64 {
        verdicts::started();
        spawn(move || {
            let unit = format!("root.{i}");
            verdicts::ended(&unit, Some(format!("diverged {unit}")));
            verdicts::park();
        });
    }
    assert!(verdicts::conclude(2, None));
    assert_eq!(pool().helpers.report().0, 0);
    std::process::exit(1)
}
