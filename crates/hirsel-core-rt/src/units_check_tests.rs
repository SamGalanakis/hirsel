use super::{CASES, run_one};
use std::process::Command;

/// Run `case` over seeds in child processes (one schedule each, on the
/// CPUs this process may use, up to two at a time by default). Returns the first failing
/// seed and its report, plus the schedule, completion and goal counts.
fn search(case: &str) -> (Option<(u64, String)>, u64, u64, u64) {
    let env = |k: &str, d: u64| {
        std::env::var(k)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(d)
    };
    let first = env("HIRSEL_CHECK_FROM", 0);
    let count = env("HIRSEL_CHECK_SEEDS", 2000);
    let jobs = env("HIRSEL_CHECK_JOBS", env("HIRSEL_JOBS", 2).min(2)) as usize;
    assert!(
        jobs > 0,
        "the scheduler campaign needs at least one child slot"
    );
    // HIRSEL_CHECK_ALL: run every seed and list each failing one (for
    // measuring how often an unaided search hits a case).
    let all = std::env::var_os("HIRSEL_CHECK_ALL").is_some();
    let mut fails = Vec::new();
    let mut inconclusive = 0u64;
    let mut completed = 0u64;
    let mut reached_goal = 0u64;
    let cpus = crate::host::sys::allowed_cpus();
    let case_index = CASES.iter().position(|entry| entry.name == case).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let exe = std::env::current_exe().unwrap();
    let test = format!("units::check::tests::{case}");
    let mut seed = first;
    let mut ran = 0;
    let mut running: Vec<(u64, std::process::Child, std::path::PathBuf)> = Vec::new();
    let mut found = None;
    let mut law = super::conn::Law::default();
    while (seed < first + count && (all || found.is_none())) || !running.is_empty() {
        while running.len() < jobs && seed < first + count && (all || found.is_none()) {
            let out = dir.path().join(format!("{seed}.txt"));
            let cpu = cpus[(case_index + (seed - first) as usize) % cpus.len()];
            let child = Command::new(&exe)
                .env("HIRSEL_CHECK_CPU", cpu.to_string())
                .args(["--exact", &test, "--nocapture", "--test-threads=1", "-q"])
                .env("HIRSEL_CHECK_CHILD", case)
                .env("HIRSEL_CHECK_SEED", seed.to_string())
                .env("HIRSEL_CHECK_OUT", &out)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap();
            running.push((seed, child, out));
            seed += 1;
        }
        let (s, mut child, out) = running.remove(0);
        let status = child.wait().unwrap();
        ran += 1;
        let text = std::fs::read_to_string(&out).unwrap_or_default();
        let _ = std::fs::remove_file(&out);
        match status.code() {
            Some(0) => {
                completed += 1;
                reached_goal += 1;
                if let Some(v) = law.add(s, &text) {
                    fails.push(s);
                    if found.as_ref().is_none_or(|(f, _): &(u64, String)| s < *f) {
                        found = Some((s, format!("FAIL determinism: {v}\n")));
                    }
                }
            }
            Some(2) => {
                inconclusive += 1;
                eprintln!("{case} seed {s}: inconclusive");
            }
            code => {
                completed += 1;
                fails.push(s);
                if all {
                    eprintln!(
                        "{case} seed {s}: fails (exit {code:?}): {}",
                        text.lines().next().unwrap_or("")
                    );
                }
                if found.as_ref().is_none_or(|(f, _): &(u64, String)| s < *f) {
                    found = Some((s, text));
                }
            }
        }
    }
    if all {
        fails.sort();
        eprintln!(
            "{case}: {} of {ran} schedules fail, {inconclusive} inconclusive; failing seeds {fails:?}",
            fails.len()
        );
    }
    (found, ran, completed, reached_goal)
}

fn check(case: &str) {
    if std::env::var("HIRSEL_CHECK_CHILD").as_deref() == Ok(case) {
        if let Some(cpu) = std::env::var("HIRSEL_CHECK_CPU")
            .ok()
            .and_then(|c| c.parse::<usize>().ok())
        {
            crate::host::sys::pin_to_cpu(cpu);
        }
        let seed = std::env::var("HIRSEL_CHECK_SEED").unwrap().parse().unwrap();
        let out = std::env::var("HIRSEL_CHECK_OUT").unwrap();
        run_one(CASES.iter().find(|c| c.name == case).unwrap(), seed, &out);
    }
    let t = std::time::Instant::now();
    let (found, ran, completed, reached_goal) = search(case);
    let secs = t.elapsed().as_secs_f64();
    if let Some((seed, report)) = found {
        panic!(
            "{case}: law violated at seed {seed} (schedule {ran} of the search, {secs:.1}s)\n{report}"
        );
    }
    assert!(
        completed > 0,
        "{case}: zero of {ran} schedules completed ({secs:.1}s)"
    );
    eprintln!(
        "{case}: {reached_goal} of {completed} completed schedules reached their goal across {ran} schedules ({secs:.1}s)"
    );
}

#[test]
fn stale_sleep_bound() {
    check("stale_sleep_bound");
}
#[test]
fn nobody_in_poller() {
    check("nobody_in_poller");
}
#[test]
fn busy_slot_deadline() {
    check("busy_slot_deadline");
}
#[test]
fn requeue_without_notification() {
    check("requeue_without_notification");
}
#[test]
fn outside_end_wakes_waiter() {
    check("outside_end_wakes_waiter");
}
#[test]
fn injector_starved() {
    check("injector_starved");
}
#[test]
fn connection_spin_yields_to_work() {
    check("connection_spin_yields_to_work");
}
#[test]
fn conn_wait_serve() {
    check("conn_wait_serve");
}
#[test]
fn conn_seal_in_flight() {
    check("conn_seal_in_flight");
}
#[test]
fn conn_helper_forward() {
    check("conn_helper_forward");
}
#[test]
fn conn_release_race() {
    check("conn_release_race");
}
#[test]
fn conn_slot_reuse() {
    check("conn_slot_reuse");
}
#[test]
fn returnable_stack_reuse() {
    check("returnable_stack_reuse");
}
#[test]
fn returnable_receive() {
    check("returnable_receive");
}
#[test]
fn returnable_transfer() {
    check("returnable_transfer");
}
#[test]
fn park_waits() {
    check("park_waits");
}

#[test]
fn conn_flat_slot_reuse() {
    check("conn_flat_slot_reuse");
}
