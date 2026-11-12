//! Native integration: source programs through
//! every native stage validator, then the heap executor and linked
//! executables, each compared with the reference interpreter's observation.
//!
//! The tests marked `ignore` link for AArch64 and run under qemu or run many
//! configurations; `tools/check-native.sh` runs them.
use crate::source_std;
use hirsel_engine::{
    DefPath, DefinitionQueries, QueryEngine, RunConfiguration, SimulationConfig,
    ValidatedNativeInput, default_context, default_environment, entry_path, run_observed_snapshot,
    source_check_request,
};
use hirsel_front_phases::compiler_adapters;
use hirsel_intern::jobs::with_compiler_stack;
use hirsel_native::Observation;
use hirsel_native::Partition;
use hirsel_native::StageName;
use hirsel_native::TargetSpec;
use hirsel_native::Toolchain;
use hirsel_native_testing::Configuration;
use hirsel_native_testing::Engine;
use hirsel_native_testing::Outcome;
use hirsel_native_testing::smoke_matrix;
use hirsel_native_testing::topology_matrix;
use std::path::PathBuf;

/// A checked program with its frozen snapshot and reference observation.
struct Checked {
    validated: ValidatedNativeInput,
    entry: DefPath,
    reference: Observation,
}

fn checked_in(engine: &mut QueryEngine, source: &str) -> Checked {
    let context = default_context();
    if source.contains("std@1/") {
        let mut loaded = hirsel_front_phases::load_program_source(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/std_numeric.hir"),
            Some(source),
        )
        .unwrap();
        loaded.check_for_execution().unwrap();
        *engine = loaded.engine;
    } else {
        let file = engine.sources.add("native.hir", source.into()).unwrap();
        let output = source_check_request(engine, file, &context).unwrap();
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
    }
    checked_engine(engine)
}

fn checked_engine(engine: &mut QueryEngine) -> Checked {
    let entry = entry_path(&default_context());
    let config = RunConfiguration {
        entry: entry.clone(),
        hash_seed: 0,
        inputs: vec![],
    };
    let execution = run_observed_snapshot(engine, &config, SimulationConfig::default()).unwrap();
    let reference = Observation::of_reference(execution.observation.as_ref().unwrap());
    let input = engine.export_native_input().unwrap();
    let validated = engine.import_native_input(input).unwrap();
    Checked {
        validated,
        entry,
        reference,
    }
}

/// `source`'s Ready requests, as `checked` loads it.
fn ready(source: &str, entry: &DefPath, toolchain: &Toolchain) -> hirsel_requests::ReadyRequests {
    if source.contains("std@1/") {
        hirsel_requests::ready_supplier(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/std_numeric.hir"),
            Some(source),
            entry,
            toolchain,
        )
    } else {
        hirsel_requests::source_ready_supplier(source, "native.hir", entry, toolchain)
    }
    .unwrap()
}

fn checked(source: &str) -> Checked {
    let mut engine = if source.contains("std@1/") {
        source_std::engine()
    } else {
        QueryEngine::new(default_environment(), compiler_adapters())
    };
    checked_in(&mut engine, source)
}

fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The runtime archives, built once per test binary.
fn toolchain(configurations: &[Configuration]) -> Toolchain {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("native-runtime");
    hirsel_native_testing::toolchain_for_matrix(&dir, configurations).unwrap()
}

/// Every configuration agrees with the reference; returns the reference.
fn agree(name: &str, source: &str, configurations: &[Configuration]) -> Observation {
    let name = name.to_string();
    let configurations = configurations.to_vec();
    with_compiler_stack(move || {
        let program = checked(&source);
        let toolchain = toolchain(&configurations);
        let outcomes = hirsel_native_testing::differential_ready(
            &mut *ready(source, &program.entry, &toolchain),
            &program.entry,
            &program.reference,
            &configurations,
            &toolchain,
            &scratch(&name),
            &hirsel_native::Fixture::default(),
        );
        for (configuration, outcome) in outcomes {
            match outcome {
                Outcome::Agree => {}
                Outcome::Mismatch(actual) => panic!(
                    "{name} under {configuration}:\n{}\nreference:\n{}",
                    actual.render(),
                    program.reference.render()
                ),
                Outcome::Unavailable(error) => panic!("{name} under {configuration}: {error}"),
                Outcome::Parted { .. } => unreachable!("differential runs whole campaigns"),
            }
        }
        program.reference
    })
}

/// Output after the last newline reaches the executable's stdout (found by
/// the agree campaign: the launcher returned without flushing it).
const NO_NEWLINE: &str = "pub fn main(rt: Io, args: Args) -> Result[(), GrantError] {\n  let s = io.stdout(rt)?\n  let _ = out.print(s, \"b\\n\")\n  let _ = out.print(s, \"a\")\n  Ok(())\n}\n";

const HELLO: &str = "pub fn main(rt: Io, args: Args) -> Result[(), GrantError] {\n  let _ = out.print(io.stdout(rt)?, \"hi\\n\")\n  Ok(())\n}\n";

/// Recursion, a counted box, a match on a call's Result, `?` and an Error exit.
const CONTROL: &str = "type P { a: u64, b: box u64 }\nfn fib(n: u64) -> u64 {\n  if n < 2u64 { n } else { fib(n - 1u64) + fib(n - 2u64) }\n}\nfn half(x: u64) -> Result[u64, ()] {\n  if x % 2u64 == 0u64 { Ok(x / 2u64) } else { Error(()) }\n}\npub fn main(rt: Io, args: Args) -> Result[(), ()] {\n  let s = io.stdout(rt)? grant_error\n  let p = P(a: fib(10u64), b: box 7u64)\n  let _ = out.print(s, if p.a == 55u64 { \"fib ok\\n\" } else { \"fib bad\\n\" })\n  match half(p.a + 1u64) {\n    Ok(v) => { let _ = out.print(s, if v == 28u64 { \"half ok\\n\" } else { \"half bad\\n\" }) }\n    Error(e) => { let _ = out.print(s, \"half error\\n\") }\n  }\n  let w = half(p.a)?\n  let _ = out.print(s, \"unreachable\\n\")\n  Ok(())\n}\n\n@noalloc\nfn grant_error(refusal: GrantError) -> () uses {} { () }\n";

/// A resource whose finalizer fails after output: FAIL-3 cleanup with the
/// failure's rule and site.
const FINALIZER: &str = "resource type R { n: u64 }\ndrop(x) {\n  let _ = 10u64 / x.n\n  ()\n}\npub fn main(rt: Io, args: Args) -> Result[(), GrantError] {\n  let s = io.stdout(rt)?\n  let r = R(n: 0)\n  let _ = out.print(s, \"before drop\\n\")\n  Ok(())\n}\n";

/// `float.sum_exact` from source to native: the exact sum is 1, the naive 0.
const EXACT_SUM: &str = "pub fn main(rt: Io, args: Args) -> Result[(), GrantError] {\n  let xs: Array[f64] = [1e30f64, 1.0f64, -1e30f64]\n  let exact = std@1/f64.sum_exact(xs)\n  let _ = out.print(io.stdout(rt)?, if exact == 1.0f64 { \"exact\\n\" } else { \"inexact\\n\" })\n  Ok(())\n}\n";

/// Two instances of a generic function, a size parameter, a closure with a
/// capture and a self-tail loop: several units per program, so partitions
/// really divide it.
const GENERIC: &str = "fn keep[T: copyable](take a: T) -> T {\n  a\n}\nfn total[n: size](xs: Inline[u64, n], i: u64, acc: u64) -> u64 {\n  if i == n { acc } else { total(xs, i + 1u64, acc + xs[i]) }\n}\nfn apply(f: fn(x: u64) -> u64, x: u64) -> u64 {\n  f(x)\n}\npub fn main(rt: Io, args: Args) -> Result[(), GrantError] {\n  let s = io.stdout(rt)?\n  let forty: u64 = 40u64\n  let base: u64 = keep(take forty)\n  let yes: bool = true\n  let flag: bool = keep(take yes)\n  let xs: Inline[u64, 3] = [1u64, 2u64, 3u64]\n  let sum = total(xs, 0u64, 0u64)\n  let add = fn(x: u64) -> u64 { x + base }\n  let got = apply(add, sum)\n  let _ = out.print(s, if flag && got == 46u64 { \"generic ok\\n\" } else { \"generic bad\\n\" })\n  Ok(())\n}\n";

/// A region-placed list read through its slot: the slot holds a pointer at
/// the payload, not at a header (found by the corpus matrix: natively the
/// count was read past the payload and every valid index failed MEM-14).
const LIST_READ: &str = "pub fn main(rt: Io, args: Args) -> Result[(), GrantError] {\n  let xs: List[u8] = [7u8, 9u8]\n  let i: u64 = 1\n  let _ = out.print(io.stdout(rt)?, if xs[i] == 9u8 { \"nine\\n\" } else { \"bad\\n\" })\n  Ok(())\n}\n";

const SMOKE: &[(&str, &str)] = &[
    ("hello", HELLO),
    ("control", CONTROL),
    ("finalizer", FINALIZER),
    ("generic", GENERIC),
    ("no-newline", NO_NEWLINE),
];

#[test]
fn smoke_programs_agree_on_the_heap_executor_and_as_x86_64_executables() {
    let matrix = smoke_matrix();
    assert_eq!(agree("smoke-hello", HELLO, &matrix).output, b"hi\n");
    let control = agree("smoke-control", CONTROL, &matrix);
    assert_eq!(control.output, b"fib ok\nhalf ok\n");
    assert_eq!(control.exit_code, Some(1));
    let finalizer = agree("smoke-finalizer", FINALIZER, &matrix);
    assert_eq!(finalizer.exit_code, None);
    assert_eq!(finalizer.failures.len(), 1);
    assert_eq!(
        (
            finalizer.failures[0].rule.as_str(),
            finalizer.failures[0].line
        ),
        ("SEM-1", 3)
    );
    assert_eq!(
        agree("smoke-exact-sum", EXACT_SUM, &matrix).output,
        b"exact\n"
    );
    assert_eq!(
        agree("smoke-generic", GENERIC, &matrix).output,
        b"generic ok\n"
    );
    assert_eq!(
        agree("smoke-list-read", LIST_READ, &matrix).output,
        b"nine\n"
    );
}

/// `fn serve() -> Never { ..; serve() }` is the canonical server loop: its
/// diverging self-tail site is a loop, so a million restarts reuse one frame
/// (recursion would need well over the 8 MiB main-thread stack) and the only
/// exit is the failure edge.
const SERVE: &str = "fn serve(n: u64) -> Never {\n  let _ = 10u64 / n\n  serve(n - 1u64)\n}\npub fn main(rt: Io, args: Args) -> Result[(), GrantError] {\n  let _ = out.print(io.stdout(rt)?, \"serving\\n\")\n  serve(1000000u64)\n}\n";

use hirsel_native::OptLevel;
use hirsel_native::Options;
use hirsel_native::emit;
use hirsel_native::link;

fn x86() -> TargetSpec {
    TargetSpec::x86_64_unknown_linux_gnu()
}

/// A same-signature body edit rebuilt in a live engine gives the units and the
/// object a fresh engine gives, and the edit recomputes no other definition's
/// source checks (PROG-5's cutoff, by `recompute_count`).
#[test]
fn a_same_signature_body_edit_matches_a_fresh_build() {
    use hirsel_query_products::QueryKind;
    let source = |body: &str| {
        format!(
            "fn weight(n: u64) -> u64 {{\n  {body}\n}}\npub fn main(rt: Io, args: Args) -> Result[(), GrantError] {{\n  let _ = out.print(io.stdout(rt)?, if weight(3u64) == 6u64 {{ \"six\\n\" }} else {{ \"other\\n\" }})\n  Ok(())\n}}\n"
        )
    };
    with_compiler_stack(move || {
        let (before, after) = (source("n + n"), source("n * 2u64 + 1u64"));
        let digests = |program: &Checked, source: &str| {
            let ready = {
                let options = &Options::new(x86());
                hirsel_requests::source_ready_supplier(
                    source,
                    "native.hir",
                    &program.entry,
                    &hirsel_native_testing::ready_toolchain(&options.target),
                )
                .unwrap()(options)
            }
            .unwrap();
            let ready = &ready.program;
            let units: Vec<_> = ready.units.iter().map(|u| u.unit().digest()).collect();
            let object = emit(&ready, OptLevel::Speed, |_| true).unwrap();
            (units, object.digest())
        };
        let mut engine = QueryEngine::new(default_environment(), compiler_adapters());
        let first = checked_in(&mut engine, &before);
        assert_eq!(first.reference.output, b"six\n");
        let old = digests(&first, &before);
        let main = first.entry.clone();
        let mut callee = main.clone();
        callee.0.name = "weight".into();
        let callee = hirsel_syntax::DefId::intern(&callee);
        let reads = |engine: &QueryEngine| {
            [QueryKind::Check, QueryKind::Modes, QueryKind::Lower].map(|kind| {
                let reads = engine.memo_reads(&main, kind).expect("main has a memo");
                assert!(
                    !reads.iter().any(|(key, _)| {
                        *key == hirsel_query_products::reads::ReadKey::ExecutedBody(callee)
                    }),
                    "{kind:?} must not depend on the callee body"
                );
                reads.clone()
            })
        };
        let main_before = reads(&engine);
        let edited = checked_in(&mut engine, &after);
        // Debug carry audits can recompute a memo. Its semantic reads stay fixed.
        assert_eq!(reads(&engine), main_before);
        assert_eq!(edited.reference.output, b"other\n");
        let fresh = checked(&after);
        assert_eq!(edited.reference, fresh.reference);
        let (rebuilt, clean) = (digests(&edited, &after), digests(&fresh, &after));
        assert_eq!(rebuilt, clean);
        assert_ne!(rebuilt, old);
        // The rebuilt program also runs as the fresh one does.
        let matrix = smoke_matrix();
        let toolchain = toolchain(&matrix);
        let dir = scratch("body-edit");
        for program in [&edited, &fresh] {
            for (configuration, outcome) in hirsel_native_testing::differential_ready(
                &mut *hirsel_requests::source_ready_supplier(
                    &after,
                    "native.hir",
                    &program.entry,
                    &toolchain,
                )
                .unwrap(),
                &program.entry,
                &fresh.reference,
                &matrix,
                &toolchain,
                &dir,
                &hirsel_native::Fixture::default(),
            ) {
                assert_eq!(outcome, Outcome::Agree, "{configuration}");
            }
        }
    });
}

/// A hot loop calling a small helper in another unit.
const SMALL_HELPER: &str = "fn step(h: u64, value: u64) -> u64 {\n  let wide: u128 = int.widen(h) * 131u128 + int.widen(value)\n  int.truncate(wide)\n}\npub fn main(rt: Io, args: Args) -> Result[(), GrantError] {\n  let mut h: u64 = 0\n  for i in 0u64..1000u64 { h = step(h, i) }\n  let _ = out.print(io.stdout(rt)?, if h == 16055195744720194772u64 { \"mixed\\n\" } else { \"other\\n\" })\n  Ok(())\n}\n";

/// A body with a root parameter, its caller in another object: `grow` succeeds
/// once and fails once after pushing, and the caller reads the list back each
/// time.
const ROOTS_ACROSS_OBJECTS: &str = "fn grow(mut rows: List[u64], n: u64) -> Result[(), ()] {\n  for i in 0u64..n {\n    list.push(mut rows, i)\n  }\n  if n > 30u64 { Error(()) } else { Ok(()) }\n}\npub fn main(rt: Io, args: Args) -> Result[(), GrantError] {\n  let mut rows: List[u64] = []\n  let first = match grow(mut rows, 20u64) {\n    Ok(_) => true,\n    Error(_) => false,\n  }\n  let second = match grow(mut rows, 40u64) {\n    Ok(_) => false,\n    Error(_) => true,\n  }\n  let ok = first && second && list.len(rows) == 60 && rows[19] == 19 && rows[59] == 39\n  let _ = out.print(io.stdout(rt)?, if ok { \"ok\\n\" } else { \"bad\\n\" })\n  Ok(())\n}\n";

/// Every direct call of a body with roots enters the owner's value entry, a
/// source body's included (784 L1): one object per unit at O0 imports
/// `grow.roots9` itself, not its slot form, and the roots the callee grew are
/// read back on success and failure on the interpreter, the heap checker and
/// native code as one object, one per unit and seeded groups.
#[test]
fn a_rooted_source_callee_in_another_object_returns_its_roots_on_both_outcomes() {
    use hirsel_native::OptLevel;
    use hirsel_native::Options;
    use hirsel_native::partition_objects;
    use hirsel_native_testing::Runtime;
    let mut matrix = topology_matrix(2);
    matrix.push(Configuration {
        options: Options::new(x86()),
        engine: Engine::Heap { campaign: true },
    });
    matrix.push(Configuration {
        options: Options::new(x86()),
        engine: Engine::Native {
            opt: OptLevel::None,
            partition: Partition::Singleton,
            runtime: Runtime::Diagnostic,
        },
    });
    let observed = agree("roots-across-objects", ROOTS_ACROSS_OBJECTS, &matrix);
    assert_eq!(observed.output, b"ok\n");
    with_compiler_stack(|| {
        let program = checked(ROOTS_ACROSS_OBJECTS);
        let ready = {
            let options = &Options::new(x86());
            ready(
                ROOTS_ACROSS_OBJECTS,
                &program.entry,
                &hirsel_native_testing::ready_toolchain(&options.target),
            )(options)
        }
        .unwrap();
        let ready = &ready.program;
        let registry = hirsel_lir::descriptor_registry::DescriptorRegistry::new(
            &ready.table,
            &ready.finalizers,
        );
        let mut grow = program.entry.clone();
        grow.0.name = "grow".into();
        let objects = partition_objects(
            &ready,
            OptLevel::None,
            Partition::Singleton,
            &registry,
            false,
            None,
        )
        .unwrap();
        let caller = objects
            .iter()
            .find(|o| {
                o.exports
                    .iter()
                    .any(|s| s.definition.as_ref() == Some(&program.entry))
            })
            .unwrap();
        let imported: Vec<_> = caller
            .imports
            .iter()
            .filter(|s| s.definition.as_ref() == Some(&grow))
            .map(|s| s.name.as_str())
            .collect();
        assert!(
            matches!(imported.as_slice(), [name] if name.ends_with(".roots9")),
            "{imported:?}"
        );
    });
}

/// A failing index in statement position fails at the index expression, by
/// line and column, in all three engines (the diagnostics part's primary site is the
/// operation that fails; corpus headers pin only the line).
#[test]
fn an_index_failure_site_is_the_index_expression_in_every_engine() {
    const INLINE: &str = "pub fn main(rt: Io, args: Args) -> Result[(), ()] {
  let xs: Inline[u8, 1] = [7u8]
  let i: u64 = 1
  let x = xs[i]
  Ok(())
}
";
    const LIST: &str = "pub fn main(rt: Io, args: Args) -> Result[(), ()] {
  let xs: List[u8] = [7u8]
  let i: u64 = 1
  let x = xs[i]
  Ok(())
}
";
    let matrix = smoke_matrix();
    for (name, source) in [("site-inline", INLINE), ("site-list", LIST)] {
        let observed = agree(name, source, &matrix);
        let [failure] = observed.failures.as_slice() else {
            panic!("{name}: {observed:?}")
        };
        assert_eq!(
            (failure.rule.as_str(), failure.line, failure.column),
            ("MEM-14", 4, 11),
            "{name}"
        );
    }
}

/// A native request's owned Ready execution view runs like the reference in
/// every smoke configuration: the heap executor and both backends execute the
/// request's settled Ready products and report failures through the view's
/// own site tables.
#[test]
fn requested_ready_views_agree_with_the_reference_under_the_smoke_matrix() {
    use hirsel_native::BuildOptions;
    use hirsel_native::NativeError;
    use hirsel_native::Options;
    use hirsel_requests::CompileOperation;
    use hirsel_requests::CompileResult;
    use hirsel_requests::CompilerRequests;
    use hirsel_requests::NativeOutput;
    use hirsel_requests::NativeReady;
    use hirsel_requests::NativeRequest;
    const PRINT: &str = "pub fn main(rt: Io, args: Args) -> Result[(), GrantError] {
  let _ = out.print(io.stdout(rt)?, \"hi\\n\")
  Ok(())
}
";
    const LIST: &str = "pub fn main(rt: Io, args: Args) -> Result[(), ()] {
  let xs: List[u8] = [7u8]
  let i: u64 = 1
  let x = xs[i]
  Ok(())
}
";
    // A std-free standalone package, as the in-memory reference checks it.
    const SIDECAR: &str = r#"{"schema":1,"package":{"name":"standalone","version":"0"},"entry":{"module":[],"name":"main"},"sources":[{"path":"main.hir","module":[]}],"dependencies":[]}"#;
    with_compiler_stack(|| {
        let matrix = smoke_matrix();
        let tools = toolchain(&matrix);
        for (name, source) in [("ready-print", PRINT), ("ready-list", LIST)] {
            let dir = scratch(name);
            let path = dir.join("main.hir");
            std::fs::write(&path, source).unwrap();
            std::fs::write(path.with_extension("hir.json"), SIDECAR).unwrap();
            let mut loaded = hirsel_front_phases::load_program_source(&path, None).unwrap();
            loaded.check_for_execution().unwrap();
            let entry = loaded.entry.clone();
            let execution = run_observed_snapshot(
                &mut loaded.engine,
                &RunConfiguration {
                    entry: entry.clone(),
                    hash_seed: 0,
                    inputs: vec![],
                },
                SimulationConfig::default(),
            )
            .unwrap();
            let reference = Observation::of_reference(execution.observation.as_ref().unwrap());
            let compiler = hirsel_front_phases::COMPILER;
            let mut requests =
                CompilerRequests::for_host(compiler.clone(), compiler.configuration())
                    .with_toolchain(std::sync::Arc::new(tools.clone()));
            let mut ready =
                |options: &Options| -> Result<std::sync::Arc<NativeReady>, NativeError> {
                    let failed = |message: String| NativeError {
                        stage: StageName::Ready,
                        definition: None,
                        validator: false,
                        message,
                    };
                    let output = requests
                        .execute(
                            &path,
                            CompileOperation::NativeBuild(NativeRequest {
                                definition: None,
                                test_entries: vec![],
                                supplemental_sources: vec![],
                                output: NativeOutput::Ready,
                                options: BuildOptions::new(
                                    options.clone(),
                                    hirsel_native::Backend::Llvm,
                                    hirsel_native::OptLevel::Default,
                                ),
                                run: None,

                                evidence: None,
                                platform: None,
                            }),
                        )
                        .map_err(|error| failed(error.to_string()))?;
                    match &output.result {
                        CompileResult::NativeReady(view) => Ok(view.clone()),
                        CompileResult::NativeError(error) => Err(error.clone()),
                        _ => Err(failed("another operation".into())),
                    }
                };
            let outcomes = hirsel_native_testing::differential_ready(
                &mut ready,
                &entry,
                &reference,
                &matrix,
                &tools,
                &dir,
                &Default::default(),
            );
            assert_eq!(outcomes.len(), matrix.len(), "{name}");
            for (configuration, outcome) in outcomes {
                assert!(
                    matches!(outcome, Outcome::Agree),
                    "{name} under {configuration}: {outcome:?}"
                );
            }
            // Hand-derived: the list index `xs[i]` on line 4, column 11.
            if name == "ready-list" {
                let [failure] = reference.failures.as_slice() else {
                    panic!("{reference:?}")
                };
                assert_eq!(
                    (failure.rule.as_str(), failure.line, failure.column),
                    ("MEM-14", 4, 11)
                );
            } else {
                assert_eq!(reference.output, b"hi\n");
            }
        }
    });
}

#[test]
#[ignore = "native: run by tools/check-native.sh"]
fn descriptor_indices_are_assigned_before_all_twenty_two_link_partitions() {
    let source = r#"
type Pair { first: box u64, second: box u64 }
fn leaf(n: u64) -> box u64 { box n }
fn pair(n: u64) -> box Pair { box Pair(first: leaf(n), second: leaf(n + 1u64)) }
fn same(a: box Pair, b: box Pair) -> bool { a == b }
pub fn main(rt: Io, args: Args) -> Result[(), GrantError] {
    let a = pair(40u64)
    let b = pair(40u64)
    let c = pair(41u64)
    let valid = same(a, b) && !same(a, c)
    let _ = out.print(io.stdout(rt)?, if valid { "indices ok\n" } else { "indices bad\n" })
    Ok(())
}
"#;
    let matrix = topology_matrix(20);
    assert_eq!(matrix.len(), 22);
    let observed = agree("descriptor-indices", source, &matrix);
    assert_eq!(observed.output, b"indices ok\n");
}

#[test]
fn native_main_records_closed_stdout_and_broken_pipe_as_output_errors() {
    use hirsel_native::OptLevel;
    use hirsel_native::Options;
    use hirsel_native::link;
    use std::os::unix::process::CommandExt;
    use std::process::Command;
    with_compiler_stack(|| {
        let checked = checked(HELLO);
        let configurations = smoke_matrix();
        let tools = toolchain(&configurations);
        let target = TargetSpec::x86_64_unknown_linux_gnu();
        let program = ready(HELLO, &checked.entry, &tools)(&Options::new(target)).unwrap();
        let program = &program.program;
        let root = scratch("closed-output");
        let executable = link(
            &program,
            &checked.entry,
            OptLevel::Speed,
            Partition::Whole,
            &tools,
            &root,
        )
        .unwrap();
        let tape = root.with_extension("tape");
        for broken_pipe in [false, true] {
            let mut command = Command::new(&executable);
            command
                .env_clear()
                .env("HIRSEL_RT_ROOT", &root)
                .env("HIRSEL_RT_TAPE", &tape);
            if broken_pipe {
                use std::os::fd::{FromRawFd, OwnedFd};
                unsafe extern "C" {
                    fn pipe(fds: *mut i32) -> i32;
                    fn close(fd: i32) -> i32;
                }
                let mut fds = [0; 2];
                assert_eq!(unsafe { pipe(fds.as_mut_ptr()) }, 0);
                unsafe {
                    close(fds[0]);
                }
                command.stdout(std::process::Stdio::from(unsafe {
                    OwnedFd::from_raw_fd(fds[1])
                }));
            } else {
                // SAFETY: child-only pre-exec close of a standard descriptor.
                unsafe {
                    command.pre_exec(|| {
                        libc_close_stdout();
                        Ok(())
                    });
                }
            }
            let output = command.output().unwrap();
            assert_eq!(output.status.code(), Some(0), "{output:?}");
            let record = hirsel_rt::launch::parse_recording(
                &hirsel_rt::launch::read_recording_text(&tape).unwrap(),
            )
            .unwrap();
            assert!(matches!(
                record.events[1].completion,
                hirsel_rt::io::Completion::Unit(Err(_))
            ));
            assert_eq!(record.events[1].fault.as_ref().unwrap().partial, 0);
        }
        // A valid argument admits the launch; the byte-invalid twin refuses
        // before entry and before creating its ordinary tape.
        use std::os::unix::ffi::OsStringExt;
        std::fs::remove_file(&tape).unwrap();
        for (argument, expected) in [
            (std::ffi::OsString::from("é"), 0),
            (std::ffi::OsString::from_vec(vec![0xff]), 3),
        ] {
            let output = Command::new(&executable)
                .arg(argument)
                .env_clear()
                .env("HIRSEL_RT_ROOT", &root)
                .env("HIRSEL_RT_TAPE", &tape)
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(expected), "{output:?}");
            if expected == 0 {
                assert_eq!(output.stdout, checked.reference.output);
                let record = hirsel_rt::launch::parse_recording(
                    &hirsel_rt::launch::read_recording_text(&tape).unwrap(),
                )
                .unwrap();
                assert_eq!(record.arguments, ["é"]);
                std::fs::remove_file(&tape).unwrap();
            } else {
                assert!(!tape.exists());
                assert!(
                    String::from_utf8_lossy(&output.stderr).contains("argument 0 is not UTF-8")
                );
            }
        }
        // Invalid snapshot names and links affect operations, not launch admission.
        std::fs::write(
            root.join(std::ffi::OsString::from_vec(vec![0xff])),
            b"invalid",
        )
        .unwrap();
        std::os::unix::fs::symlink("../outside", root.join("escape")).unwrap();
        let output = Command::new(&executable)
            .env_clear()
            .env("HIRSEL_RT_ROOT", &root)
            .env("HIRSEL_RT_TAPE", &tape)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        assert_eq!(output.stdout, checked.reference.output);
        let record = hirsel_rt::launch::parse_recording(
            &hirsel_rt::launch::read_recording_text(&tape).unwrap(),
        )
        .unwrap();
        assert_eq!(record.tree.links["escape"], "../outside");
        std::fs::remove_file(tape).unwrap();
        // Keep the fd close local to the child without adding a production dependency.
        unsafe fn libc_close_stdout() {
            unsafe extern "C" {
                fn close(fd: i32) -> i32;
            }
            unsafe {
                close(1);
            }
        }
    });
}

// The importer check and the executable comparison build separately, so the
// two cases run in parallel shards.

/// Root-free process entries keep the ordinary Result and Failure exit contract.
#[test]
fn zero_parameter_entries_agree_without_host_roots() {
    let mut matrix = smoke_matrix();
    matrix.push(Configuration {
        options: matrix[0].options.clone(),
        engine: Engine::Cranelift {
            partition: Partition::Whole,
            runtime: hirsel_native_testing::Runtime::Diagnostic,
        },
    });
    for (name, source, exit) in [
        (
            "zero-ok",
            "pub fn main() -> Result[(), ()] { Ok(()) }",
            Some(0),
        ),
        (
            "zero-error",
            "pub fn main() -> Result[(), ()] { Error(()) }",
            Some(1),
        ),
        (
            "zero-failure",
            "pub fn main() -> Result[(), ()] { let _ = 1u64 / 0u64; Ok(()) }",
            None,
        ),
    ] {
        let observed = agree(name, source, &matrix);
        assert_eq!(observed.exit_code, exit);
        assert!(observed.output.is_empty());
        assert_eq!(observed.failures.len(), usize::from(exit.is_none()));
    }
}

// Native stack exhaustion is a run stop: unbounded recursion on the root task's stack and on a
// spawned task's stack, and recursion through frames wider than a fiber's guard, each end with
// the typed stack stop after the output they produced, on both backends, never a crash. How
// deep each run gets is the backend's business and is not compared.

const DEEP: &str = r#"fn deep(n: u64) -> u64 {
  deep(n + 1u64) + deep(n + 2u64)
}
"#;

fn on_root() -> String {
    format!(
        r#"{DEEP}
pub fn main(rt: Io, args: Args) -> Result[(), GrantError] {{
  let _ = out.print(io.stdout(rt)?, "start\n")
  let d = deep(0u64)
  let _ = out.print(io.stdout(rt)?, if d == 0u64 {{ "zero\n" }} else {{ "nonzero\n" }})
  Ok(())
}}
"#
    )
}

fn on_task() -> String {
    format!(
        r#"{DEEP}
pub fn main(rt: Io, args: Args) -> Result[(), GrantError] {{
  let tasks = io.tasks(rt)?
  let _ = out.print(io.stdout(rt)?, "start\n")
  task.scope(
    tasks,
    fn(s: Scope) {{
      let h = task.async(s, fn() {{
        let d = deep(0u64)
      }})
      task.await(s, take h)
    }}
  )
  Ok(())
}}
"#
    )
}

/// Each activation keeps a 72,000-byte inline array, wider than a fiber's 64 KiB guard, live
/// across its call, so each engine's probe loop is what meets the guard. The unreachable base
/// case keeps the call's result live (without it LLVM proves the call never returns and drops
/// the array), and one element differs from the rest so the read after the call cannot fold
/// the array away. Whether a missing probe would still fault in the guard depends on what the
/// kernel maps below it, so no run can observe the probes themselves.
fn large_frames() -> String {
    r#"fn wide(n: u64) -> u64 {
  if n == 1000000000000u64 {
    return 0u64
  }
  let mut w: Inline[u64, 9000] = [n; 9000]
  w[(n * 7u64) % 9000u64] = n * 3u64
  let below = wide(n + 1u64)
  w[(n * 13u64) % 9000u64] + below
}

pub fn main(rt: Io, args: Args) -> Result[(), GrantError] {
  let _ = out.print(io.stdout(rt)?, "start\n")
  let d = wide(0u64)
  let _ = out.print(io.stdout(rt)?, if d == 0u64 { "zero\n" } else { "nonzero\n" })
  Ok(())
}
"#
    .to_owned()
}

/// Builds `source` with each backend as `run --native` does (LLVM optimized as one object,
/// Cranelift unoptimized per unit), launches it and decodes its observation record: the run
/// printed `start` and then stopped on the stack budget.
fn assert_stack_stop(name: &str, source: &str) {
    use hirsel_native::{Backend, OptLevel, Options};
    with_compiler_stack(|| {
        let entry = entry_path(&default_context());
        let tools = toolchain(&smoke_matrix());
        let program = ready(source, &entry, &tools)(&Options::new(x86())).unwrap();
        for (backend, opt, partition) in [
            (Backend::Llvm, OptLevel::Default, Partition::Whole),
            (Backend::Cranelift, OptLevel::None, Partition::Singleton),
        ] {
            let root = scratch(&format!("stack-{name}-{backend:?}"));
            let objects = hirsel_native::emit_backend_objects(
                &program.program,
                backend,
                opt,
                partition,
                false,
                None,
            )
            .unwrap();
            let executable =
                hirsel_native::link_objects(&program.program, &entry, &objects, &tools, &root)
                    .unwrap();
            let launched =
                hirsel_native::launch(&executable, &x86(), &tools, &root, false).unwrap();
            let sources = Default::default();
            let sites = hirsel_native::Sites::from_anchors(&sources, Default::default());
            let hirsel_native::Observed::Ran(observation, waits) =
                hirsel_native::observe_run(&launched, &program.program, &sites).unwrap()
            else {
                panic!("{backend:?} {launched:?}")
            };
            assert_eq!(observation.output, b"start\n", "{backend:?}");
            assert_eq!(
                observation.outcome(&sites, waits).unwrap(),
                hirsel_run_schema::RunOutcome::BudgetExceeded(hirsel_run_schema::BudgetKind::Stack),
                "{backend:?}"
            );
        }
    })
}

#[test]
fn unbounded_recursion_on_the_root_stack_is_a_stack_stop() {
    assert_stack_stop("root", &on_root());
}

#[test]
fn unbounded_recursion_on_a_task_stack_is_a_stack_stop() {
    assert_stack_stop("task", &on_task());
}

#[test]
fn frames_wider_than_the_guard_are_probed_into_a_stack_stop() {
    assert_stack_stop("wide", &large_frames());
}
