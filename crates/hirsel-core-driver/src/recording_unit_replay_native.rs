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
    for backend in [Backend::Cranelift, Backend::Llvm] {
        let mut options = native_options(backend, dir.path());
        options.runtime = env_record_runtime();
        options.recording = true;
        options.grant_mask = ["Tasks", "Stdout"]
            .iter()
            .fold(0, |m, g| m | 1 << grant_kind(g).unwrap());
        options.cas = dir.path().join(format!("u4d-store-{backend:?}"));
        options.output = dir.path().join(format!("u4d-{backend:?}"));
        let program = product.build(&entry, &options).unwrap().executable;
        let path = dir.path().join(format!("u4d-{backend:?}.tape"));
        let (code, out, err) = tape_run(&program, &[], Some(&path));
        assert_eq!((code, out.as_str()), (Some(0), "units"), "{backend:?}: {err}");
        let tape = std::fs::read(&path).unwrap();
        let file = std::fs::File::open(&path).unwrap();
        let read = |at: u64, b: &mut [u8]| file.read_at(b, at);
        let (mut index, header) = Index::open(read).unwrap();
        let paths: std::collections::BTreeMap<String, u32> = index.paths().map(|(s, n)| (n, s)).collect();
        let names: Vec<_> = paths.keys().map(String::as_str).collect();
        assert_eq!(names, ["root", "root.0", "root.0.0", "root.1"], "{backend:?}");

        for unit in [None, Some("root"), Some("root.0"), Some("root.0.0"), Some("root.1")] {
            let (code, out, err) = u4_replay(&program, &path, unit);
            assert_eq!((code, out.as_str(), err.as_str()), (Some(0), "", ""), "{backend:?} {unit:?}");
        }

        // (c) inner's result, 21, taken by root.0 under ordinal 99.
        let wrong = u4_patched(&tape, &header.tape_id, paths["root.0"], u4_take_to_99(&[21]));
        let corrupt = dir.path().join(format!("u4d-{backend:?}-take.tape"));
        std::fs::write(&corrupt, &wrong).unwrap();
        for unit in [None, Some("root.0")] {
            let (code, out, err) = u4_replay(&program, &corrupt, unit);
            assert_eq!((code, out.as_str()), (Some(1), ""), "{backend:?} {unit:?}: {err}");
            let lines: Vec<_> = err.lines().collect();
            assert!(
                matches!(&lines[..], [l] if l.starts_with("replay: Diverged(Take) at 2 in root.0 ")
                    && l.contains("no helper with ordinal 99")),
                "{backend:?} {unit:?}: {err}"
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
        let unresolved = dir.path().join(format!("u4d-{backend:?}-entry.tape"));
        std::fs::write(&unresolved, &unknown).unwrap();
        let (code, out, err) = u4_replay(&program, &unresolved, None);
        assert_eq!((code, out.as_str()), (Some(1), ""), "{backend:?}: {err}");
        let lines: Vec<_> = err.lines().collect();
        assert!(
            matches!(&lines[..], [l] if l.starts_with("replay: Diverged(Op) at 0 in root.1 ")),
            "{backend:?}: {err}"
        );
    }
}
