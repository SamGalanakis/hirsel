// FAIL-17 on the native engines, over the laws of the case corpus
// (core/front/cases/rules/FAIL-17-stack.hirsel, reference runs on the evaluator): every
// law holds in main on a task stack, and a 10 MiB
// chain of non-recursive frames fails a spawned task with `Stack` (its join is an
// error) or is caught by an isolate inside it, and an uncaught `Stack` in main ends
// the run with status 101 and a `Stack at` cause line (PROG-3). The walk laws compare,
// hash and clone values a million levels deep. Oracle: FAIL-17.
const FAIL17_TESTS: &str = r#"
@test
fn fiber_caught(tasks: Tasks) -> Result[(), Exit] waits {
    let h = task.spawn(tasks, bytes 65536, 0u64, fn(take n: u64) -> u64 { chain_caught() + n })
    match task.join(take h) {
        Ok(v) => if v == 1u64 { Ok(()) } else { Error(Exit(status: 2)) },
        Error(_) => Error(Exit(status: 3)),
    }
}
@test
fn fiber_uncaught(tasks: Tasks) -> Result[(), Exit] waits {
    let h = task.spawn(tasks, bytes 65536, 0u64, fn(take n: u64) -> u64 { c01(n) })
    match task.join(take h) {
        Ok(_) => Error(Exit(status: 2)),
        Error(_) => if recursion_caught() == 1u64 { Ok(()) } else { Error(Exit(status: 4)) },
    }
}
@test
fn main_uncaught() -> Result[(), ()] { let _ = c01(0u64); Ok(()) }
@test
fn walk_laws() -> Result[(), Exit] {
    if tail_walks(1000000u64) != 1u64 { return Error(Exit(status: 20)) }
    if head_walks(1000000u64) != 1u64 { return Error(Exit(status: 21)) }
    if kids_walk(300000u64) != 1u64 { return Error(Exit(status: 22)) }
    if grid_walk(150000u64) != 1u64 { return Error(Exit(status: 23)) }
    if tail_clones(1000000u64) != 1u64 { return Error(Exit(status: 24)) }
    if head_clones(1000000u64) != 1u64 { return Error(Exit(status: 25)) }
    if kids_clones(300000u64) != 1u64 { return Error(Exit(status: 26)) }
    if grid_clones(150000u64) != 1u64 { return Error(Exit(status: 27)) }
    if fork_clones(1000000u64) != 1u64 { return Error(Exit(status: 28)) }
    if brood_clones(300000u64) != 1u64 { return Error(Exit(status: 29)) }
    Ok(())
}
@test
fn main_laws() -> Result[(), Exit] {
    if recursion_caught() != 1u64 { return Error(Exit(status: 10)) }
    if steps_pass_it() != 1u64 { return Error(Exit(status: 11)) }
    if bytes_pass_it() != 1u64 { return Error(Exit(status: 12)) }
    if chain_caught() != 1u64 { return Error(Exit(status: 13)) }
    if finalizer_within_margin() != 1u64 { return Error(Exit(status: 14)) }
    if continues() != 1u64 { return Error(Exit(status: 15)) }
    Ok(())
}
"#;

fn fail17_stack_laws(backend: Backend) {
    let dir = tempfile::tempdir().unwrap();
    let laws = include_str!("../../../core/front/cases/rules/FAIL-17-stack.hirsel");
    // The corpus's refused frame is the build-time law; these runs exclude it.
    let laws = &laws[..laws.find("// An 8 MiB frame").unwrap()];
    let product = Driver::default()
        .check(
            prog4_request(&format!("{laws}{FAIL17_TESTS}"), dir.path()),
            &mut Meter::new(false),
        )
        .unwrap();
    assert!(
        product.accepted(),
        "{:?} {:?}",
        product.diagnostics,
        product.incomplete
    );
    let test = |name: &str| DefPath::from_full(&format!("app@1/main/{name}")).unwrap();
    let mut options = native_options(backend, dir.path());
    options.grant_mask = 1 << grant_kind("Tasks").unwrap();
    for optimize in [false, true] {
        options.optimize = optimize;
        // The walk laws hold two values a million levels deep at once.
        for (name, code, limit) in [
            ("main_laws", 0, 1 << 20),
            ("walk_laws", 0, 1 << 28),
            ("fiber_caught", 0, 1 << 20),
            ("fiber_uncaught", 0, 1 << 20),
            ("main_uncaught", 101, 1 << 20),
        ] {
            options.root_limit = limit;
            options.output = dir.path().join(format!("{name}-{optimize}"));
            let outcome = product.run_test(&test(name), &options).unwrap();
            let stderr = String::from_utf8_lossy(&outcome.output.stderr);
            assert_eq!(
                outcome.output.status.code(),
                Some(code),
                "{name} optimize {optimize}: {stderr}"
            );
            if code == 101 {
                assert_eq!(stderr.lines().count(), 1, "{name}: {stderr}");
                assert!(
                    stderr.starts_with("Stack at app@1/main.hirsel:"),
                    "{name}: {stderr}"
                );
            }
        }
    }
}
