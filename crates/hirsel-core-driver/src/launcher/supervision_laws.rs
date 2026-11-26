//! Laws for pipelined exec acknowledgement (perf #7) and indexed child/group state (perf #9).
use super::session::tests::serve_fixture as serve;
use super::session::{Child, Event, Group, Launch, Mode, Phase, Refusal, Request, Role};
use super::{Image, ImageFile, Images, Launcher, Lifecycle};
use std::collections::BTreeSet;
use std::fs;
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

struct TestImages {
    paths: Vec<PathBuf>,
}

impl Images for TestImages {
    fn resolve(&self, assembly: [u8; 32]) -> Result<Image, String> {
        let path = self
            .paths
            .get(assembly[0] as usize)
            .ok_or("no such image")?
            .clone();
        Ok(Image {
            path,
            executable: ImageFile::Script,
            digest: [assembly[0]; 32],
            n_place: 1 << 16,
            prefix: Vec::new(),
        })
    }

    fn collect(
        &self,
        _: &BTreeSet<[u8; 32]>,
        _: &dyn Fn(crate::code_admission::ObjectKey) -> bool,
    ) -> std::io::Result<u64> {
        Ok(0)
    }
}

fn script(dir: &Path, name: &str, body: &str, executable: bool) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    let mode = if executable { 0o755 } else { 0o644 };
    fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
    path
}

fn grp(name: &str) -> Group {
    Group {
        name: name.into(),
        incarnation: 1,
    }
}

fn start_req(group: Group, image: u8, workdir: &Path, mode: Mode) -> Request {
    let mut assembly = vec![0; 32];
    assembly[0] = image;
    Request::Start(Launch {
        group,
        assembly,
        workdir: workdir.to_str().unwrap().into(),
        args: vec![],
        mode,
        recording: None,
    })
}

fn serving_child(group: Group, generation: u64) -> Child {
    Child {
        group,
        role: Role::Serving(generation),
    }
}

/// Pipelined exec acknowledgement (perf #7): a slow-exec child does not block an
/// unrelated fast start from publishing `Launched(_, Phase::Started)` first.
#[test]
fn slow_exec_does_not_block_unrelated_start() {
    let dir = tempfile::tempdir().unwrap();
    let work = dir.path();
    let state = work.join("state");
    fs::create_dir_all(&state).unwrap();
    let slow = script(work, "slow-delay-120ms", "exec sleep 30", true);
    let fast = script(work, "fast", "exec sleep 30", true);
    let mut launcher = Launcher::new(&state, Lifecycle::Leader, || {
        Ok(TestImages {
            paths: vec![slow, fast],
        })
    })
    .unwrap();
    let g_slow = grp("slow");
    let g_fast = grp("fast");
    launcher.handle(Request::Bind(g_slow.clone(), "127.0.0.1:0".into()));
    launcher.handle(Request::Bind(g_fast.clone(), "127.0.0.1:0".into()));
    launcher.take_events();

    let slow_child = serving_child(g_slow.clone(), 1);
    let fast_child = serving_child(g_fast.clone(), 1);
    launcher.handle(start_req(g_slow, 0, work, Mode::Serving(1, serve(1, None))));
    launcher.handle(start_req(g_fast, 1, work, Mode::Serving(1, serve(1, None))));

    let mut launched_order = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    while launched_order.len() < 2 {
        assert!(Instant::now() < deadline, "order={launched_order:?}");
        launcher.turn(Duration::from_millis(5)).unwrap();
        for ev in launcher.take_events() {
            if let Event::Launched(child, Phase::Started) = ev {
                launched_order.push(child);
            }
        }
    }
    assert_eq!(
        launched_order,
        vec![fast_child, slow_child],
        "unrelated fast start must publish Started before the 120ms slow-exec child"
    );
}

/// Pipelined exec error path and store-holder claim release:
/// 1. A non-executable image fails on the exec pipe with `Refusal::Spawn(...)`.
/// 2. While it is pending exec, a second conflicting store-holder is refused `Held`.
/// 3. Once exec fails, the store-holder claim is released, the listener watch re-arms
///    for cold arrivals, and a subsequent valid start succeeds.
#[test]
fn exec_failure_releases_store_claim_and_rearms_listener() {
    let dir = tempfile::tempdir().unwrap();
    let work = dir.path();
    let state = work.join("state");
    fs::create_dir_all(&state).unwrap();
    // Non-executable file with delay so we can test claim contention while pending exec.
    let bad = script(work, "bad-delay-60ms", "exec sleep 30", false);
    let good = script(work, "good", "exec sleep 30", true);
    let mut launcher = Launcher::new(&state, Lifecycle::Leader, || {
        Ok(TestImages {
            paths: vec![bad, good],
        })
    })
    .unwrap();
    let g = grp("g");
    launcher.handle(Request::Bind(g.clone(), "127.0.0.1:0".into()));
    let addr = launcher.listener_addr(&g).unwrap();
    launcher.take_events();

    let c1 = serving_child(g.clone(), 1);
    let c2 = serving_child(g.clone(), 2);
    let c3 = serving_child(g.clone(), 3);
    launcher.handle(start_req(
        g.clone(),
        0,
        work,
        Mode::Serving(1, serve(1, None)),
    ));
    // While c1 is still pending exec, its store-holder claim blocks c2.
    launcher.handle(start_req(
        g.clone(),
        1,
        work,
        Mode::Serving(2, serve(2, None)),
    ));
    assert_eq!(
        launcher.take_events(),
        vec![Event::Refused(c2, Refusal::Held)]
    );

    // Wait for c1's exec pipe failure.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut refused = None;
    while refused.is_none() {
        assert!(Instant::now() < deadline);
        launcher.turn(Duration::from_millis(5)).unwrap();
        for ev in launcher.take_events() {
            if let Event::Refused(c, Refusal::Spawn(msg)) = ev
                && c == c1
            {
                refused = Some(msg);
            }
        }
    }
    let msg = refused.unwrap();
    assert!(
        msg.to_lowercase().contains("permission denied"),
        "expected EACCES error from exec pipe, got: {msg}"
    );

    // Listener watch must be re-armed after exec failure.
    let _conn = TcpStream::connect(addr).unwrap();
    let mut arrived = false;
    while !arrived {
        assert!(Instant::now() < deadline);
        launcher.turn(Duration::from_millis(5)).unwrap();
        arrived = launcher.take_events().contains(&Event::Arrived(g.clone()));
    }

    // Store-holder claim was released, so c3 starts cleanly.
    launcher.handle(start_req(
        g.clone(),
        1,
        work,
        Mode::Serving(3, serve(3, None)),
    ));
    let mut started = false;
    while !started {
        assert!(Instant::now() < deadline);
        launcher.turn(Duration::from_millis(5)).unwrap();
        started = launcher
            .take_events()
            .contains(&Event::Launched(c3.clone(), Phase::Started));
    }
}

/// Never a double `Started`: duplicate `Start` while a child is pending exec coalesces
/// into one `Launched(_, Phase::Started)` once exec completes, and `Hello` while pending
/// does not duplicate it in `Inventory`.
#[test]
fn duplicate_start_and_hello_while_pending_never_double_started() {
    let dir = tempfile::tempdir().unwrap();
    let work = dir.path();
    let state = work.join("state");
    fs::create_dir_all(&state).unwrap();
    let slow = script(work, "slow-delay-80ms", "exec sleep 30", true);
    let mut launcher = Launcher::new(&state, Lifecycle::Leader, || {
        Ok(TestImages { paths: vec![slow] })
    })
    .unwrap();
    let g = grp("g");
    launcher.handle(Request::Bind(g.clone(), "127.0.0.1:0".into()));
    launcher.take_events();

    let req = start_req(g.clone(), 0, work, Mode::Serving(1, serve(1, None)));
    let c1 = serving_child(g.clone(), 1);
    launcher.handle(req.clone());
    // Duplicate Start while pending exec must coalesce.
    launcher.handle(req);
    assert!(launcher.take_events().is_empty());

    // Hello while pending exec must NOT list the un-acked child in Inventory.
    launcher.handle(Request::Hello);
    let inv = launcher.take_events();
    assert_eq!(inv, vec![Event::Inventory(vec![], vec![], vec![])]);

    // Wait past the 80ms delay and verify exactly ONE Launched(_, Phase::Started).
    let end = Instant::now() + Duration::from_millis(250);
    let mut launched_count = 0usize;
    while Instant::now() < end {
        launcher.turn(Duration::from_millis(10)).unwrap();
        for ev in launcher.take_events() {
            if ev == Event::Launched(c1.clone(), Phase::Started) {
                launched_count += 1;
            }
        }
    }
    assert_eq!(launched_count, 1, "expected exactly one Started event");
}

/// Bounded concurrent starts: starting more than `MAX_CONCURRENT_STARTS` children queues
/// the overflow and drains every start to `Phase::Started` without dropping or duplicating.
#[test]
fn bounded_concurrent_starts_drain_overflow_queue() {
    let dir = tempfile::tempdir().unwrap();
    let work = dir.path();
    let state = work.join("state");
    fs::create_dir_all(&state).unwrap();
    let ok = script(work, "ok", "exit 0", true);
    let mut launcher = Launcher::new(&state, Lifecycle::Leader, || {
        Ok(TestImages { paths: vec![ok] })
    })
    .unwrap();
    let total = super::supervision::MAX_CONCURRENT_STARTS + 16;
    for i in 0..total {
        let g = grp(&format!("g-{i}"));
        launcher.handle(Request::Bind(g, "127.0.0.1:0".into()));
    }
    launcher.take_events();

    for i in 0..total {
        let g = grp(&format!("g-{i}"));
        launcher.handle(start_req(
            g,
            0,
            work,
            Mode::Describe(super::session::Attempt {
                hi: i as u64,
                lo: 1,
            }),
        ));
    }
    let mut started = BTreeSet::new();
    let mut ended = BTreeSet::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    while started.len() < total || ended.len() < total {
        assert!(
            Instant::now() < deadline,
            "started={} ended={}",
            started.len(),
            ended.len()
        );
        launcher.turn(Duration::from_millis(5)).unwrap();
        for ev in launcher.take_events() {
            match ev {
                Event::Launched(c, Phase::Started) => {
                    assert!(started.insert(c), "duplicate Started");
                }
                Event::Answered(c, _) => {
                    assert!(started.contains(&c), "Answered before Started for {c:?}");
                }
                Event::Ended(c, _) => {
                    assert!(started.contains(&c), "Ended before Started for {c:?}");
                    assert!(ended.insert(c), "duplicate Ended");
                }
                other => panic!("unexpected event: {other:?}"),
            }
        }
    }
}
