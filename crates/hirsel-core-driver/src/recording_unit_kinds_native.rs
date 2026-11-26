// Replay of items whose content lies past their flat bytes (docs/recording.md,
// "Replay of a run with helpers"). Included from recording_units_native.rs.

/// The root sends variant items whose cases hold a `String`, a `List[u64]`
/// or nothing to its own connection, and receives them in order.
const U5_SEQ_ITEMS: &str = r#"
type Msg: cross = Text(u64, String) | Nums(List[u64]) | Count(u64)

fn score(take m: Msg) -> u64 {
    match take m {
        Text(n, take s) => 1000,
        Nums(take l) => list.len(l) * 100,
        Count(k) => k
    }
}

fn text(take m: Msg) -> String {
    match take m {
        Text(n, take s) => s,
        Nums(take l) => string.from("nums"),
        Count(k) => string.from("count")
    }
}

pub fn main(out: Stdout) -> Result[(), Exit] waits {
    let (c, inbox): (Client[Msg], Inbox[Msg]) = result.unwrap(conn.new(3, bytes 256))
    let mut l: List[u64] = list.new()
    list.push(mut l, 1)
    list.push(mut l, 2)
    result.unwrap(conn.send(c, Count(7)))
    result.unwrap(conn.send(c, Text(5, string.from("text"))))
    result.unwrap(conn.send(c, Nums(take l)))
    let a = score(result.unwrap(conn.receive(inbox)))
    let s = text(result.unwrap(conn.receive(inbox)))
    let d = score(result.unwrap(conn.receive(inbox)))
    if a + d * 1000 != 200007 { return Error(Exit(status: 3)) }
    result.unwrap(stdout.write(out, s))
    Ok(())
}
"#;

/// An item's `String` and list replay from its `TAKE`, whichever case holds
/// them: the replayed item's sequence words point at the elements the take
/// carries, and only the active case's sequences are copied and charged, as
/// a send's are. Before, each sequence word of a replayed item was null and
/// every case's rows were copied, so the replay aborted on its first item.
/// The oracle is the program's literals: the `Count` and list cases' scores
/// must sum to 200007, and the `String` the root writes is the one it sent,
/// so a take whose text changes ends the replay at that write's `OP`.
#[test]
#[ignore = "explicit native integration target"]
fn u5_items_with_sequences_replay_by_case() {
    crate::openat2_required::require();

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let mut request = source_request(U5_SEQ_ITEMS, dir.path());
    request.inputs.packages[1] =
        PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
    let product = Driver::default()
        .check(request, &mut Meter::new(false))
        .unwrap();
    assert!(product.accepted(), "{:?}", product.diagnostics);
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    for backend in [Backend::Cranelift, Backend::Llvm] {
        let mut options = native_options(backend, dir.path());
        options.runtime = env_record_runtime();
        options.recording = true;
        options.grant_mask = 1 << grant_kind("Stdout").unwrap();
        options.cas = dir.path().join(format!("u5q-store-{backend:?}"));
        options.output = dir.path().join(format!("u5q-{backend:?}"));
        let program = product.build(&entry, &options).unwrap().executable;
        let path = dir.path().join(format!("u5q-{backend:?}.tape"));
        let (code, out, err) = tape_run(&program, &[], Some(&path));
        assert_eq!((code, out.as_str()), (Some(0), "text"), "{backend:?}: {err}");
        let frames = unit_streams(&path, &format!("{backend:?}"), &["root"]);
        let (code, out, err) = u4_replay(&program, &path, None);
        assert_eq!(
            (code, out.as_str(), err.as_str()),
            (Some(0), "", ""),
            "{backend:?}: {:#?}",
            frames[0].1
        );
        // The second item's take, by hand from recording.md: tag 0, the
        // connection's ordinal 2 (the grant of `Stdout` is 1), 32 plain bytes
        // with 5 first, then one part, the `String`'s list (0) of 4 bytes,
        // "text", which becomes "tcxt". The root's frames: its `GRANT` (1),
        // the three sends (2-4), the item takes (5-7) and the write (8).
        let tape = std::fs::read(&path).unwrap();
        let tape_id = hirsel_tape_format::Index::open(|at: u64, b: &mut [u8]| {
            std::os::unix::fs::FileExt::read_at(&std::fs::File::open(&path).unwrap(), b, at)
        })
        .unwrap()
        .1
        .tape_id;
        let wrong = u4_patched(&tape, &tape_id, 0, |payload| {
            let Some(at) = payload.windows(6).position(|w| w == [0, 4, b't', b'e', b'x', b't']) else {
                return false;
            };
            payload[at + 3] = b'c';
            true
        });
        let corrupt = dir.path().join(format!("u5q-{backend:?}-text.tape"));
        std::fs::write(&corrupt, wrong).unwrap();
        let (code, out, err) = u4_replay(&program, &corrupt, None);
        assert_eq!((code, out.as_str()), (Some(1), ""), "{backend:?}: {err}");
        let lines: Vec<_> = err.lines().collect();
        assert!(
            matches!(&lines[..], [l] if l.starts_with("replay: Diverged(Arg) at 8 in root ")),
            "{backend:?}: {err} {:#?}",
            frames[0].1
        );
    }
}

/// A granted capability replays where it arrives, in an item or a helper's
/// result: its tag 5 snapshot takes the receiving unit's next ordinal, as
/// the recorder numbered it, and a grant on that unit's own cell, so its
/// draw is checked against that unit's stream. Before, any item or result
/// holding a capability was refused. Three programs record and replay
/// `Sealed`: a `Random` the root sends itself (RL9_CAPABILITY_HOME), one the
/// root sends a helper (RL9_CAPABILITY_HELPER), replayed as that helper's
/// unit alone, since its end hands the root a survivor, and one a helper's
/// result returns (RL9_RESULT). The oracle is the snapshot's layout in
/// recording.md: handle part (1), tag 5, the ordinal, kind 3 (`Random`), no
/// stored bytes. Renaming the ordinal to one the unit never numbers ends
/// the replay at that `TAKE`, in the unit that took it, with one line.
#[test]
#[ignore = "explicit native integration target"]
fn u5_capabilities_replay_where_they_arrive() {
    crate::openat2_required::require();
    use hirsel_tape_format::{Arrival, Frame};

    let dir = tempfile::tempdir().unwrap();
    let grants = ["Tasks", "Random", "Stdout"];
    let cases = [
        (RL9_CAPABILITY_HOME, "u5c-home", "capability", 0u32, "root", None),
        (RL9_CAPABILITY_HELPER, "u5c-helper", "capability", 1, "root.0", Some("root.0")),
        (RL9_RESULT, "u5c-result", "result", 0, "root", None),
    ];
    // A capability's part where it arrives: handle (1), tag 5, its ordinal
    // there (a send's canonical form has 0), `Random`, no stored bytes.
    let part = |w: &[u8]| matches!(w, [1, 5, o, 3, 0] if *o != 0);
    for backend in [Backend::Cranelift, Backend::Llvm] {
        for (source, name, printed, stream, unit, only) in cases {
            let path = rl9_record_with(source, dir.path(), name, backend, printed, &grants);
            let program = dir.path().join(format!("{name}-{backend:?}"));
            let at = format!("{backend:?} {name}");
            let (code, out, err) = u4_replay(&program, &path, only);
            assert_eq!((code, out.as_str(), err.as_str()), (Some(0), "", ""), "{at}");
            let names: &[&str] = if name == "u5c-home" {
                &["root"]
            } else {
                &["root", "root.0"]
            };
            let streams = unit_streams(&path, &at, names);
            let frames = &streams[stream as usize].1;
            let take = frames
                .iter()
                .position(|f| match f {
                    Frame::Take { body, .. } => {
                        matches!(body.arrival(), Arrival::Item(..) | Arrival::HelperEnd(..))
                            && body.bytes().windows(5).any(part)
                    }
                    _ => false,
                })
                .unwrap_or_else(|| panic!("{at}: no take holds a capability: {frames:#?}"));
            let tape = std::fs::read(&path).unwrap();
            let tape_id = hirsel_tape_format::Index::open(|at: u64, b: &mut [u8]| {
                std::os::unix::fs::FileExt::read_at(&std::fs::File::open(&path).unwrap(), b, at)
            })
            .unwrap()
            .1
            .tape_id;
            let wrong = u4_patched(&tape, &tape_id, stream, |payload| {
                let Some(i) = payload.windows(5).position(part) else {
                    return false;
                };
                payload[i + 2] = 99;
                true
            });
            let corrupt = dir.path().join(format!("{name}-{backend:?}-ordinal.tape"));
            std::fs::write(&corrupt, wrong).unwrap();
            let (code, out, err) = u4_replay(&program, &corrupt, only);
            assert_eq!((code, out.as_str()), (Some(1), ""), "{at}: {err}");
            let lines: Vec<_> = err.lines().collect();
            let prefix = format!("replay: Diverged(Take) at {} in {unit} ", take + 1);
            assert!(
                matches!(&lines[..], [l] if l.starts_with(&prefix)),
                "{at}: {err} {frames:#?}"
            );
        }
    }
}

/// Helper-end survivors and their release takes replay: at the owner's end
/// take, each survivor's amount stays charged under its `SurvivorName` until
/// the `LastRelease` / `SharedRelease` / `RETIRED` or tag-9 `Refund` take
/// releases it. Before, every helper end with survivors was refused. Three
/// RL9 programs record and replay `Sealed`, both whole and `--unit root`, on
/// Cranelift and LLVM: `RL9_UNITS` and `RL9_VARIANTS` (surviving connections
/// released at the root's drop of their client) and `RL9_KEY_SURVIVOR`
/// (surviving `KeyShare`). Corrupting the surviving connection's ordinal in
/// `RL9_UNITS`'s `LastRelease` take ends replay `Diverged(Take)` at that
/// frame in `root` with one line.
#[test]
#[ignore = "explicit native integration target"]
fn u5_survivors_and_refunds_replay() {
    crate::openat2_required::require();
    use hirsel_tape_format::{Arrival, Frame};

    let dir = tempfile::tempdir().unwrap();
    let cases = [
        (RL9_UNITS, "u5s-units", "conn", &["Tasks", "Stdout"][..]),
        (RL9_VARIANTS, "u5s-variants", "variants", &["Tasks", "Stdout"][..]),
        (
            RL9_KEY_SURVIVOR,
            "u5s-keysurv",
            "kept",
            &["Tasks", "Random", "Stdout"][..],
        ),
    ];
    for backend in [Backend::Cranelift, Backend::Llvm] {
        for (source, name, printed, grants) in cases {
            let path = rl9_record_with(source, dir.path(), name, backend, printed, grants);
            let program = dir.path().join(format!("{name}-{backend:?}"));
            let at = format!("{backend:?} {name}");
            for only in [None, Some("root")] {
                let (code, out, err) = u4_replay(&program, &path, only);
                assert_eq!(
                    (code, out.as_str(), err.as_str()),
                    (Some(0), "", ""),
                    "{at} only={only:?}"
                );
            }
            if name == "u5s-units" {
                let streams = rl9_streams(&path, &at);
                let root = &streams[0].1;
                let lr = root
                    .iter()
                    .position(|f| match f {
                        Frame::Take { body, .. } => {
                            matches!(body.arrival(), Arrival::LastRelease(1, 2, ..))
                        }
                        _ => false,
                    })
                    .unwrap_or_else(|| panic!("{at}: no LastRelease(1, 2): {root:#?}"));
                let tape = std::fs::read(&path).unwrap();
                let tape_id = hirsel_tape_format::Index::open(|at: u64, b: &mut [u8]| {
                    std::os::unix::fs::FileExt::read_at(&std::fs::File::open(&path).unwrap(), b, at)
                })
                .unwrap()
                .1
                .tape_id;
                // LastRelease(stream=1, ordinal=2): body length 22, tag 6,
                // stream 1 (4 bytes LE), ordinal 2.
                let pattern = [22u8, 6, 1, 0, 0, 0, 2];
                let wrong = u4_patched(&tape, &tape_id, 0, |payload| {
                    let Some(i) = payload.windows(7).position(|w| w == pattern) else {
                        return false;
                    };
                    payload[i + 6] = 99;
                    true
                });
                let corrupt = dir.path().join(format!("{name}-{backend:?}-lr.tape"));
                std::fs::write(&corrupt, wrong).unwrap();
                let (code, out, err) = u4_replay(&program, &corrupt, Some("root"));
                assert_eq!((code, out.as_str()), (Some(1), ""), "{at}: {err}");
                let lines: Vec<_> = err.lines().collect();
                let prefix = format!("replay: Diverged(Take) at {} in root ", lr + 1);
                assert!(
                    matches!(&lines[..], [l] if l.starts_with(&prefix)),
                    "{at}: {err} {root:#?}"
                );
            }
        }
    }
}

/// Waiting sends settled `Closed` (returning the item) and `Withdrawn` (on cancel).
const U5_WAITING_REFUSALS: &str = r#"
fn wait_close(take c: Client[u64]) -> u64 waits {
    match conn.send(c, 99) {
        Ok(()) => 0,
        Error(Closed(v)) => v,
    }
}

fn wait_withdraw(take c: Client[u64]) -> u64 waits {
    match conn.send(c, 55) {
        Ok(()) => 1,
        Error(Closed(v)) => v,
    }
}

fn yield_once(take n: u64) -> u64 { n }

pub fn main(tasks: Tasks, out: Stdout) -> Result[(), Exit] waits {
    let (c1, i1): (Client[u64], Inbox[u64]) = result.unwrap(conn.new(1, bytes 0))
    result.unwrap(conn.try_send(c1, 1))
    let t1 = task.spawn(tasks, bytes 65536, take c1, wait_close)
    let y1 = task.spawn(tasks, bytes 65536, 0, yield_once)
    let _ = result.unwrap(task.join(take y1))
    let _ = take i1
    if result.unwrap(task.join(take t1)) != 99 { return Error(Exit(status: 3)) }

    let (c2, i2): (Client[u64], Inbox[u64]) = result.unwrap(conn.new(1, bytes 0))
    result.unwrap(conn.try_send(c2, 2))
    let mut t2 = task.spawn(tasks, bytes 65536, take c2, wait_withdraw)
    let y2 = task.spawn(tasks, bytes 65536, 0, yield_once)
    let _ = result.unwrap(task.join(take y2))
    task.cancel(mut t2)
    match task.join(take t2) {
        Error(Cancelled(take causes)) => { let _ = take causes },
        _ => return Error(Exit(status: 4)),
    }
    let _ = take i2
    result.unwrap(stdout.write(out, "settled"))
    Ok(())
}
"#;

/// Waiting sends (`CONN_SEND` `ISSUE`) and their settlement `TAKE`s (tag 3:
/// `Accepted`, `Closed`, `Withdrawn`) replay through the pending-send ledger.
/// Before, any waiting send was refused at `ISSUE` (`a send that waits`).
/// `RL9_ACCEPTED` (Accepted) and `U5_WAITING_REFUSALS` (Closed and Withdrawn)
/// record and replay `Sealed` whole and `--unit root` on Cranelift and LLVM.
/// Corrupting the settlement's `ISSUE` position in `RL9_ACCEPTED` ends replay
/// `Diverged(Take)` at that settlement frame in `root` with one line.
#[test]
#[ignore = "explicit native integration target"]
fn u5_waiting_sends_and_settlements_replay() {
    crate::openat2_required::require();
    use hirsel_tape_format::{Arrival, Frame, Settlement, op};

    let dir = tempfile::tempdir().unwrap();
    let cases = [
        (RL9_ACCEPTED, "u5w-accepted", "accepted"),
        (U5_WAITING_REFUSALS, "u5w-refusals", "settled"),
    ];
    for backend in [Backend::Cranelift, Backend::Llvm] {
        for (source, name, printed) in cases {
            let path = rl9_record(source, dir.path(), name, backend, printed);
            let program = dir.path().join(format!("{name}-{backend:?}"));
            let at = format!("{backend:?} {name}");
            for only in [None, Some("root")] {
                let (code, out, err) = u4_replay(&program, &path, only);
                assert_eq!(
                    (code, out.as_str(), err.as_str()),
                    (Some(0), "", ""),
                    "{at} only={only:?}"
                );
            }
            if name == "u5w-accepted" {
                let root = rl8_root_frames(&path);
                let issue = root
                    .iter()
                    .position(|f| matches!(f, Frame::Issue { op: o, .. } if *o == op::CONN_SEND))
                    .expect("waiting send ISSUE") as u8
                    + 1;
                let settle = root
                    .iter()
                    .position(|f| match f {
                        Frame::Take { body, .. } => matches!(
                            body.arrival(),
                            Arrival::Settlement(p, Settlement::Accepted) if p == u64::from(issue)
                        ),
                        _ => false,
                    })
                    .expect("settlement TAKE");
                let tape = std::fs::read(&path).unwrap();
                let tape_id = hirsel_tape_format::Index::open(|at: u64, b: &mut [u8]| {
                    std::os::unix::fs::FileExt::read_at(&std::fs::File::open(&path).unwrap(), b, at)
                })
                .unwrap()
                .1
                .tape_id;
                // Settlement body: len 3, tag 3, issue position, outcome 0.
                let pattern = [3u8, 3, issue, 0];
                let wrong = u4_patched(&tape, &tape_id, 0, |payload| {
                    let Some(i) = payload.windows(4).position(|w| w == pattern) else {
                        return false;
                    };
                    payload[i + 2] = 99;
                    true
                });
                let corrupt = dir.path().join(format!("{name}-{backend:?}-settle.tape"));
                std::fs::write(&corrupt, wrong).unwrap();
                let (code, out, err) = u4_replay(&program, &corrupt, Some("root"));
                assert_eq!((code, out.as_str()), (Some(1), ""), "{at}: {err}");
                assert!(
                    err.trim().starts_with("replay: Corrupt "),
                    "{at}: {err} {root:#?}"
                );
            }
            if name == "u5w-refusals" {
                let root = rl8_root_frames(&path);
                let settle = root
                    .iter()
                    .position(|f| match f {
                        Frame::Take { body, .. } => matches!(
                            body.arrival(),
                            Arrival::Settlement(_, Settlement::Closed(_))
                        ),
                        _ => false,
                    })
                    .expect("closed settlement TAKE");
                let issue = match &root[settle] {
                    Frame::Take { body, .. } => match body.arrival() {
                        Arrival::Settlement(p, _) => p as u8,
                        _ => unreachable!(),
                    },
                    _ => unreachable!(),
                };
                let tape = std::fs::read(&path).unwrap();
                let tape_id = hirsel_tape_format::Index::open(|at: u64, b: &mut [u8]| {
                    std::os::unix::fs::FileExt::read_at(&std::fs::File::open(&path).unwrap(), b, at)
                })
                .unwrap()
                .1
                .tape_id;
                // Closed settlement body: tag 3, issue, outcome 1, Value(len 8, 99u64, 0 parts).
                let pattern = [3u8, issue, 1, 8, 99, 0, 0, 0, 0, 0, 0, 0, 0];
                let wrong = u4_patched(&tape, &tape_id, 0, |payload| {
                    let Some(i) = payload.windows(pattern.len()).position(|w| w == pattern) else {
                        return false;
                    };
                    payload[i + 4] = 88;
                    true
                });
                let corrupt = dir.path().join(format!("{name}-{backend:?}-closed.tape"));
                std::fs::write(&corrupt, wrong).unwrap();
                let (code, out, err) = u4_replay(&program, &corrupt, Some("root"));
                assert_eq!((code, out.as_str()), (Some(1), ""), "{at}: {err}");
                let lines: Vec<_> = err.lines().collect();
                let prefix = format!("replay: Diverged(Take) at {} in root ", settle + 1);
                assert!(
                    matches!(&lines[..], [l] if l.starts_with(&prefix)),
                    "{at}: {err} {root:#?}"
                );
            }
        }
    }
}

/// `Shared` holders (including nested `Shared[Shared[u64]]` and a survivor
/// `Shared` node) and TCP sockets replay where they arrive in entries and
/// items (`RL9_SHARED`, `RL9_SURVIVOR`, `RL9_NESTED`, `RL9_SOCKET`), both
/// whole and `--unit root` (and `--unit root.0` for helper streams), on
/// Cranelift and LLVM. Corrupting an arriving `Shared` or `Socket` ordinal
/// ends replay `Diverged(Take)` at that take frame in `root` with one line.
#[test]
#[ignore = "explicit native integration target"]
fn u5_shared_and_socket_arrivals_replay() {
    crate::openat2_required::require();
    use hirsel_tape_format::{Arrival, Frame};

    let dir = tempfile::tempdir().unwrap();
    let shared_cases: &[(&str, &str, &str, &[Option<&str>])] = &[
        (
            RL9_SHARED,
            "u5s-shared",
            "shared",
            &[None, Some("root"), Some("root.0")],
        ),
        (
            RL9_SURVIVOR,
            "u5s-survivor",
            "survivor",
            &[None, Some("root"), Some("root.0")],
        ),
        (RL9_NESTED, "u5s-nested", "nested", &[None, Some("root")]),
    ];
    for backend in [Backend::Cranelift, Backend::Llvm] {
        for &(source, name, printed, units) in shared_cases {
            let path = rl9_record(source, dir.path(), name, backend, printed);
            let program = dir.path().join(format!("{name}-{backend:?}"));
            let at = format!("{backend:?} {name}");
            for &only in units {
                let (code, out, err) = u4_replay(&program, &path, only);
                assert_eq!(
                    (code, out.as_str(), err.as_str()),
                    (Some(0), "", ""),
                    "{at} only={only:?}"
                );
            }
            if name == "u5s-survivor" {
                let streams = rl9_streams(&path, &at);
                let root = &streams[0].1;
                let g = root
                    .iter()
                    .filter(|f| matches!(f, Frame::Grant { .. }))
                    .count() as u8;
                let item = root
                    .iter()
                    .position(|f| {
                        matches!(f, Frame::Take { body, .. } if matches!(body.arrival(), Arrival::Item(..)))
                    })
                    .expect("shared item take");
                let tape = std::fs::read(&path).unwrap();
                let tape_id = hirsel_tape_format::Index::open(|at: u64, b: &mut [u8]| {
                    std::os::unix::fs::FileExt::read_at(&std::fs::File::open(&path).unwrap(), b, at)
                })
                .unwrap()
                .1
                .tape_id;
                // Shared snapshot part: part_count 1, part tag 1, kind 1, ordinal g+4, home stream 1, node 2.
                let pattern = [1u8, 1, 1, g + 4, 1, 0, 0, 0, 2];
                let wrong = u4_patched(&tape, &tape_id, 0, |payload| {
                    let Some(i) = payload.windows(pattern.len()).position(|w| w == pattern) else {
                        return false;
                    };
                    payload[i + 3] = 99;
                    true
                });
                let corrupt = dir.path().join(format!("{name}-{backend:?}-ord.tape"));
                std::fs::write(&corrupt, wrong).unwrap();
                let (code, out, err) = u4_replay(&program, &corrupt, Some("root"));
                assert_eq!((code, out.as_str()), (Some(1), ""), "{at}: {err}");
                let lines: Vec<_> = err.lines().collect();
                let prefix = format!("replay: Diverged(Take) at {} in root ", item + 1);
                assert!(
                    matches!(&lines[..], [l] if l.starts_with(&prefix)),
                    "{at}: {err}"
                );
            }
        }
        let path = rl9_record_net(
            RL9_SOCKET,
            dir.path(),
            "u5s-socket",
            backend,
            "sock",
            &["Net", "Tasks", "Stdout"],
            &["Listen 127.0.0.0/8 0-65535", "Connect 127.0.0.0/8 1-65535"],
        );
        let program = dir.path().join(format!("u5s-socket-{backend:?}"));
        let at = format!("{backend:?} u5s-socket");
        for only in [None, Some("root"), Some("root.0")] {
            let (code, out, err) = u4_replay(&program, &path, only);
            assert_eq!(
                (code, out.as_str(), err.as_str()),
                (Some(0), "", ""),
                "{at} only={only:?}"
            );
        }
        let streams = rl9_streams(&path, &at);
        let root = &streams[0].1;
        let g = root
            .iter()
            .filter(|f| matches!(f, Frame::Grant { .. }))
            .count() as u8;
        let item = root
            .iter()
            .position(|f| {
                matches!(f, Frame::Take { body, .. } if matches!(body.arrival(), Arrival::Item(..)))
            })
            .expect("socket item take");
        let tape = std::fs::read(&path).unwrap();
        let tape_id = hirsel_tape_format::Index::open(|at: u64, b: &mut [u8]| {
            std::os::unix::fs::FileExt::read_at(&std::fs::File::open(&path).unwrap(), b, at)
        })
        .unwrap()
        .1
        .tape_id;
        // Socket snapshot part: part_count 1, part tag 1, kind 2 (Stream), ordinal g+8, accepted 1.
        let pattern = [1u8, 1, 2, g + 8, 1];
        let wrong = u4_patched(&tape, &tape_id, 0, |payload| {
            let Some(i) = payload.windows(pattern.len()).position(|w| w == pattern) else {
                return false;
            };
            payload[i + 3] = 99;
            true
        });
        let corrupt = dir.path().join(format!("u5s-socket-{backend:?}-ord.tape"));
        std::fs::write(&corrupt, wrong).unwrap();
        let (code, out, err) = u4_replay(&program, &corrupt, Some("root"));
        assert_eq!((code, out.as_str()), (Some(1), ""), "{at}: {err}");
        let lines: Vec<_> = err.lines().collect();
        let prefix = format!("replay: Diverged(Take) at {} in root ", item + 1);
        assert!(
            matches!(&lines[..], [l] if l.starts_with(&prefix)),
            "{at}: {err}"
        );
    }
}

/// A list of capabilities crosses into a helper entry and back in its result.
const U5_HELPER_LIST: &str = r#"
fn draw_and_return(take rs: List[Random]) -> List[Random] {
    let mut xs = take rs
    let last = option.unwrap(list.pop(mut xs))
    let mut b: Bytes = list.with_capacity(8)
    host.random(last, mut b)
    take xs
}

pub fn main(tasks: Tasks, r: Random, out: Stdout) -> Result[(), Exit] waits {
    let mut rs: List[Random] = list.with_capacity(2)
    list.push(mut rs, r)
    list.push(mut rs, r)
    let h = result.unwrap(task.spawn_unit(tasks, bytes 65536, take rs, draw_and_return))
    let mut back = result.unwrap(task.join(take h))
    let first = option.unwrap(list.pop(mut back))
    let mut b: Bytes = list.with_capacity(8)
    host.random(first, mut b)
    result.unwrap(stdout.write(out, "hlist"))
    Ok(())
}
"#;

/// Lists whose elements hold handles (`RL9_LIST`, `RL9_VARIANT`'s
/// `List[Option[Random]]`, and `U5_HELPER_LIST` across helper entry and
/// result) replay per-element parts in items, entries and results, both whole
/// and `--unit root` (and `--unit root.0` for `U5_HELPER_LIST`), on Cranelift
/// and LLVM. Corrupting an arriving element's capability ordinal in `RL9_LIST`
/// ends replay `Diverged(Take)` at that take frame in `root` with one line.
#[test]
#[ignore = "explicit native integration target"]
fn u5_held_lists_replay() {
    crate::openat2_required::require();
    use hirsel_tape_format::{Arrival, Frame};

    let dir = tempfile::tempdir().unwrap();
    let cases: &[(&str, &str, &str, &[&str], &[Option<&str>])] = &[
        (
            RL9_LIST,
            "u5l-list",
            "list",
            &["Tasks", "Random", "Stdout"],
            &[None, Some("root")],
        ),
        (
            RL9_VARIANT,
            "u5l-variant",
            "variant",
            &["Random", "Stdout"],
            &[None, Some("root")],
        ),
        (
            U5_HELPER_LIST,
            "u5l-hlist",
            "hlist",
            &["Tasks", "Random", "Stdout"],
            &[None, Some("root"), Some("root.0")],
        ),
    ];
    for backend in [Backend::Cranelift, Backend::Llvm] {
        for &(source, name, printed, grants, units) in cases {
            let path = rl9_record_with(source, dir.path(), name, backend, printed, grants);
            let program = dir.path().join(format!("{name}-{backend:?}"));
            let at = format!("{backend:?} {name}");
            for &only in units {
                let (code, out, err) = u4_replay(&program, &path, only);
                assert_eq!(
                    (code, out.as_str(), err.as_str()),
                    (Some(0), "", ""),
                    "{at} only={only:?}"
                );
            }
            if name == "u5l-list" {
                let root = rl8_root_frames(&path);
                let g = root
                    .iter()
                    .filter(|f| matches!(f, Frame::Grant { .. }))
                    .count() as u8;
                let item = root
                    .iter()
                    .position(|f| {
                        matches!(f, Frame::Take { body, .. } if matches!(body.arrival(), Arrival::Item(..)))
                    })
                    .expect("list item take");
                let tape = std::fs::read(&path).unwrap();
                let tape_id = hirsel_tape_format::Index::open(|at: u64, b: &mut [u8]| {
                    std::os::unix::fs::FileExt::read_at(&std::fs::File::open(&path).unwrap(), b, at)
                })
                .unwrap()
                .1
                .tape_id;
                // Second element capability snapshot: part tag 1, kind 5, ordinal g+4, grant kind 3 (Random), 0 stored.
                let pattern = [1u8, 5, g + 3, 3, 0, 1, 5, g + 4, 3, 0];
                let wrong = u4_patched(&tape, &tape_id, 0, |payload| {
                    let Some(i) = payload.windows(pattern.len()).position(|w| w == pattern) else {
                        return false;
                    };
                    payload[i + 7] = 99;
                    true
                });
                let corrupt = dir.path().join(format!("{name}-{backend:?}-ord.tape"));
                std::fs::write(&corrupt, wrong).unwrap();
                let (code, out, err) = u4_replay(&program, &corrupt, Some("root"));
                assert_eq!((code, out.as_str()), (Some(1), ""), "{at}: {err}");
                let lines: Vec<_> = err.lines().collect();
                let prefix = format!("replay: Diverged(Take) at {} in root ", item + 1);
                assert!(
                    matches!(&lines[..], [l] if l.starts_with(&prefix)),
                    "{at}: {err}"
                );
            }
        }
    }
}
