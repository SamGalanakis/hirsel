/// Two helpers whose `X` and `R` both differ, one spawned through a function
/// parameter, so its spawn site cannot know which function it starts.
const SPAWN_ENTRIES: &str = r#"
fn above(take n: u64) -> bool { n > 20 }

fn pick(take b: bool) -> u64 { if b { 7 } else { 9 } }

fn launch(tasks: Tasks, f: fn(take bool) -> u64, b: bool) -> u64 waits {
    let h = result.unwrap(task.spawn_unit(tasks, bytes 65536, b, f))
    result.unwrap(task.join(take h))
}

pub fn main(tasks: Tasks, out: Stdout) -> Result[(), Exit] waits {
    let h = result.unwrap(task.spawn_unit(tasks, bytes 65536, 21, above))
    let a = result.unwrap(task.join(take h))
    if launch(tasks, pick, a) != 7 { return Error(Exit(status: 3)) }
    result.unwrap(stdout.write(out, "entries"))
    Ok(())
}
"#;

/// The link's spawn table starts each helper as its spawn does: a recorded
/// spawn looks its function's identity up in the table and cuts its stream
/// unless the row there has the bridge, layout and both crossing
/// descriptors the spawn passed. So each helper's stream holds its entry,
/// and no stream is cut. The values are hand-derived from docs/recording.md:
/// a `u64` is 8 plain bytes and a `bool` one, each with no parts.
#[test]
#[ignore = "explicit native integration target"]
fn spawn_entries_start_each_helper_as_its_spawn_does() {
    crate::openat2_required::require();
    use hirsel_tape_format::Frame;
    let dir = tempfile::tempdir().unwrap();
    let word = |v: u8| [vec![8, v], vec![0; 7], vec![0]].concat();
    let flag = |v: u8| vec![1, v, 0];
    let expected: [(&[u8], Vec<u8>, Vec<u8>); 2] = [
        (b"\x01app@1/main/above(take u64)->bool", word(21), flag(1)),
        (b"\x01app@1/main/pick(take bool)->u64", flag(1), word(7)),
    ];
    for backend in [Backend::Cranelift, Backend::Llvm] {
        let path = rl9_record(SPAWN_ENTRIES, dir.path(), "entries", backend, "entries");
        let streams = unit_streams(&path, &format!("{backend:?}"), &["root", "root.0", "root.1"]);
        for (spawn, (descriptor, x, r)) in expected.iter().enumerate() {
            let (entry, frames) = &streams[spawn + 1];
            let entry = entry.as_ref().expect("a helper entry");
            let body = entry.body();
            assert_eq!(body.descriptor, *descriptor, "{backend:?} root.{spawn}");
            assert_eq!(body.value.bytes(), x.as_slice(), "{backend:?} root.{spawn}");
            let Some(Frame::HelperEnd { ending, .. }) = frames.last() else {
                panic!("{backend:?} root.{spawn}: {frames:?}");
            };
            // The result, then the residual: 0, as the helper had no bound.
            let r = [r.as_slice(), &[0]].concat();
            assert_eq!(ending.body(), r.as_slice(), "{backend:?} root.{spawn}");
        }
    }
}

const CROSS_FUNCTION: &str = r#"
fn old(take n: u64) -> u64 { n + 1 }

fn produce(take n: u64) -> fn(take u64) -> u64 { old }

fn consume(take f: fn(take u64) -> u64) -> u64 { f(7) }

pub fn main(tasks: Tasks, out: Stdout) -> Result[(), Exit] waits {
    let h = result.unwrap(task.spawn_unit(tasks, bytes 65536, 0, produce))
    let f = result.unwrap(task.join(take h))
    if f(7) != 8 { return Error(Exit(status: 3)) }
    let g = result.unwrap(task.spawn_unit(tasks, bytes 65536, old, consume))
    if result.unwrap(task.join(take g)) != 8 { return Error(Exit(status: 4)) }
    // Spawning `old` gives it a code descriptor, so a value can carry it.
    let o = result.unwrap(task.spawn_unit(tasks, bytes 65536, 1, old))
    if result.unwrap(task.join(take o)) != 2 { return Error(Exit(status: 5)) }
    result.unwrap(stdout.write(out, "functions"))
    Ok(())
}
"#;

/// A replaying build without `old` cannot resolve the descriptor a value
/// carries: the root's take of root.0's end is `Diverged(Take)` and
/// root.1's entry `Diverged(Op)` at 0.
#[test]
#[ignore = "explicit native integration target"]
fn missing_cross_function_diverges_at_take_and_entry() {
    crate::openat2_required::require();
    let dir = tempfile::tempdir().unwrap();
    for backend in [Backend::Cranelift, Backend::Llvm] {
        let path = rl9_record(CROSS_FUNCTION, dir.path(), "functions", backend, "functions");
        let different = rl9_record(
            &CROSS_FUNCTION.replace("old", "new"),
            dir.path(),
            "no-old-function",
            backend,
            "functions",
        );
        let program = different.with_extension("");
        for (unit, verdict) in [
            ("root", "replay: Diverged(Take)"),
            ("root.1", "replay: Diverged(Op) at 0 in root.1"),
        ] {
            let argv = ["--hirsel-replay-recording", "diagnose", path.to_str().unwrap(), unit];
            let (code, out, err) = tape_run(&program, &argv, None);
            assert_eq!((code, out.as_str()), (Some(1), ""), "{backend:?} {unit}: {err}");
            assert!(err.lines().any(|line| line.starts_with(verdict)),
                "{backend:?} {unit}: {err}");
            assert!(err.contains("function") && err.contains("descriptor"),
                "{backend:?} {unit}: {err}");
        }
    }
}
