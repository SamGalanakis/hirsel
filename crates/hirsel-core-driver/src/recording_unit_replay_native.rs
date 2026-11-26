// Replay of a run with helpers (docs/recording.md, unit tapes U4b and U4d):
// a replayed spawn starts nothing, each helper end comes from its parent's
// own `TAKE` of it, and each helper stream replays as its own unit from its
// entry. Included from recording_units_native.rs.

/// Two plain helpers, each joined by the root.
const U4_UNITS: &str = r#"
fn twice(take n: u64) -> u64 { n * 2 }

fn next(take n: u64) -> u64 { n + 1 }

pub fn main(tasks: Tasks, out: Stdout) -> Result[(), Exit] waits {
    let h = result.unwrap(task.spawn_unit(tasks, bytes 65536, 21, twice))
    let g = result.unwrap(task.spawn_unit(tasks, bytes 65536, 4, next))
    let a = result.unwrap(task.join(take h))
    let b = result.unwrap(task.join(take g))
    if a != 42 || b != 5 { return Error(Exit(status: 3)) }
    result.unwrap(stdout.write(out, "units"))
    Ok(())
}
"#;

/// A tape file's blocks in file order: each block's bytes, whole.
fn u4_blocks(tape: &[u8]) -> Vec<&[u8]> {
    use hirsel_tape_format::{BLOCK_CHECK, BLOCK_HEAD};
    let mut blocks = Vec::new();
    let mut at = 0;
    while at < tape.len() {
        let len = u32::from_le_bytes(tape[at + 16..at + 20].try_into().unwrap()) as usize;
        let end = at + BLOCK_HEAD + len + BLOCK_CHECK;
        blocks.push(&tape[at..end]);
        at = end;
    }
    blocks
}

/// The block's stream number (its head's bytes 4..8).
fn u4_stream(block: &[u8]) -> u32 {
    u32::from_le_bytes(block[4..8].try_into().unwrap())
}

/// The tape with the first block of `stream` whose payload `patch` changes
/// (it returns true) resealed, every other block as it was.
fn u4_patched(
    tape: &[u8],
    tape_id: &[u8; 16],
    stream: u32,
    mut patch: impl FnMut(&mut [u8]) -> bool,
) -> Vec<u8> {
    use hirsel_tape_format::{BLOCK_CHECK, BLOCK_HEAD};
    let mut out = Vec::new();
    let mut done = false;
    for block in u4_blocks(tape) {
        let mut bytes = block[..block.len() - BLOCK_CHECK].to_vec();
        if done || u4_stream(block) != stream || !patch(&mut bytes[BLOCK_HEAD..]) {
            out.extend_from_slice(block);
            continue;
        }
        done = true;
        let seq = u64::from_le_bytes(block[8..16].try_into().unwrap());
        hirsel_tape_format::seal(tape_id, block[0], stream, seq, &mut bytes);
        out.extend_from_slice(&bytes);
    }
    assert!(done, "no block of stream {stream} to patch");
    out
}

/// Patch the first helper-end `TAKE` delivering a u64 in `values` to name
/// ordinal 99:
/// arrival tag 4, the ordinal, Returned (0), then the value's 8 plain bytes
/// and no parts or survivors.
fn u4_take_to_99(values: &[u8]) -> impl FnMut(&mut [u8]) -> bool + '_ {
    move |payload| {
        let Some(at) = payload.windows(14).position(|w| {
            w[0] == 4 && w[2..4] == [0, 8] && values.contains(&w[4]) && w[5..14].iter().all(|&b| b == 0)
        }) else {
            return false;
        };
        assert!(payload[at + 1] < 99);
        payload[at + 1] = 99;
        true
    }
}

/// Replay the recorded image itself as `hirsel replay` does, with the unit
/// `unit` alone when given: exit code, stdout, stderr.
fn u4_replay(program: &Path, tape: &Path, unit: Option<&str>) -> (Option<i32>, String, String) {
    let tape = tape.to_str().unwrap();
    let mut argv = vec!["--hirsel-replay-recording", "same", tape];
    argv.extend(unit);
    tape_run(program, &argv, None)
}

/// U4b's gate laws, on one recording per backend:
/// - (a) With one helper's stream dropped from the file, a full replay ends
///   `Truncated at 0` in that child, exit 1, at its spawn. With `--unit
///   root` the same tape replays to `Sealed` (exit 0, nothing printed): the
///   root's every observation matched the recording and both helper ends
///   came from its own takes.
/// - The intact tape's full replay is `Sealed` (exit 0, nothing printed):
///   each helper stream replays from its entry as its own unit. So is
///   `--unit root.0` alone.
/// - (b) A root `TAKE` naming a helper ordinal the root never spawned
///   cannot apply: `--unit root` ends `Diverged(Take)` at that TAKE.
///
/// The root's stream, by hand from recording.md (positions count frames
/// from 1, as every verdict's do): the run's `GRANT`s of `Tasks` (1) and
/// `Stdout` (2), the `UNIT_SPAWN` OP of `twice` (3) and of `next` (4), the
/// two helper ends' `TAKE`s (5 and 6), the Stdout write's OP (7) and the
/// root's `END` (8). The first TAKE is at 5, whatever the helper it
/// delivers.
#[test]
#[ignore = "explicit native integration target"]
fn u4_root_replays_its_helper_takes() {
    crate::openat2_required::require();
    use hirsel_tape_format::{Frame, Index, op};
    use std::os::unix::fs::FileExt;

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let mut request = source_request(U4_UNITS, dir.path());
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
        options.grant_mask = ["Tasks", "Stdout"]
            .iter()
            .fold(0, |m, g| m | 1 << grant_kind(g).unwrap());
        options.cas = dir.path().join(format!("u4-store-{backend:?}"));
        options.output = dir.path().join(format!("u4-{backend:?}"));
        let program = product.build(&entry, &options).unwrap().executable;
        let path = dir.path().join(format!("u4-{backend:?}.tape"));
        let (code, out, err) = tape_run(&program, &[], Some(&path));
        assert_eq!((code, out.as_str()), (Some(0), "units"), "{backend:?}: {err}");
        let tape = std::fs::read(&path).unwrap();

        // The root's frames are as counted above.
        let file = std::fs::File::open(&path).unwrap();
        let read = |at: u64, b: &mut [u8]| file.read_at(b, at);
        let (mut index, header) = Index::open(read).unwrap();
        let paths: Vec<_> = index.paths().collect();
        let names: Vec<_> = paths.iter().map(|(_, n)| n.as_str()).collect();
        assert_eq!(names, ["root", "root.0", "root.1"], "{backend:?}");
        let gone = paths[2].0;
        let mut blocks = index.take(0).unwrap();
        let mut decoder = blocks.decoder(read).unwrap();
        let mut frames = Vec::new();
        while let Some((flags, payload)) = blocks.next(read).unwrap() {
            frames.extend(
                decoder
                    .block(payload, flags & hirsel_tape_format::END != 0)
                    .unwrap(),
            );
        }
        let shape: Vec<_> = frames
            .iter()
            .map(|f| match f {
                Frame::Op { op: o, .. } if *o == op::UNIT_SPAWN => "spawn",
                Frame::Op { .. } => "op",
                Frame::Take { .. } => "take",
                Frame::End { .. } => "end",
                Frame::Grant { .. } => "grant",
                _ => "other",
            })
            .collect();
        assert_eq!(
            shape,

            ["grant", "grant", "spawn", "spawn", "take", "take", "op", "end"],
            "{backend:?}: {frames:?}"
        );

        // The intact tape: every stream Sealed, together and root.0 alone.
        for unit in [None, Some("root.0")] {
            let (code, out, err) = u4_replay(&program, &path, unit);
            assert_eq!((code, out.as_str(), err.as_str()), (Some(0), "", ""), "{backend:?} {unit:?}");
        }

        // (a) root.1's blocks dropped: its spawn is the root's second.
        let blocks = u4_blocks(&tape);
        let dropped: Vec<u8> = blocks
            .iter()
            .filter(|b| u4_stream(b) != gone)
            .flat_map(|b| b.iter().copied())
            .collect();
        let missing = dir.path().join(format!("u4-{backend:?}-missing.tape"));
        std::fs::write(&missing, &dropped).unwrap();
        let (code, out, err) = u4_replay(&program, &missing, None);
        assert_eq!(
            (code, out.as_str(), err.as_str()),
            (Some(1), "", "replay: Truncated at 0 in root.1\n"),
            "{backend:?}"
        );
        let (code, out, err) = u4_replay(&program, &missing, Some("root"));
        assert_eq!((code, out.as_str(), err.as_str()), (Some(0), "", ""), "{backend:?}");
        // The package replay image (`hirsel replay PACKAGE`) checks both
        // spawns by the link's spawn table as the recording did, so it too
        // reaches the second spawn and finds root.1 missing.
        let recording = options.clone();
        options.grant_mask = 0;
        options.replay = Some(crate::ReplayPlan {
            path: missing.to_str().unwrap().into(),
            diagnose: false,
        });
        options.output = dir.path().join(format!("u4-{backend:?}-replay"));
        let replay = product.build(&entry, &options).unwrap().executable;
        options = recording;
        let (code, out, err) = tape_run(&replay, &[], None);
        assert_eq!(
            (code, out.as_str(), err.as_str()),
            (Some(1), "", "replay: Truncated at 0 in root.1\n"),
            "{backend:?}: package replay"
        );

        // (b) The first root TAKE (twice's 42 or next's 5) names ordinal 99.
        let wrong = u4_patched(&tape, &header.tape_id, 0, u4_take_to_99(&[42, 5]));
        let corrupt = dir.path().join(format!("u4-{backend:?}-take.tape"));
        std::fs::write(&corrupt, &wrong).unwrap();
        let (code, out, err) = u4_replay(&program, &corrupt, Some("root"));
        assert_eq!((code, out.as_str()), (Some(1), ""), "{backend:?}: {err}");
        assert!(
            err.lines().any(|l| l.starts_with("replay: Diverged(Take) at 5 in root ")
                && l.contains("no helper with ordinal 99")),
            "{backend:?}: {err}"
        );
    }
}

/// A helper that spawns and joins a helper of its own, beside a plain one.
const U4D_UNITS: &str = r#"
type Job { tasks: Tasks, n: u64 }

fn inner(take n: u64) -> u64 { n * 3 }

fn outer(take job: Job) -> u64 waits {
    let h = result.unwrap(task.spawn_unit(job.tasks, bytes 65536, job.n, inner))
    result.unwrap(task.join(take h)) + 1
}

fn next(take n: u64) -> u64 { n + 1 }

pub fn main(tasks: Tasks, out: Stdout) -> Result[(), Exit] waits {
    let h = result.unwrap(task.spawn_unit(tasks, bytes 262144, Job(tasks: tasks, n: 7), outer))
    let g = result.unwrap(task.spawn_unit(tasks, bytes 65536, 4, next))
    let a = result.unwrap(task.join(take h))
    let b = result.unwrap(task.join(take g))
    if a != 22 || b != 5 { return Error(Exit(status: 3)) }
    result.unwrap(stdout.write(out, "units"))
    Ok(())
}
"#;

/// U4d's gate laws: each helper stream replays as its own unit from its
/// entry, and one unit's divergence ends only that unit.
/// - The intact tape (streams root, root.0, root.0.0, root.1) replays
///   `Sealed` in full and with each `--unit` alone. root.0's entry crosses a
///   `Tasks` capability, which its own spawn of root.0.0 uses.
/// - (c) root.0's `TAKE` of root.0.0's end names ordinal 99. root.0's
///   stream after its entry, by hand: the `UNIT_SPAWN` OP of `inner` (1) and
///   that `TAKE` (2), so root.0 ends `Diverged(Take) at 2`. The root, which
///   takes root.0's end from its own stream, and root.0.0 and root.1 are
///   `Sealed`: the replay writes that one line and exits 1.
/// - (d) root.1's entry names a code descriptor this build has no spawn
///   table row for: root.1 ends `Diverged(Op) at 0`, alone.
#[test]
#[ignore = "explicit native integration target"]
fn u4d_helper_streams_replay_as_units() {
    crate::openat2_required::require();
    use hirsel_tape_format::Index;
    use std::os::unix::fs::FileExt;

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let mut request = source_request(U4D_UNITS, dir.path());
    request.inputs.packages[1] =
        PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
    let product = Driver::default()
        .check(request, &mut Meter::new(false))
        .unwrap();
    assert!(product.accepted(), "{:?}", product.diagnostics);
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    // The release LLVM build cuts its program into per-definition parts:
    // each helper's code descriptor and spawnable marker must survive the cut.
    // Lowered one body per partition, a helper's descriptor is also referenced
    // only from its spawner's partition.
    let builds = [
        (Backend::Cranelift, "dev"),
        (Backend::Llvm, "dev"),
        (Backend::Llvm, "release"),
        (Backend::Llvm, "partitioned"),
    ];
    for (backend, build) in builds {
        let mut options = native_options(backend, dir.path());
        options.runtime = env_record_runtime();
        options.recording = true;
        options.optimize = build != "dev";
        options.grant_mask = ["Tasks", "Stdout"]
            .iter()
            .fold(0, |m, g| m | 1 << grant_kind(g).unwrap());
        let backend = format!("{backend:?}-{build}");
        options.cas = dir.path().join(format!("u4d-store-{backend}"));
        options.output = dir.path().join(format!("u4d-{backend}"));
        super::release::RELEASE_PARTITION.set((build == "partitioned").then_some(1));
        let program = product.build(&entry, &options).unwrap().executable;
        super::release::RELEASE_PARTITION.set(None);
        let path = dir.path().join(format!("u4d-{backend}.tape"));
        let (code, out, err) = tape_run(&program, &[], Some(&path));
        assert_eq!((code, out.as_str()), (Some(0), "units"), "{backend}: {err}");
        let tape = std::fs::read(&path).unwrap();
        let file = std::fs::File::open(&path).unwrap();
        let read = |at: u64, b: &mut [u8]| file.read_at(b, at);
        let (mut index, header) = Index::open(read).unwrap();
        let paths: std::collections::BTreeMap<String, u32> = index.paths().map(|(s, n)| (n, s)).collect();
        let names: Vec<_> = paths.keys().map(String::as_str).collect();
        assert_eq!(names, ["root", "root.0", "root.0.0", "root.1"], "{backend}");

        for unit in [None, Some("root"), Some("root.0"), Some("root.0.0"), Some("root.1")] {
            let (code, out, err) = u4_replay(&program, &path, unit);
            assert_eq!((code, out.as_str(), err.as_str()), (Some(0), "", ""), "{backend} {unit:?}");
        }

        // (c) inner's result, 21, taken by root.0 under ordinal 99.
        let wrong = u4_patched(&tape, &header.tape_id, paths["root.0"], u4_take_to_99(&[21]));
        let corrupt = dir.path().join(format!("u4d-{backend}-take.tape"));
        std::fs::write(&corrupt, &wrong).unwrap();
        for unit in [None, Some("root.0")] {
            let (code, out, err) = u4_replay(&program, &corrupt, unit);
            assert_eq!((code, out.as_str()), (Some(1), ""), "{backend} {unit:?}: {err}");
            let lines: Vec<_> = err.lines().collect();
            assert!(
                matches!(&lines[..], [l] if l.starts_with("replay: Diverged(Take) at 2 in root.0 ")
                    && l.contains("no helper with ordinal 99")),
                "{backend} {unit:?}: {err}"
            );
        }

        // (d) root.1's descriptor with its last byte changed.
        let stream = paths["root.1"];
        let mut blocks = index.take(stream).unwrap();
        let mut decoder = blocks.decoder(read).unwrap();
        let entry_frame = loop {
            if let Some(entry) = decoder.take_entry() {
                break entry;
            }
            let (flags, payload) = blocks.next(read).unwrap().expect("root.1's entry");
            decoder.block(payload, flags & hirsel_tape_format::END != 0).unwrap();
        };
        let descriptor = entry_frame.body().descriptor.to_vec();
        let unknown = u4_patched(&tape, &header.tape_id, stream, |payload| {
            let Some(at) = payload.windows(descriptor.len()).position(|w| w == descriptor) else {
                return false;
            };
            payload[at + descriptor.len() - 1] ^= 0xff;
            true
        });
        let unresolved = dir.path().join(format!("u4d-{backend}-entry.tape"));
        std::fs::write(&unresolved, &unknown).unwrap();
        let (code, out, err) = u4_replay(&program, &unresolved, None);
        assert_eq!((code, out.as_str()), (Some(1), ""), "{backend}: {err}");
        let lines: Vec<_> = err.lines().collect();
        assert!(
            matches!(&lines[..], [l] if l.starts_with("replay: Diverged(Op) at 0 in root.1 ")),
            "{backend}: {err}"
        );
    }
}

/// A helper that fails with an owned message, and one its owner cancels
/// while it sleeps.
const U5_ENDS: &str = r#"
type Nap { clock: Clock, until: u64 }

fn boom(take n: u64) -> u64 {
    let mut m = string.with_capacity(40)
    string.push(mut m, "boom ")
    string.push_u64(mut m, n)
    fail(take m)
}

fn nap(take x: Nap) -> u64 waits {
    host.sleep_until(x.clock, x.until)
    1
}

pub fn main(tasks: Tasks, clock: Clock, out: Stdout) -> Result[(), Exit] waits {
    let f = result.unwrap(task.spawn_unit(tasks, bytes 65536, 5, boom))
    match task.join(take f) { Error(Failed(take causes)) => { let _ = take causes; }, _ => return Error(Exit(status: 3)) }
    let mut c = result.unwrap(task.spawn_unit(tasks, bytes 65536, Nap(clock: clock, until: 18446744073709551615), nap))
    task.cancel(mut c)
    match task.join(take c) { Error(Cancelled(take causes)) => { let _ = take causes; }, _ => return Error(Exit(status: 4)) }
    result.unwrap(stdout.write(out, "units"))
    Ok(())
}
"#;

/// Failed and cancelled helper ends replay: the owner's end take delivers
/// the recorded causes to its join, and its `UNIT_CANCEL` is checked.
/// - The intact tape (streams root, root.0, root.1) replays `Sealed` in full
///   and with each `--unit` alone. Red before: the root's failed end take
///   and its `UNIT_CANCEL` were refused (exit 2).
/// - The root's stream, by hand: the `GRANT`s of `Tasks`, `Clock` and
///   `Stdout` (1-3), the `UNIT_SPAWN` of `boom` (4), its end `TAKE` (5), the
///   `UNIT_SPAWN` of `nap` (6), its `UNIT_CANCEL` (7), its end `TAKE` (8),
///   the write's OP (9) and the `END` (10).
/// - (e) The failed end `TAKE` names ordinal 99: exactly one line,
///   `Diverged(Take) at 5 in root`.
/// - (f) The `UNIT_CANCEL` names object 99: exactly one line,
///   `Diverged(Object) at 7 in root`.
/// - (g) root.0's `END` message reads "boon 5": root.0's stream after its
///   entry holds only that `END` (no host operation), so exactly one line,
///   `Diverged(Outcome) at 1 in root.0`.
#[test]
#[ignore = "explicit native integration target"]
fn u5_failed_and_cancelled_helper_ends_replay() {
    crate::openat2_required::require();
    use hirsel_tape_format::Index;
    use std::os::unix::fs::FileExt;

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let mut request = source_request(U5_ENDS, dir.path());
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
        options.grant_mask = ["Tasks", "Clock", "Stdout"]
            .iter()
            .fold(0, |m, g| m | 1 << grant_kind(g).unwrap());
        options.cas = dir.path().join(format!("u5-store-{backend:?}"));
        options.output = dir.path().join(format!("u5-{backend:?}"));
        let program = product.build(&entry, &options).unwrap().executable;
        let path = dir.path().join(format!("u5-{backend:?}.tape"));
        let (code, out, err) = tape_run(&program, &[], Some(&path));
        assert_eq!((code, out.as_str()), (Some(0), "units"), "{backend:?}: {err}");
        let tape = std::fs::read(&path).unwrap();
        let file = std::fs::File::open(&path).unwrap();
        let read = |at: u64, b: &mut [u8]| file.read_at(b, at);
        let (index, header) = Index::open(read).unwrap();
        let paths: std::collections::BTreeMap<String, u32> =
            index.paths().map(|(s, n)| (n, s)).collect();
        let names: Vec<_> = paths.keys().map(String::as_str).collect();
        assert_eq!(names, ["root", "root.0", "root.1"], "{backend:?}");

        for unit in [None, Some("root"), Some("root.0"), Some("root.1")] {
            let (code, out, err) = u4_replay(&program, &path, unit);
            assert_eq!((code, out.as_str(), err.as_str()), (Some(0), "", ""), "{backend:?} {unit:?}");
        }
        let one_line = |tape: &[u8], name: &str, prefix: &str| {
            let corrupt = dir.path().join(format!("u5-{backend:?}-{name}.tape"));
            std::fs::write(&corrupt, tape).unwrap();
            let (code, out, err) = u4_replay(&program, &corrupt, None);
            assert_eq!((code, out.as_str()), (Some(1), ""), "{backend:?} {name}: {err}");
            let lines: Vec<_> = err.lines().collect();
            assert!(
                matches!(&lines[..], [l] if l.starts_with(prefix)),
                "{backend:?} {name}: {err}"
            );
        };

        // (e) The failed end: tag 4, the ordinal, Failed (1), one cause of
        // kind Fail (4).
        let wrong = u4_patched(&tape, &header.tape_id, 0, |payload| {
            let Some(at) = payload
                .windows(5)
                .position(|w| w[0] == 4 && w[2] == 1 && w[3] == 1 && w[4] == 4)
            else {
                return false;
            };
            payload[at + 1] = 99;
            true
        });
        one_line(&wrong, "failed", "replay: Diverged(Take) at 5 in root ");

        // (f) The UNIT_CANCEL OP: code 167, the object, the empty literal and
        // the Out (status 0, value 0).
        let wrong = u4_patched(&tape, &header.tape_id, 0, |payload| {
            let Some(at) = payload
                .windows(5)
                .position(|w| w[0] == 167 && w[2..] == [0, 0, 0])
            else {
                return false;
            };
            payload[at + 1] = 99;
            true
        });
        one_line(&wrong, "cancel", "replay: Diverged(Object) at 7 in root ");

        // (g) root.0's END message.
        let wrong = u4_patched(&tape, &header.tape_id, paths["root.0"], |payload| {
            let Some(at) = payload.windows(6).position(|w| w == b"boom 5") else {
                return false;
            };
            payload[at + 3] = b'n';
            true
        });
        one_line(&wrong, "message", "replay: Diverged(Outcome) at 1 in root.0 ");
    }
}

/// The root sends two plain items to its own connection and receives them.
const U5_ITEMS: &str = r#"
pub fn main(out: Stdout) -> Result[(), Exit] waits {
    let (c, inbox): (Client[u64], Inbox[u64]) = result.unwrap(conn.new(2, bytes 0))
    result.unwrap(conn.send(c, 5))
    result.unwrap(conn.send(c, 7))
    let a = result.unwrap(conn.receive(inbox))
    let b = result.unwrap(conn.receive(inbox))
    if a * 10 + b != 57 { return Error(Exit(status: 3)) }
    result.unwrap(stdout.write(out, "conn"))
    Ok(())
}
"#;

/// A unit's own items replay from its item `TAKE`s, and its `CONN_SEND`s
/// and drops are checked.
#[test]
#[ignore = "explicit native integration target"]
fn u5_items_replay_from_their_takes() {
    crate::openat2_required::require();

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let mut request = source_request(U5_ITEMS, dir.path());
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
        options.cas = dir.path().join(format!("u5i-store-{backend:?}"));
        options.output = dir.path().join(format!("u5i-{backend:?}"));
        let program = product.build(&entry, &options).unwrap().executable;
        let path = dir.path().join(format!("u5i-{backend:?}.tape"));
        let (code, out, err) = tape_run(&program, &[], Some(&path));
        assert_eq!((code, out.as_str()), (Some(0), "conn"), "{backend:?}: {err}");
        let frames = unit_streams(&path, &format!("{backend:?}"), &["root"]);
        let (code, out, err) = u4_replay(&program, &path, None);
        assert_eq!(
            (code, out.as_str(), err.as_str()),
            (Some(0), "", ""),
            "{backend:?}: {:#?}",
            frames[0].1
        );
        let tape = std::fs::read(&path).unwrap();
        let tape_id = hirsel_tape_format::Index::open(|at: u64, b: &mut [u8]| {
            std::os::unix::fs::FileExt::read_at(&std::fs::File::open(&path).unwrap(), b, at)
        })
        .unwrap()
        .1
        .tape_id;
        let one_line = |name: &str, window: &[u8], at: usize, prefix: &str| {
            let wrong = u4_patched(&tape, &tape_id, 0, |payload| {
                let Some(i) = payload.windows(window.len()).position(|w| w == window) else {
                    return false;
                };
                payload[i + at] = 99;
                true
            });
            let corrupt = dir.path().join(format!("u5i-{backend:?}-{name}.tape"));
            std::fs::write(&corrupt, wrong).unwrap();
            let (code, out, err) = u4_replay(&program, &corrupt, None);
            assert_eq!((code, out.as_str()), (Some(1), ""), "{backend:?} {name}: {err}");
            let lines: Vec<_> = err.lines().collect();
            assert!(
                matches!(&lines[..], [l] if l.starts_with(prefix)),
                "{backend:?} {name}: {err}"
            );
        };
        // The first CONN_SEND (165) names the client, object 3, with item 5.
        one_line("send", &[165, 3, 10, 8, 5], 1, "replay: Diverged(Object) at 2 in root ");
        // The first item TAKE: tag 0, the connection's ordinal 2, item 5.
        one_line("item", &[0, 2, 8, 5], 1, "replay: Diverged(Take) at 4 in root ");
        // The inbox's CLOSE (3) names the connection, object 2. A CLOSE is
        // matched by its object, as a provider close is, so the drop that
        // finds none diverges at the frame after it.
        one_line("close", &[3, 2, 0, 0, 0], 1, "replay: Diverged(Object) at 8 in root ");
    }
}

/// A sealed `Secret` crosses in an item, a helper's entry and its result,
/// and always comes back to the root that paid for it, so no unit takes a
/// refund of its charges.
const U5_SECRETS: &str = r#"
fn bounce(take k: Secret) -> Secret waits {
    let (c, i): (Client[Secret], Inbox[Secret]) = result.unwrap(conn.new(1, bytes 0))
    result.unwrap(conn.send(c, take k))
    result.unwrap(conn.receive(i))
}

pub fn main(tasks: Tasks, out: Stdout) -> Result[(), Exit] waits {
    let mut b: Bytes = list.with_capacity(1)
    list.push(mut b, 5)
    let s = result.unwrap(secret.seal(take b, "mac"))
    let (c, i): (Client[Secret], Inbox[Secret]) = result.unwrap(conn.new(1, bytes 0))
    result.unwrap(conn.send(c, take s))
    let k = result.unwrap(conn.receive(i))
    let h = result.unwrap(task.spawn_unit(tasks, bytes 65536, take k, bounce))
    let back = result.unwrap(task.join(take h))
    result.unwrap(conn.send(c, take back))
    let last = result.unwrap(conn.receive(i))
    result.unwrap(stdout.write(out, "secret"))
    Ok(())
}
"#;

/// An arriving `Secret` replays as its designation with no bytes behind it,
/// numbered where it arrives (docs/recording.md, "Handle snapshots"). Its
/// snapshot is a handle part (1), tag 7, the receiving ordinal, then the
/// designation: `Sealed` (1), stream `u32`, the seal's ordinal, and the
/// policy text. By hand from recording.md: the root's grants are `Tasks` (1)
/// and `Stdout` (2), its connection 3 and client 4; the item it takes from
/// itself arrives as 5, the helper handle is 6 and the returned secret 7. The
/// helper's entry secret is its 1. Each case moves one designation to stream
/// 99, so the unit's next use of that secret, which writes its designation,
/// is the one line, `Diverged(Arg)`: a seal and `conn.new` write no frame,
/// so the root's frames are its two `GRANT`s, the send (3), the item's
/// `TAKE` (4), the spawn (5), the end's `TAKE` (6) and the second send (7),
/// and the helper's first frame is its send (1).
#[test]
#[ignore = "explicit native integration target"]
fn u5_secrets_cross_in_items_entries_and_results() {
    crate::openat2_required::require();

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let mut request = source_request(U5_SECRETS, dir.path());
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
        options.grant_mask = ["Tasks", "Stdout"]
            .iter()
            .fold(0, |m, g| m | 1 << grant_kind(g).unwrap());
        options.cas = dir.path().join(format!("u5s-store-{backend:?}"));
        options.output = dir.path().join(format!("u5s-{backend:?}"));
        let program = product.build(&entry, &options).unwrap().executable;
        let path = dir.path().join(format!("u5s-{backend:?}.tape"));
        let (code, out, err) = tape_run(&program, &[], Some(&path));
        assert_eq!((code, out.as_str()), (Some(0), "secret"), "{backend:?}: {err}");
        let frames = unit_streams(&path, &format!("{backend:?}"), &["root", "root.0"]);
        let (code, out, err) = u4_replay(&program, &path, None);
        assert_eq!(
            (code, out.as_str(), err.as_str()),
            (Some(0), "", ""),
            "{backend:?}: {frames:#?}"
        );
        let tape = std::fs::read(&path).unwrap();
        let tape_id = hirsel_tape_format::Index::open(|at: u64, b: &mut [u8]| {
            std::os::unix::fs::FileExt::read_at(&std::fs::File::open(&path).unwrap(), b, at)
        })
        .unwrap()
        .1
        .tape_id;
        let one_line = |name: &str, stream: u32, ordinal: u8, expected: &str| {
            // The snapshot's head and its designation's stream, which moves to 99.
            let window = [1, 7, ordinal, 1, 0, 0, 0, 0];
            let wrong = u4_patched(&tape, &tape_id, stream, |payload| {
                let Some(i) = payload.windows(8).position(|w| w == window) else {
                    return false;
                };
                payload[i + 4] = 99;
                true
            });
            let corrupt = dir.path().join(format!("u5s-{backend:?}-{name}.tape"));
            std::fs::write(&corrupt, wrong).unwrap();
            let (code, out, err) = u4_replay(&program, &corrupt, None);
            assert_eq!((code, out.as_str()), (Some(1), ""), "{backend:?} {name}: {err}");
            let lines: Vec<_> = err.lines().collect();
            assert!(
                matches!(&lines[..], [l] if l.starts_with(expected)),
                "{backend:?} {name}: {err}"
            );
        };
        one_line("item", 0, 5, "replay: Diverged(Arg) at 5 in root ");
        one_line("entry", 1, 1, "replay: Diverged(Arg) at 1 in root.0 ");
        one_line("result", 0, 7, "replay: Diverged(Arg) at 7 in root ");
    }
}

/// A virtual `KeyShare` crosses in an item, a helper's entry and its result,
/// comes back to the root that paid for it, and then agrees; a second share
/// the root makes after the crossings agrees too.
const U5_KEY_SHARES: &str = r#"
fn bounce(take k: secret.KeyShare) -> secret.KeyShare waits {
    let (c, i): (Client[secret.KeyShare], Inbox[secret.KeyShare]) = result.unwrap(conn.new(1, bytes 0))
    result.unwrap(conn.send(c, take k))
    result.unwrap(conn.receive(i))
}

pub fn main(tasks: Tasks, out: Stdout) -> Result[(), Exit] waits {
    let mut point: Bytes = list.with_capacity(32)
    let s = result.unwrap(secret.key_share(provide.random(11), X25519, mut point))
    let (c, i): (Client[secret.KeyShare], Inbox[secret.KeyShare]) = result.unwrap(conn.new(1, bytes 0))
    result.unwrap(conn.send(c, take s))
    let k = result.unwrap(conn.receive(i))
    let h = result.unwrap(task.spawn_unit(tasks, bytes 65536, take k, bounce))
    let back = result.unwrap(task.join(take h))
    result.unwrap(conn.send(c, take back))
    let last = result.unwrap(conn.receive(i))
    let mut again: Bytes = list.with_capacity(32)
    let fresh = result.unwrap(secret.key_share(provide.random(12), X25519, mut again))
    let profile: schedule.Profile13 = Quic(Aes128GcmSha256)
    let hash = string.as_bytes("0123456789abcdef0123456789abcdef")
    let crossed = result.unwrap(schedule.tls13(profile, Client, take last, hash, hash))
    let made = result.unwrap(schedule.tls13(profile, Server, take fresh, hash, hash))
    drop(take crossed)
    drop(take made)
    result.unwrap(stdout.write(out, "share"))
    Ok(())
}
"#;

/// An arriving `KeyShare` replays as its snapshot with no key and no charge,
/// numbered where it arrives; its agreement takes the recorded verdict, and
/// it takes no number among the run's shares, so the share the root makes
/// after the crossings still agrees as its 1 (docs/recording.md,
/// "Snapshots"). A virtual share's snapshot is a handle part (1), tag 8, the
/// receiving ordinal, then its media origin: the root's stream `u32` 0,
/// delivery 0 and its first tracked charge 0, then `X25519` (0). The
/// ordinals and frames are those of `u5_secrets_cross_in_items_entries_and_results`:
/// a virtual share writes no frame. Each case moves one origin's stream to
/// 99, so the next frame that writes that snapshot is the one line.
#[test]
#[ignore = "explicit native integration target"]
fn u5_key_shares_cross_and_agree() {
    crate::openat2_required::require();

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let mut request = source_request(U5_KEY_SHARES, dir.path());
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
        options.grant_mask = ["Tasks", "Stdout"]
            .iter()
            .fold(0, |m, g| m | 1 << grant_kind(g).unwrap());
        options.cas = dir.path().join(format!("u5k-store-{backend:?}"));
        options.output = dir.path().join(format!("u5k-{backend:?}"));
        let program = product.build(&entry, &options).unwrap().executable;
        let path = dir.path().join(format!("u5k-{backend:?}.tape"));
        let (code, out, err) = tape_run(&program, &[], Some(&path));
        assert_eq!((code, out.as_str()), (Some(0), "share"), "{backend:?}: {err}");
        let frames = unit_streams(&path, &format!("{backend:?}"), &["root", "root.0"]);
        let (code, out, err) = u4_replay(&program, &path, None);
        assert_eq!(
            (code, out.as_str(), err.as_str()),
            (Some(0), "", ""),
            "{backend:?}: {frames:#?}"
        );
        let tape = std::fs::read(&path).unwrap();
        let tape_id = hirsel_tape_format::Index::open(|at: u64, b: &mut [u8]| {
            std::os::unix::fs::FileExt::read_at(&std::fs::File::open(&path).unwrap(), b, at)
        })
        .unwrap()
        .1
        .tape_id;
        let one_line = |name: &str, stream: u32, ordinal: u8, expected: &str| {
            let window = [1, 8, ordinal, 0, 0, 0, 0, 0, 0, 0];
            let wrong = u4_patched(&tape, &tape_id, stream, |payload| {
                let Some(i) = payload.windows(10).position(|w| w == window) else {
                    return false;
                };
                payload[i + 3] = 99;
                true
            });
            let corrupt = dir.path().join(format!("u5k-{backend:?}-{name}.tape"));
            std::fs::write(&corrupt, wrong).unwrap();
            let (code, out, err) = u4_replay(&program, &corrupt, None);
            assert_eq!((code, out.as_str()), (Some(1), ""), "{backend:?} {name}: {err}");
            let lines: Vec<_> = err.lines().collect();
            assert!(
                matches!(&lines[..], [l] if l.starts_with(expected)),
                "{backend:?} {name}: {err}"
            );
        };
        one_line("item", 0, 5, "replay: Diverged(Arg) at 5 in root ");
        one_line("entry", 1, 1, "replay: Diverged(Arg) at 1 in root.0 ");
        one_line("result", 0, 7, "replay: Diverged(Arg) at 7 in root ");
    }
}

/// Nested helpers exchange a `Secret` and a `KeyShare` as items on their own
/// connections and in their entries and results; both come back to the root
/// that paid for them, which agrees with the share.
const U5_WORKERS: &str = r#"
fn inner(take k: (Secret, secret.KeyShare)) -> (Secret, secret.KeyShare) waits {
    let (c, i): (Client[(Secret, secret.KeyShare)], Inbox[(Secret, secret.KeyShare)]) = result.unwrap(conn.new(1, bytes 0))
    result.unwrap(conn.send(c, take k))
    result.unwrap(conn.receive(i))
}

fn outer(take input: (Tasks, (Secret, secret.KeyShare))) -> (Secret, secret.KeyShare) waits {
    let (take tasks, take k) = take input
    let (c, i): (Client[(Secret, secret.KeyShare)], Inbox[(Secret, secret.KeyShare)]) = result.unwrap(conn.new(1, bytes 0))
    result.unwrap(conn.send(c, take k))
    let held = result.unwrap(conn.receive(i))
    let h = result.unwrap(task.spawn_unit(tasks, bytes 65536, take held, inner))
    result.unwrap(task.join(take h))
}

pub fn main(tasks: Tasks, out: Stdout) -> Result[(), Exit] waits {
    let mut b: Bytes = list.with_capacity(1)
    list.push(mut b, 5)
    let s = result.unwrap(secret.seal(take b, "mac"))
    let mut point: Bytes = list.with_capacity(32)
    let share = result.unwrap(secret.key_share(provide.random(11), X25519, mut point))
    let h = result.unwrap(task.spawn_unit(tasks, bytes 262144, (tasks, (take s, take share)), outer))
    let back = result.unwrap(task.join(take h))
    let (c, i): (Client[(Secret, secret.KeyShare)], Inbox[(Secret, secret.KeyShare)]) = result.unwrap(conn.new(1, bytes 0))
    result.unwrap(conn.send(c, take back))
    let (take kept, take last) = result.unwrap(conn.receive(i))
    let profile: schedule.Profile13 = Quic(Aes128GcmSha256)
    let hash = string.as_bytes("0123456789abcdef0123456789abcdef")
    let keys = result.unwrap(schedule.tls13(profile, Client, take last, hash, hash))
    drop(take keys)
    drop(take kept)
    result.unwrap(stdout.write(out, "workers"))
    Ok(())
}
"#;

/// Recording and replay run on `HIRSEL_UNTIED` workers as an ordinary run
/// does (CONC-15: worker-count independence, docs/recording.md). A run
/// recorded at 1 worker replays `Sealed` at 4, and one recorded at 4
/// replays `Sealed` at 1, each with the recorded output and the same three
/// streams (`root`, `root.0`, `root.0.0`): which worker runs a unit is not a
/// recorded observation.
#[test]
#[ignore = "explicit native integration target"]
fn u5_replays_at_another_worker_count() {
    crate::openat2_required::require();

    let run = |program: &Path, argv: &[&str], record: Option<&Path>, workers: &str| {
        // A run that hangs on the pool ends 124 and fails with its case.
        let mut cmd = std::process::Command::new("timeout");
        cmd.arg("120").arg(program).args(argv).env("HIRSEL_UNTIED", workers);
        match record {
            Some(path) => cmd.env("HIRSEL_RECORD", path),
            None => cmd.env_remove("HIRSEL_RECORD"),
        };
        let output = cmd.output().unwrap();
        (
            output.status.code(),
            String::from_utf8(output.stdout).unwrap(),
            String::from_utf8(output.stderr).unwrap(),
        )
    };
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let mut request = source_request(U5_WORKERS, dir.path());
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
        options.grant_mask = ["Tasks", "Stdout"]
            .iter()
            .fold(0, |m, g| m | 1 << grant_kind(g).unwrap());
        options.cas = dir.path().join(format!("u5w-store-{backend:?}"));
        options.output = dir.path().join(format!("u5w-{backend:?}"));
        let program = product.build(&entry, &options).unwrap().executable;
        for (recorded, replayed) in [("1", "4"), ("4", "1")] {
            let at = format!("{backend:?} recorded at {recorded}, replayed at {replayed}");
            let path = dir.path().join(format!("u5w-{backend:?}-{recorded}.tape"));
            let (code, out, err) = run(&program, &[], Some(&path), recorded);
            assert_eq!((code, out.as_str()), (Some(0), "workers"), "{at}: {err}");
            let frames = unit_streams(&path, &at, &["root", "root.0", "root.0.0"]);
            let tape = path.to_str().unwrap();
            let argv = ["--hirsel-replay-recording", "same", tape];
            let (code, out, err) = run(&program, &argv, None, replayed);
            assert_eq!(
                (code, out.as_str(), err.as_str()),
                (Some(0), "", ""),
                "{at}: {frames:#?}"
            );
        }
        // A helper whose replay diverges ends there while the others run to
        // their verdicts on the one worker: its entry's `Secret` designation
        // moves to stream 99, so its send (its frame 1) is the one line.
        let path = dir.path().join(format!("u5w-{backend:?}-1.tape"));
        let tape = std::fs::read(&path).unwrap();
        let tape_id = hirsel_tape_format::Index::open(|at: u64, b: &mut [u8]| {
            std::os::unix::fs::FileExt::read_at(&std::fs::File::open(&path).unwrap(), b, at)
        })
        .unwrap()
        .1
        .tape_id;
        let wrong = u4_patched(&tape, &tape_id, 1, |payload| {
            let Some(i) = payload
                .windows(8)
                .position(|w| w[..2] == [1, 7] && w[3..] == [1, 0, 0, 0, 0])
            else {
                return false;
            };
            payload[i + 4] = 99;
            true
        });
        let corrupt = dir.path().join(format!("u5w-{backend:?}-diverged.tape"));
        std::fs::write(&corrupt, wrong).unwrap();
        let argv = ["--hirsel-replay-recording", "same", corrupt.to_str().unwrap()];
        let (code, out, err) = run(&program, &argv, None, "1");
        assert_eq!((code, out.as_str()), (Some(1), ""), "{backend:?} diverged: {err}");
        let lines: Vec<_> = err.lines().collect();
        assert!(
            matches!(&lines[..], [l] if l.starts_with("replay: Diverged(Arg) at 1 in root.0 ")),
            "{backend:?} diverged: {err}"
        );
    }
}
