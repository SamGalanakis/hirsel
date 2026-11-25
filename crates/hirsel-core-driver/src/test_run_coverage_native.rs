// Coverage laws across returnable waits, run from the prog4 LLVM harness
// (`test_run_native.rs`).
const KNOWN_WAITS: &str = r#"
type Job { tasks: Tasks, n: u64 }
fn leaf(take n: u64) -> u64 { n + 10u64 }
fn worker(take job: Job) -> u64 waits {
    let mut out = 0u64
    if job.n == 9u64 { out = 1u64 }
    let h = task.spawn(job.tasks, bytes 16384, job.n, leaf)
    let got = result.unwrap(task.join(take h))
    if got > 5u64 { out + got } else { out + got + 100u64 }
}
@test
fn waits(tasks: Tasks) -> Result[(), ()] waits {
    let h = task.spawn(tasks, bytes 262144, Job(tasks: tasks, n: 2u64), worker)
    if result.unwrap(task.join(take h)) == 12u64 { Ok(()) } else { Error(()) }
}
"#;

/// Known arms across an owned wait: `worker` is a spawned task's callee that
/// joins a child, so a release build gives it a returnable variant whose
/// blocks are not its source's (a new block 0, the entry moved, the block
/// with the join split). Its arms by hand: `n == 9` only `no` (n is 2),
/// `got > 5` only `yes` (got is 12), and `waits`'s `== 12u64` only `yes`.
fn coverage_across_waits(backend: Backend, optimize: bool) -> BTreeMap<String, bool> {
    let dir = tempfile::tempdir().unwrap();
    let product = Driver::default().check(prog4_request(KNOWN_WAITS, dir.path()), &mut Meter::new(false)).unwrap();
    assert!(product.accepted(), "{:?} {:?}", product.diagnostics, product.incomplete);
    let test = DefPath::from_full("app@1/main/waits").unwrap();
    let mut options = NativeOptions { optimize, coverage: Some([package("app")].into()), ..native_options(backend, dir.path()) };
    options.grant_mask = 1 << crate::native::grant_kind("Tasks").unwrap();
    options.root_limit = 1 << 22;
    let outcome = product.run_test(&test, &options).unwrap_or_else(|stop| panic!("{backend:?} {optimize}: {stop}"));
    assert!(outcome.passed(), "{outcome:?}");
    let reached: BTreeMap<_, _> = coverage_reached(&outcome.coverage).into_iter().filter(|(key, _)| key.starts_with("app@1/")).collect();
    let mut expected = BTreeMap::new();
    // An item starts at its attribute; the task entry adapter of each
    // function that spawns has an entry site.
    let arms = [("leaf", "fn leaf(", &[][..]), ("worker", "fn worker(", &[("job.n == 9u64", false, true), ("got > 5u64", true, false)]), ("waits", "@test", &[("result.unwrap(task.join(take h)) == 12u64", true, false)])];
    for (name, start, arms) in arms {
        let item = &KNOWN_WAITS[KNOWN_WAITS.find(start).unwrap()..];
        expected.insert(format!("app@1/main/{name}#e"), true);
        if name != "leaf" {
            expected.insert(format!("app@1/main/{name}$task_entry0#e"), true);
        }
        for &(text, yes, no) in arms {
            let start = item.find(text).unwrap();
            let key = format!("app@1/main/{name}@{start}-{}.0:", start + text.len());
            expected.extend([(format!("{key}yes"), yes), (format!("{key}no"), no)]);
        }
    }
    assert_eq!(reached, expected, "{backend:?} {optimize}");
    reached
}

/// Engines agree across a returnable wait: Cranelift's reached sites are the
/// oracle for LLVM dev and release.
fn engines_agree_across_waits() {
    let cranelift = coverage_across_waits(Backend::Cranelift, false);
    assert_eq!(coverage_across_waits(Backend::Llvm, false), cranelift);
    assert_eq!(coverage_across_waits(Backend::Llvm, true), cranelift);
}

const CORE_LEAVES: &str = r#"
@test
fn leaves() -> Result[(), ()] {
    let s = string.slice("hex 7f", 4u64, 6u64)
    let b = string.as_bytes(s)
    let mut m: Map[u64, u64] = map.new()
    map.insert(mut m, 5u64, string.byte_len(s))
    match (int.ascii_digit(b[0], 16u32), map.get(m, 5u64)) {
        (Some(d), Some(n)) => if d == 7u8 && n == 2u64 { Ok(()) } else { Error(()) },
        _ => Error(()),
    }
}
"#;

/// The reached sites of `app` and `core` for CORE_LEAVES, which calls two
/// small plain core bodies, `int.ascii_digit` and `string.slice`, and the
/// generic `map.insert` and `map.get` of one key.
fn core_leaves_coverage(backend: Backend, optimize: bool) -> BTreeMap<String, bool> {
    let dir = tempfile::tempdir().unwrap();
    let product = Driver::default().check(prog4_request(CORE_LEAVES, dir.path()), &mut Meter::new(false)).unwrap();
    assert!(product.accepted(), "{:?} {:?}", product.diagnostics, product.incomplete);
    let test = DefPath::from_full("app@1/main/leaves").unwrap();
    let options = NativeOptions { optimize, coverage: Some([package("app"), package("core")].into()), ..native_options(backend, dir.path()) };
    let outcome = product.run_test(&test, &options).unwrap_or_else(|stop| panic!("{backend:?} {optimize}: {stop}"));
    assert!(outcome.passed(), "{outcome:?}");
    coverage_reached(&outcome.coverage)
}

/// Engines agree on plain core bodies: LLVM release keeps every site of
/// `core@1/int/ascii_digit` and `core@1/string/slice`, and reaches the
/// sites Cranelift reaches.
fn engines_agree_on_core_leaves() {
    let cranelift = core_leaves_coverage(Backend::Cranelift, false);
    for key in ["core@1/int/ascii_digit#e", "core@1/string/slice#e", "core@1/map/get#e"] {
        assert_eq!(cranelift.get(key), Some(&true), "{key}");
    }
    let release = core_leaves_coverage(Backend::Llvm, true);
    let differ: Vec<_> = cranelift.keys().chain(release.keys()).filter(|k| cranelift.get(*k) != release.get(*k)).collect();
    assert!(differ.is_empty(), "LLVM release differs from Cranelift at {differ:?}");
}


/// The Engines agree laws of release units: across returnable waits and on
/// plain and generic core bodies.
fn release_engines_agree() {
    engines_agree_across_waits();
    engines_agree_on_core_leaves();
}
