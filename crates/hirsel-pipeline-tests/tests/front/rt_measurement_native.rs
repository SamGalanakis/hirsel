//! Native smoke and hand-counted programs against independently built runtime variants.
use hirsel_driver::native;
use hirsel_engine::{
    QueryEngine, RunConfiguration, SimulationConfig, ValidatedNativeInput, default_context,
    default_environment, entry_path, run_observed_snapshot, source_check_request,
};
use hirsel_front_phases::compiler_adapters;
use hirsel_intern::jobs::with_compiler_stack;
use hirsel_native::Observation;
use hirsel_native::OptLevel;
use hirsel_native::Options;
use hirsel_native::Partition;
use hirsel_native::RuntimeBuild;
use hirsel_native::TargetSpec;
use hirsel_native::Toolchain;
use std::path::{Path, PathBuf};

fn checked(source: &str) -> (ValidatedNativeInput, hirsel_engine::DefPath, Observation) {
    let context = default_context();
    let mut engine = QueryEngine::new(default_environment(), compiler_adapters());
    let file = engine.sources.add("native.hir", source.into()).unwrap();
    let output = source_check_request(&mut engine, file, &context).unwrap();
    assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
    let entry = entry_path(&context);
    let execution = run_observed_snapshot(
        &mut engine,
        &RunConfiguration {
            entry: entry.clone(),
            hash_seed: 0,
            inputs: vec![],
        },
        SimulationConfig::default(),
    )
    .unwrap();
    let reference = Observation::of_reference(execution.observation.as_ref().unwrap());
    let input = engine.export_native_input().unwrap();
    (engine.import_native_input(input).unwrap(), entry, reference)
}
fn scratch(name: &str) -> PathBuf {
    let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    path
}
fn variants(dir: &Path, target: &TargetSpec, diagnostic: bool) -> Vec<Toolchain> {
    let mut variants = vec![];
    for system_allocator in [false, true] {
        for measurement in [false, true] {
            variants.push(
                Toolchain::in_tree_with_runtime(
                    dir,
                    &[target],
                    RuntimeBuild {
                        diagnostic,
                        system_allocator,
                        measurement,
                        memcheck: false,
                        ownership_trace: false,
                    },
                )
                .unwrap(),
            );
        }
    }
    let archives: std::collections::BTreeSet<_> = variants
        .iter()
        .map(|t| t.archive(&target.triple).unwrap())
        .collect();
    assert_eq!(
        archives.len(),
        4,
        "feature choices must have distinct cache keys"
    );
    variants
}
fn counter(text: &str, key: &str) -> u64 {
    text.lines()
        .find_map(|line| line.strip_prefix(&format!("{key} ")))
        .unwrap()
        .parse()
        .unwrap()
}

#[test]
fn free_worker_reports_include_the_allocation_population_at_widths_1_and_8() {
    with_compiler_stack(|| {
        let dir = scratch("measurement-free-heaps");
        let source = dir.join("allocation.hir");
        std::fs::write(
            &source,
            include_str!("../../../../bench/multicore/programs/allocation.hir"),
        )
        .unwrap();
        let std_manifest =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../stdlib/std@1/hirsel.json");
        std::fs::write(
            dir.join("hirsel.json"),
            serde_json::json!({
                "schema": 1, "package": {"name": "multicore", "version": "1"},
                "entry": {"module": [], "name": "main"},
                "sources": [{"path": "allocation.hir", "module": []}],
                "dependencies": [{"manifest": std_manifest,
                    "package": {"name": "std", "version": "1"}, "expose": []}],
            })
            .to_string(),
        )
        .unwrap();
        let mut loaded = hirsel_front_phases::load_program(&source).unwrap();
        loaded.check_for_execution().unwrap();
        let input = loaded.engine.export_native_input().unwrap();
        let sources = hirsel_native::profile_sources(&input);
        let input = loaded.engine.import_native_input(input).unwrap();
        let target = TargetSpec::x86_64_unknown_linux_gnu();
        let ready = hirsel_native_batch::build(&input, &Options::new(target.clone())).unwrap();
        let tools = Toolchain::in_tree_with_runtime(
            &dir.join("runtime"),
            &[&target],
            RuntimeBuild {
                measurement: true,
                ..RuntimeBuild::default()
            },
        )
        .unwrap();
        let objects = hirsel_native::emit_objects(
            &ready,
            OptLevel::Speed,
            Partition::Whole,
            true,
            Some(&sources),
        )
        .unwrap();
        std::fs::create_dir_all(dir.join("objects")).unwrap();
        let exe = hirsel_native::link_standalone_objects(
            &ready,
            &loaded.entry,
            &objects,
            &tools,
            &dir.join("objects"),
        )
        .unwrap();
        let iterations = 7;
        let mut cooperative = None;
        for (provider, width, totals) in [
            ("cooperative", 1, true),
            ("free", 1, true),
            ("free", 8, true),
            ("free", 8, false),
        ] {
            let name = format!("{provider}-{width}-{totals}");
            let grants = dir.join(format!("{name}.json"));
            std::fs::write(&grants, serde_json::json!({
                "io": {"stdout": "stdout.bytes@1", "tasks": format!("tasks.{provider}@1"), "dirs": []},
                "placement": {"cores": width}, "args": [],
            }).to_string()).unwrap();
            let report = dir.join(format!("{name}.measurement"));
            let profile_path = report.with_extension("measurement.sites.json");
            let mut command = std::process::Command::new(&exe);
            command
                .arg("--hirsel-grants")
                .arg(&grants)
                .arg(iterations.to_string());
            if totals {
                command.env("HIRSEL_RT_MEASUREMENT", &report);
            } else {
                command
                    .env_remove("HIRSEL_RT_MEASUREMENT")
                    .env("HIRSEL_RT_PROFILE", &profile_path);
            }
            let output = command.output().unwrap();
            assert!(output.status.success(), "{name}: {output:?}");
            assert_eq!(output.stdout, format!("{}\n", 8 * iterations).as_bytes());
            let profile: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&profile_path).unwrap()).unwrap();
            let allocations = profile["totals"]["allocations"].as_u64().unwrap();
            if totals {
                let text = std::fs::read_to_string(&report).unwrap();
                let class_allocations: u64 = text
                    .lines()
                    .filter(|line| line.starts_with("class "))
                    .map(|line| {
                        line.split_whitespace()
                            .nth(2)
                            .unwrap()
                            .parse::<u64>()
                            .unwrap()
                    })
                    .sum();
                assert_eq!(class_allocations, allocations, "{name}");
            }
            // Eight tasks each grow one 64-element list per iteration, through
            // five backing allocations. This count comes from the workload.
            let at_push: u64 = profile["sites"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|site| {
                    site["file"].as_str().unwrap().ends_with("allocation.hir")
                        && site["line"] == 15
                        && site["operation"] == "Call"
                })
                .map(|site| site["counts"]["allocations"].as_u64().unwrap_or(0))
                .sum();
            assert_eq!(at_push, 8 * iterations * 5, "{name}: {profile}");
            for (kind, total) in profile["totals"].as_object().unwrap() {
                let attributed: u64 = profile["sites"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|site| site["counts"][kind].as_u64().unwrap_or(0))
                    .sum();
                assert_eq!(attributed, total.as_u64().unwrap(), "{name}: {kind}");
            }
            if let Some(expected) = cooperative {
                assert_eq!(allocations, expected, "{name}");
            } else {
                cooperative = Some(allocations);
            }
        }
    });
}

fn smoke_entries(path: &Path) -> Vec<(hirsel_engine::LoadedProgram, u64)> {
    use hirsel_requests::CompilerRequests;
    let source = std::fs::read_to_string(path).unwrap();
    if !source.lines().any(|line| line.trim() == "// expect: test") {
        return vec![(hirsel_front_phases::load_program(path).unwrap(), 0)];
    }
    let compiler = hirsel_front_phases::COMPILER;
    let mut requests = CompilerRequests::for_host(compiler.clone(), compiler.configuration());
    let (_, inputs) = hirsel_engine::requests::observe_inputs(
        requests.input_limits(),
        requests.capture_attempts(),
        |inputs| compiler.load_program_inputs(path, inputs, false),
    )
    .unwrap();
    let discovery = hirsel_driver::testing::discover(
        &mut requests,
        path,
        inputs,
        &[],
        None,
        hirsel_driver::testing::Selection::Tests,
    )
    .unwrap();
    assert!(
        !discovery.tests.is_empty(),
        "{} selected no tests",
        path.display()
    );
    let inputs = discovery
        .execution_inputs(requests.input_limits().bytes)
        .unwrap();
    discovery
        .tests
        .iter()
        .map(|test| {
            let mut loaded = compiler.load_program_inputs(path, &inputs, true).unwrap();
            loaded.entry = test.entry.clone();
            (loaded, test.seed)
        })
        .collect()
}

fn smoke_paths(shard: &str) -> Vec<&'static str> {
    let (index, count) = shard
        .split_once('/')
        .expect("runtime smoke shard must be i/n");
    let index: usize = index.parse().expect("runtime smoke shard index");
    let count: usize = count.parse().expect("runtime smoke shard count");
    assert!(
        index > 0 && count > 0 && index <= count,
        "invalid runtime smoke shard {shard}"
    );
    let paths: Vec<_> = include_str!("../../../../native-tests/regressions/smoke.txt")
        .lines()
        .filter(|path| !path.is_empty() && !path.starts_with('#'))
        .enumerate()
        .filter_map(|(offset, path)| (offset % count == index - 1).then_some(path))
        .collect();
    assert!(
        !paths.is_empty(),
        "runtime smoke shard {shard} selected no programs"
    );
    paths
}

#[test]
fn runtime_smoke_shards_cover_each_program_once() {
    let all = smoke_paths("1/1");
    let shards: Vec<_> = (1..=8)
        .flat_map(|index| smoke_paths(&format!("{index}/8")))
        .collect();
    assert_eq!(shards.len(), all.len());
    let unique: std::collections::BTreeSet<_> = shards.into_iter().collect();
    assert_eq!(unique.len(), all.len());
    assert_eq!(unique, all.into_iter().collect());
}

#[test]
#[ignore = "full gate; HIRSEL_RUNTIME_SMOKE_SHARD=i/n selects one partition"]
fn full_gate_every_native_smoke_case_preserves_its_observation_in_all_runtime_variants() {
    with_compiler_stack(|| {
        let target = TargetSpec::x86_64_unknown_linux_gnu();
        let cache = Path::new(env!("CARGO_TARGET_TMPDIR")).join("measurement-runtime-cache");
        let variants = variants(&cache, &target, true);
        let root = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
        let mut programs = 0;
        let mut entries = 0;
        let mut observations = 0;
        let shard = std::env::var("HIRSEL_RUNTIME_SMOKE_SHARD").unwrap_or_else(|_| "1/1".into());
        for path in smoke_paths(&shard) {
            let _progress =
                hirsel_intern::test_progress::enter("runtime-smoke-case", || path.to_owned());
            programs += 1;
            for (mut loaded, test_seed) in smoke_entries(&root.join(path)) {
                entries += 1;
                loaded
                    .check_for_execution()
                    .unwrap_or_else(|error| panic!("{path}: {error}"));
                let entry = loaded.entry.clone();
                let execution = run_observed_snapshot(
                    &mut loaded.engine,
                    &RunConfiguration {
                        entry: entry.clone(),
                        hash_seed: 0,
                        inputs: vec![],
                    },
                    SimulationConfig {
                        test_seed,
                        ..Default::default()
                    },
                )
                .unwrap();
                let reference = Observation::of_reference(execution.observation.as_ref().unwrap());
                let raw = loaded.engine.export_native_input().unwrap();
                let input = loaded.engine.import_native_input(raw).unwrap();
                let mut options = Options::new(target.clone());
                options.launch.test_seed = test_seed;
                let program = hirsel_native_batch::build(&input, &options).unwrap();
                let sites = hirsel_native::Sites::new(input.snapshot());
                for tools in &variants {
                    let name = format!(
                        "runtime-smoke-{}-{}-{}",
                        Path::new(path).file_stem().unwrap().to_str().unwrap(),
                        tools.runtime_build.system_allocator,
                        tools.runtime_build.measurement
                    );
                    // Each variant toolchain links its own archive, the optimized
                    // executable as one object, under the diagnostic checks.
                    let dir = scratch(&name);
                    let observed = hirsel_native::link(
                        &program,
                        &entry,
                        OptLevel::Speed,
                        Partition::Whole,
                        tools,
                        &dir,
                    )
                    .and_then(|exe| hirsel_native::launch(&exe, &target, tools, &dir, true))
                    .and_then(|launched| hirsel_native::observe(&launched, &program, &sites));
                    assert!(
                        observed.as_ref() == Ok(&reference),
                        "{path}, {:?}: {observed:?}",
                        tools.runtime_build
                    );
                    observations += 1;
                }
            }
        }
        assert!(programs > 0 && observations > 0);
        eprintln!(
            "runtime smoke: {programs} programs, {entries} entries, {observations} variant observations"
        );
    });
}

// A leaf sequence whose last credit dies through inline glue. No output allocation.
const LEAF: &str = r#"
pub fn main(rt: Io, args: Args) -> Result[(), GrantError] {
  let xs: Array[u64] = [7u64, 9u64]
  let _ = out.print(io.stdout(rt)?, if xs[1u64] == 9u64 { "nine\n" } else { "bad\n" })
  Ok(())
}
"#;

fn trees_and_inline_counts(target: TargetSpec, name: &str) {
    with_compiler_stack(move || {
        let cache = Path::new(env!("CARGO_TARGET_TMPDIR")).join("measurement-runtime-cache");
        let variants = variants(&cache, &target, false);
        for (case, source) in [("inline-leaf", LEAF)] {
            let (input, entry, reference) = checked(source);
            assert_eq!(reference.output, b"nine\n");
            let mut options = Options::new(target.clone());
            options.regions = false;
            let program = hirsel_native_batch::build(&input, &options).unwrap();
            let default_object = hirsel_native::emit(&program, OptLevel::Speed, |_| true).unwrap();
            assert!(
                !default_object
                    .imports
                    .iter()
                    .any(|i| i.name.starts_with("hirsel_rt_v2_measure_"))
            );
            for tools in &variants {
                let dir = scratch(&format!(
                    "{name}-{case}-{}-{}",
                    tools.runtime_build.system_allocator, tools.runtime_build.measurement
                ));
                let exe = hirsel_native::link(
                    &program,
                    &entry,
                    OptLevel::Speed,
                    Partition::Whole,
                    tools,
                    &dir,
                )
                .unwrap();
                let launched = hirsel_native::launch(&exe, &target, tools, &dir, false).unwrap();
                assert_eq!(launched.exit, Some(0), "{:?}", launched);
                assert_eq!(launched.stdout, reference.output);
                assert!(
                    launched
                        .record
                        .contains(if tools.runtime_build.measurement {
                            "audit 0 0 0 0 0"
                        } else {
                            "audit unavailable"
                        }),
                    "{}",
                    launched.record
                );
                if tools.runtime_build.measurement {
                    let text = std::fs::read_to_string(dir.join("measurement")).unwrap();
                    assert!(text.starts_with("hirsel-measurement 1\n"));
                    assert_eq!(counter(&text, "live_bytes"), 0);
                    if case == "inline-leaf" {
                        // One Array: length + two u64s -> class 32. Its one inline drop plus
                        // the temporary static output slice's immortal drop are two attempts.
                        assert!(text.contains("class 32 1 1\n"), "{text}");
                        assert_eq!(counter(&text, "count_decrements"), 2, "{text}");
                        assert_eq!(counter(&text, "count_increments"), 0, "{text}");
                        assert_eq!(counter(&text, "peak_live_bytes"), 32, "{text}");
                    }
                } else {
                    let linked = std::fs::read(dir.join("unit-0.o")).unwrap();
                    assert_eq!(
                        hirsel_syntax::ContentHash::of_bytes(&linked),
                        hirsel_syntax::ContentHash::of_bytes(&default_object.bytes),
                        "measurement off must emit the default object hash"
                    );
                    assert!(!dir.join("measurement").exists());
                }
            }
        }
        let expected = b"stretch tree of depth 7\t check: 255\n64\t trees of depth 4\t check: 1984\n16\t trees of depth 6\t check: 2032\nlong lived tree of depth 6\t check: 127\n";
        for tools in &variants {
            let dir = scratch(&format!(
                "{name}-binarytrees-{}-{}",
                tools.runtime_build.system_allocator, tools.runtime_build.measurement
            ));
            let exe = dir.join("binarytrees");
            let arm = target.triple.starts_with("aarch64");
            let mut cmd = std::process::Command::new(if arm {
                tools.aarch64_cc.as_ref().unwrap()
            } else {
                &tools.clang
            });
            let output = cmd
                .arg(
                    Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("../hirsel-rt/harness/binarytrees.c"),
                )
                .args(["-std=c11", "-O2", "-DHIRSEL_RT", "-I"])
                .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../hirsel-rt/include"))
                .arg(tools.archive(&target.triple).unwrap())
                .args([
                    "-lgcc_s",
                    "-lutil",
                    "-lrt",
                    "-lpthread",
                    "-lm",
                    "-ldl",
                    "-o",
                ])
                .arg(&exe)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let mut command = if arm {
                let mut cmd = std::process::Command::new(tools.qemu_aarch64.as_ref().unwrap());
                cmd.arg("-L")
                    .arg(tools.aarch64_sysroot.as_ref().unwrap())
                    .arg(&exe);
                cmd
            } else {
                std::process::Command::new(&exe)
            };
            let output = command.arg("6").output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(output.stdout, expected, "{:?}", tools.runtime_build);
        }
    });
}
#[test]
#[ignore = "full gate"]
fn full_gate_binarytrees_and_inline_counts_match_on_x86_64() {
    trees_and_inline_counts(TargetSpec::x86_64_unknown_linux_gnu(), "measurement-x86");
}
#[test]
#[ignore = "full gate"]
fn full_gate_binarytrees_and_inline_counts_match_on_aarch64() {
    trees_and_inline_counts(TargetSpec::aarch64_unknown_linux_gnu(), "measurement-arm");
}

#[test]
fn production_corpus_object_keeps_its_uninstrumented_bytes() {
    with_compiler_stack(|| {
        let (input, _, _) = checked(include_str!(
            "../../../../conformance/PROV/PROV-1-copied-prefix-write-run.hir"
        ));
        let ready = hirsel_native_batch::build(
            &input,
            &Options::new(TargetSpec::x86_64_unknown_linux_gnu()),
        )
        .unwrap();
        let object = hirsel_native::emit(&ready, OptLevel::Speed, |_| true).unwrap();
        assert!(
            !object
                .imports
                .iter()
                .any(|i| i.name.starts_with("hirsel_rt_v2_measure_"))
        );
        // Transient exact-parent comparison; the fixed hash is evidence in README, not a
        // permanent constraint on unrelated future compiler improvements.
        if let Some(path) = std::env::var_os("HIRSEL_MEASUREMENT_OBJECT_CHECK") {
            assert_eq!(object.bytes, std::fs::read(path).unwrap());
        }
    });
}

#[test]
fn diagnostic_launch_rejects_one_leaked_block_and_accepts_its_freed_twin() {
    with_compiler_stack(|| {
        let target = TargetSpec::x86_64_unknown_linux_gnu();
        let cache = Path::new(env!("CARGO_TARGET_TMPDIR")).join("measurement-runtime-cache");
        let tools = Toolchain::in_tree_diagnostic(&cache, &[&target]).unwrap();
        let (input, _, _) = checked(LEAF);
        let ready = hirsel_native_batch::build(&input, &Options::new(target.clone())).unwrap();
        let dir = scratch("measurement-leak-control");
        let source = dir.join("control.c");
        std::fs::write(&source, r#"#include "hirsel_rt_v2.h"
#include <string.h>
static uint8_t entry(hirsel_rt_v2_ctx *ctx, uint8_t *destination, uint8_t *result, const uint64_t *rt, const uint64_t *args) {
  (void)destination; (void)rt; (void)args;
  hirsel_rt_v2_header *block = hirsel_rt_v2_alloc(ctx, 0, 8, 8, HIRSEL_RT_V2_DOMAIN_HEAP, 0);
  memset(hirsel_rt_v2_payload(block), 0, 8);
#ifdef FREE_TWIN
  hirsel_rt_v2_release(ctx, block, 0, 0, 0, 0);
#endif
  memset(result, 0, 4);
  return 0;
}
int main(void) { hirsel_rt_v2_launch_config config = {0}; return hirsel_rt_v2_launch(entry, 4, 0, 0, &config); }
"#).unwrap();
        for free in [false, true] {
            let exe = dir.join(if free { "freed" } else { "leaked" });
            let mut cmd = std::process::Command::new(&tools.clang);
            cmd.arg(&source)
                .arg("-I")
                .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../hirsel-rt/include"));
            if free {
                cmd.arg("-DFREE_TWIN");
            }
            let output = cmd
                .arg(tools.archive(&target.triple).unwrap())
                .args([
                    "-lgcc_s",
                    "-lutil",
                    "-lrt",
                    "-lpthread",
                    "-lm",
                    "-ldl",
                    "-o",
                ])
                .arg(&exe)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let launched = hirsel_native::launch(&exe, &target, &tools, &dir, false).unwrap();
            let observation = hirsel_native::observe(
                &launched,
                &ready,
                &hirsel_native::Sites::new(input.snapshot()),
            );
            if free {
                assert!(observation.is_ok(), "{observation:?}");
            } else {
                assert!(
                    launched.record.contains("audit 1 16 0 0 0"),
                    "{}",
                    launched.record
                );
                assert!(
                    observation
                        .unwrap_err()
                        .message
                        .contains("exit audit is not clean")
                );
                let mut forged = launched.clone();
                forged.record = forged
                    .record
                    .replace("audit 1 16 0 0 0", "audit unavailable");
                assert!(
                    hirsel_native::observe(
                        &forged,
                        &ready,
                        &hirsel_native::Sites::new(input.snapshot())
                    )
                    .is_err()
                );
            }
        }
    });
}

/// A fixed-size block of the system archive is bare: one malloc of its header and class
/// capacity. The control runs against the size-class archive too (`SLABS`), where the same
/// operations hold apart from the backing accounting.
const BARE_CONTROL: &str = r#"#include "hirsel_rt_v2.h"
#include <assert.h>
#include <string.h>
static const hirsel_rt_v2_layout node = {HIRSEL_RT_V2_LAYOUT_MAGIC, HIRSEL_RT_V2_LAYOUT_VERSION,
  16, 8, NULL, NULL, 0, 0, HIRSEL_RT_V2_LAYOUT_NO_EXTENT, 0, NULL};
static const hirsel_rt_v2_layout *const layouts[] = {&node};
enum { BYTES = 0, NODE = 2 };
static hirsel_rt_v2_stats stats(hirsel_rt_v2_ctx *ctx) { hirsel_rt_v2_stats s; hirsel_rt_v2_ctx_stats(ctx, &s); return s; }
static uint8_t entry(hirsel_rt_v2_ctx *ctx, uint8_t *destination, uint8_t *result, const uint64_t *rt, const uint64_t *args) {
  (void)destination; (void)rt; (void)args;
  hirsel_rt_v2_stats before = stats(ctx);
  hirsel_rt_v2_header *bare = hirsel_rt_v2_alloc(ctx, NODE, 16, 8, HIRSEL_RT_V2_DOMAIN_HEAP, 0);
  assert(bare->count == 1 && (bare->descriptor & HIRSEL_RT_V2_INDEX_MASK) == NODE);
  hirsel_rt_v2_stats one = stats(ctx);
  assert(one.live_blocks == before.live_blocks + 1 && one.live_bytes == before.live_bytes + 16);
#ifndef SLABS
  /* No prefix, back-pointer or list: the header and the 16-byte class. */
  assert((bare->descriptor >> HIRSEL_RT_V2_KIND_SHIFT & 3) == 0);
  assert(one.large_bytes == before.large_bytes + 24);
  assert(one.backing_allocations == before.backing_allocations + 1);
#endif
  /* Another extent, or a dynamic layout, keeps its capacity metadata. */
  hirsel_rt_v2_header *wide = hirsel_rt_v2_alloc(ctx, NODE, 40, 8, HIRSEL_RT_V2_DOMAIN_HEAP, 0);
  hirsel_rt_v2_header *bytes = hirsel_rt_v2_alloc(ctx, BYTES, 16, 8, HIRSEL_RT_V2_DOMAIN_HEAP, 0);
  assert(hirsel_rt_v2_capacity(wide) == 48 && hirsel_rt_v2_capacity(bytes) == 16);
  memset(hirsel_rt_v2_payload(wide), 0, 8);
  memset(hirsel_rt_v2_payload(bytes), 0, 8);
  /* A bare token reused for a bare request stays in place. */
  hirsel_rt_v2_header *token = NULL, *reused = NULL;
  assert(hirsel_rt_v2_reset(ctx, bare, NODE, NULL, 0, 0, &token) == 0 && token == bare);
  assert(hirsel_rt_v2_reuse(ctx, token, NODE, 16, 8, &reused) == 0 && reused == bare);
  /* Reused for a dynamic layout, it moves its bytes to storage with capacity metadata. */
  memset(hirsel_rt_v2_payload(bare), 0, 16);
  memcpy(hirsel_rt_v2_payload(bare) + 8, "kept\0\0\0", 8);
  assert(hirsel_rt_v2_reset(ctx, bare, NODE, NULL, 0, 0, &token) == 0);
  assert(hirsel_rt_v2_reuse(ctx, token, BYTES, 16, 8, &reused) == 0);
  assert(memcmp(hirsel_rt_v2_payload(reused) + 8, "kept", 5) == 0);
  assert(hirsel_rt_v2_capacity(reused) == 16 && reused->count == 1);
  assert(stats(ctx).live_blocks == before.live_blocks + 3);
  assert(hirsel_rt_v2_release(ctx, reused, BYTES, NULL, 0, 0) == 0);
  assert(hirsel_rt_v2_release(ctx, bytes, BYTES, NULL, 0, 0) == 0);
  assert(hirsel_rt_v2_release(ctx, wide, NODE, NULL, 0, 0) == 0);
  hirsel_rt_v2_header *last = hirsel_rt_v2_alloc(ctx, NODE, 16, 8, HIRSEL_RT_V2_DOMAIN_HEAP, 0);
  memset(hirsel_rt_v2_payload(last), 0, 16);
#ifndef LEAK
  assert(hirsel_rt_v2_release(ctx, last, NODE, NULL, 0, 0) == 0);
  hirsel_rt_v2_stats after = stats(ctx);
  assert(after.live_blocks == before.live_blocks && after.live_bytes == before.live_bytes);
#ifndef SLABS
  assert(after.large_bytes == before.large_bytes);
#endif
#endif
  memset(result, 0, 4);
  return 0;
}
int main(void) { hirsel_rt_v2_launch_config config = {0}; return hirsel_rt_v2_launch(entry, 4, layouts, 1, &config); }
"#;

#[test]
fn system_bare_blocks_are_counted_returned_at_exit_and_moved_by_a_dynamic_reuse() {
    let target = TargetSpec::x86_64_unknown_linux_gnu();
    let cache = Path::new(env!("CARGO_TARGET_TMPDIR")).join("measurement-runtime-cache");
    let dir = scratch("system-bare-control");
    let source = dir.join("control.c");
    std::fs::write(&source, BARE_CONTROL).unwrap();
    for (system_allocator, measurement) in [(true, false), (true, true), (false, false)] {
        let tools = Toolchain::in_tree_with_runtime(
            &cache,
            &[&target],
            RuntimeBuild {
                diagnostic: true,
                system_allocator,
                measurement,
                memcheck: false,
                ownership_trace: false,
            },
        )
        .unwrap();
        for leak in [false, true] {
            let name = format!("{system_allocator}-{measurement}-{leak}");
            let run = dir.join(&name);
            std::fs::create_dir_all(&run).unwrap();
            let exe = run.join("control");
            let mut cmd = std::process::Command::new(&tools.clang);
            cmd.args(["-std=c11", "-DHIRSEL_RT_ENABLE_STATS"])
                .arg(&source)
                .arg("-I")
                .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../hirsel-rt/include"));
            if !system_allocator {
                cmd.arg("-DSLABS");
            }
            if leak {
                cmd.arg("-DLEAK");
            }
            let output = cmd
                .arg(tools.archive(&target.triple).unwrap())
                .args([
                    "-lgcc_s",
                    "-lutil",
                    "-lrt",
                    "-lpthread",
                    "-lm",
                    "-ldl",
                    "-o",
                ])
                .arg(&exe)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            // Production allocation and the poisoning (checked) path.
            for poison in [false, true] {
                let launch = run.join(if poison { "poison" } else { "plain" });
                std::fs::create_dir_all(&launch).unwrap();
                let launched =
                    hirsel_native::launch(&exe, &target, &tools, &launch, poison).unwrap();
                // The leaked twin's one bare node is audited, then returned at exit.
                let audit = if leak {
                    "audit 1 16 0 0 0"
                } else {
                    "audit 0 0 0 0 0"
                };
                assert!(launched.record.contains(audit), "{name}: {launched:?}");
                if measurement {
                    let text = std::fs::read_to_string(launch.join("measurement")).unwrap();
                    let live = counter(&text, "live_bytes");
                    assert_eq!(live, if leak { 16 } else { 0 }, "{text}");
                    assert_eq!(counter(&text, "reuse_hits"), 2, "{text}");
                }
            }
        }
    }
}
