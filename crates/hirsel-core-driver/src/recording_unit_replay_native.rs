// Replay of a run with helpers (docs/recording.md, unit tapes U4b): a
// replayed spawn starts nothing, and each helper end comes from the root's
// own `TAKE` of it. Included from recording_units_native.rs.

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
/// - The intact tape's full replay is refused, exit 2: this build replays no
///   helper stream.
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
    use hirsel_tape_format::{BLOCK_CHECK, BLOCK_HEAD, Frame, Index, op};
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

        // The intact tape: refused, since root.0 and root.1 are not replayed.
        let (code, out, err) = u4_replay(&program, &path, None);
        assert_eq!((code, out.as_str()), (Some(2), ""), "{backend:?}: {err}");
        assert!(err.starts_with("replay: refused: "), "{backend:?}: {err}");

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

        // (b) The first root TAKE names ordinal 99: arrival tag 4, the
        // ordinal, Returned (0), then the u64 value's 8 plain bytes and no
        // parts or survivors.
        let mut wrong = Vec::new();
        let mut patched = false;
        for block in &blocks {
            let seq = u64::from_le_bytes(block[8..16].try_into().unwrap());
            let at = (!patched && u4_stream(block) == 0 && seq > 0)
                .then(|| {
                    block.windows(14).position(|w| {
                        w[0] == 4
                            && w[2..4] == [0, 8]
                            && (w[4] == 42 || w[4] == 5)
                            && w[5..14].iter().all(|&b| b == 0)
                    })
                })
                .flatten();
            let Some(at) = at else {
                wrong.extend_from_slice(block);
                continue;
            };
            patched = true;
            let mut bytes = block[..block.len() - BLOCK_CHECK].to_vec();
            assert!(bytes[at + 1] < 99, "{backend:?}");
            bytes[at + 1] = 99;
            hirsel_tape_format::seal(&header.tape_id, block[0], 0, seq, &mut bytes);
            assert_eq!(bytes.len(), block.len(), "{backend:?}");
            assert!(at >= BLOCK_HEAD, "{backend:?}");
            wrong.extend_from_slice(&bytes);
        }
        assert!(patched, "{backend:?}: a root TAKE");
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
