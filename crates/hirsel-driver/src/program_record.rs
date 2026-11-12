//! Program record B (native rerun design v4.1, N1). A check or build computes
//! its key P from the observed inputs without opening a stored product, then
//! looks up one record in the domain its registration authenticates. A hit
//! replays the record's diagnostics, report and status; a build hit also
//! materializes the recorded executable. A miss runs as before and publishes
//! one record. Only the registration grants authority: with no registration,
//! a copied store or a replaced one, every lookup misses.
//!
//! P is an explicit inventory. Every `HIRSEL_*` and tool variable is in one
//! class: key (its value is in P), owner (its canonical identity is in P),
//! bypass (B is not used), acquisition (how work is scheduled or admitted,
//! never what it produces), telemetry, or launch-only. An unclassified
//! `HIRSEL_*` that is set bypasses B.
use crate::session::{Policy, Session};
use hirsel_engine::{CheckCompletion, CheckOutput, QueryError, ToolError};
use hirsel_intern::timings::{self, Count, Phase};
use hirsel_native::{BuildOptions, NativeError};
use hirsel_span::{AnchorCodec, SourceMap};
use hirsel_syntax::ContentHash;
use hirsel_validate::programs::{
    Lookup, ProgramHit, ProgramKind, ProgramOperation, ProgramRecord, ProgramRecords, image_binding,
};
use std::path::{Path, PathBuf};

/// How P treats one environment variable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Class {
    /// Its value selects what is produced: the value is in P.
    Key,
    /// It selects an owner (a store, cache or home): its canonical path is in P.
    Owner,
    /// Verification, tracing or injection that must run fresh: B is not used.
    Bypass,
    /// Scheduling, admission or freshness of acquisition; never in P.
    Acquisition,
    /// Reports about the run; never in P.
    Telemetry,
    /// Read by the launched program, not by a build.
    Launch,
    /// A cross tool: in a build's P only when its target is the one that uses
    /// it (aarch64), never in a check's. Its contents enter through the
    /// captured platform identity, which that target's builds key.
    CrossTool,
    /// Set or read only by host tooling outside the compiler crates (the
    /// named reader, or `-` for a host profile variable with no reader); the
    /// compiler never reads it, so it is not in P. The environment audit
    /// (crates/hirsel-driver/tests/environment_inventory.rs) holds both facts.
    HostOnly(&'static str),
}

/// Exact names. Prefix classes are in `PREFIXES`.
const VARIABLES: &[(&str, Class)] = &[
    ("HIRSEL_AARCH64_CC", Class::CrossTool),
    ("HIRSEL_AARCH64_SYSROOT", Class::CrossTool),
    ("HIRSEL_AUDITOR_PACKAGE", Class::Key),
    ("HIRSEL_CAPTURE_LINKER", Class::Key),
    ("HIRSEL_GLIBC_FLOOR", Class::Key),
    ("HIRSEL_GUARD_ORACLE", Class::Key),
    ("HIRSEL_LINK_KIT", Class::Key),
    ("HIRSEL_QEMU_AARCH64", Class::CrossTool),
    ("HIRSEL_RUNTIME_ARCHIVES", Class::Key),
    // Memcheck runtimes are built and launched only by `test --native`,
    // which does not use B.
    ("HIRSEL_VALGRIND", Class::Launch),
    ("HIRSEL_VALGRIND_RUNTIME", Class::Launch),
    ("HIRSEL_VALIDATE_STAGES", Class::Key),
    ("LLVM_SYS_231_PREFIX", Class::Key),
    ("VALGRIND_LIB", Class::Launch),
    ("PATH", Class::Key),
    ("LD_LIBRARY_PATH", Class::Key),
    ("LANG", Class::Key),
    ("TZ", Class::Key),
    ("HIRSEL_PRODUCTS", Class::Owner),
    ("HIRSEL_RESULTS", Class::Owner),
    ("HIRSEL_RESULTS_DIR", Class::Owner),
    ("HIRSEL_NATIVE_CACHE", Class::Owner),
    ("HIRSEL_OBJECTS", Class::Owner),
    ("HOME", Class::Owner),
    ("XDG_CACHE_HOME", Class::Owner),
    ("XDG_STATE_HOME", Class::Owner),
    ("XDG_DATA_HOME", Class::Owner),
    ("HIRSEL_VERIFY_CACHE", Class::Bypass),
    ("HIRSEL_CARRIED_ROW_RECHECK", Class::Bypass),
    ("HIRSEL_INSTANCE_RECOMPUTATION_DIR", Class::Bypass),
    ("HIRSEL_NATIVE_RETENTION", Class::Bypass),
    ("HIRSEL_OPERATION_OWNER_TRACE", Class::Bypass),
    ("HIRSEL_VALIDATE_PROFILE", Class::Bypass),
    ("HIRSEL_OWNERSHIP_TRACE", Class::Bypass),
    ("LD_PRELOAD", Class::Bypass),
    ("LD_AUDIT", Class::Bypass),
    ("LD_DEBUG", Class::Bypass),
    ("LD_DEBUG_OUTPUT", Class::Bypass),
    ("MIMALLOC_SHOW_STATS", Class::Bypass),
    ("MIMALLOC_VERBOSE", Class::Bypass),
    ("MIMALLOC_SHOW_ERRORS", Class::Bypass),
    ("HIRSEL_JOBS", Class::Acquisition),
    ("HIRSEL_MEMORY_LIMIT", Class::Acquisition),
    ("HIRSEL_DAEMON", Class::Acquisition),
    ("HIRSEL_KEEP_NATIVE_SCRATCH", Class::Acquisition),
    ("HIRSEL_NATIVE_TIMEOUT_DIR", Class::Acquisition),
    ("HIRSEL_PLATFORM_CAPTURE", Class::Acquisition),
    ("HIRSEL_SERVE_IDLE", Class::Acquisition),
    ("TMPDIR", Class::Acquisition),
    ("XDG_RUNTIME_DIR", Class::Acquisition),
    ("HIRSEL_OPERATION_LATENCY", Class::Telemetry),
    ("HIRSEL_TEST_PHASE_DIR", Class::Telemetry),
    ("HIRSEL_TIR_CPU_PHASES", Class::Telemetry),
    ("HIRSEL_READ_DIR_FREE", Class::Telemetry),
    ("HIRSEL_WALK_FREE", Class::Telemetry),
    ("HIRSEL_PROGRAM_RECORD_DEBUG", Class::Telemetry),
    ("HIRSEL_ID_SALT", Class::Key),
    ("HIRSEL_HEAPCHECK_COMPARE_CAMPAIGNS", Class::Bypass),
    ("HIRSEL_HEAPCHECK_FULL_RECOUNT", Class::Bypass),
    ("HIRSEL_HEAPCHECK_PROFILE_CAMPAIGN", Class::Bypass),
    ("HIRSEL_HEAPCHECK_REEXECUTE_CAMPAIGN", Class::Bypass),
    ("HIRSEL_RUNTIME50_PROGRESS", Class::Launch),
    ("HIRSEL_TSAN_DOUBLE_CLAIM", Class::Launch),
    ("HIRSEL_RUNTIME_EVENTS", Class::Launch),
    ("HIRSEL_RUNTIME_EVENTS_LIMIT", Class::Launch),
    ("HIRSEL_RUNTIME_MAP", Class::Launch),
    ("HIRSEL_RUNTIME_MAP_LENGTH", Class::Launch),
    (
        "HIRSEL_BUCK2_CONFIG",
        Class::HostOnly("tools/buck2/clippy.py"),
    ),
    (
        "HIRSEL_BUCK2_LOG_DIR",
        Class::HostOnly("tools/buck2/event_logs.py"),
    ),
    (
        "HIRSEL_BUCK2_REMOTE_CONFIG",
        Class::HostOnly("tools/buck2/prepare.py"),
    ),
    (
        "HIRSEL_BUCK2_RE_CONCURRENCY",
        Class::HostOnly("tools/buck2/prepare.py"),
    ),
    (
        "HIRSEL_BUCK2_STORE",
        Class::HostOnly("tools/buck2/bootstrap_store.py"),
    ),
    ("HIRSEL_CACHE", Class::HostOnly("tools/compiler-bench/run")),
    (
        "HIRSEL_CANCEL_REPLAY_FIXTURES",
        Class::HostOnly("crates/hirsel-cli/tests/cli/cancel_replay_31.rs"),
    ),
    (
        "HIRSEL_CANCEL_REPLAY_MANIFEST",
        Class::HostOnly("crates/hirsel-cli/tests/cli/cancel_replay_31.rs"),
    ),
    ("HIRSEL_CAPPED_SLOTS", Class::HostOnly("tools/capped")),
    (
        "HIRSEL_COST_COMMIT",
        Class::HostOnly("tools/cost-trend.service"),
    ),
    (
        "HIRSEL_COST_TOOLCHAIN",
        Class::HostOnly("tools/cost-trend.service"),
    ),
    (
        "HIRSEL_CPU_OVERLAP_COMMANDS",
        Class::HostOnly("tools/ignored-tests"),
    ),
    (
        "HIRSEL_CPU_OVERLAP_CPUS",
        Class::HostOnly("tools/ignored-tests"),
    ),
    (
        "HIRSEL_CPU_OVERLAP_SLICE",
        Class::HostOnly("tools/ignored-tests"),
    ),
    (
        "HIRSEL_CPU_OVERLAP_UID",
        Class::HostOnly("tools/ignored-tests"),
    ),
    (
        "HIRSEL_CPU_PRIVATE_SCOPE",
        Class::HostOnly("tools/cpu-overlap"),
    ),
    (
        "HIRSEL_DISTRIBUTION_NOTICES",
        Class::HostOnly("tools/tests/distribution_notices.py"),
    ),
    ("HIRSEL_FMT_BASE", Class::HostOnly("tools/buck2/fmt.py")),
    (
        "HIRSEL_FULL_GATE_BINARY",
        Class::HostOnly("tools/buck2/full_gate_counts.py"),
    ),
    (
        "HIRSEL_FULL_GATE_EXPECT",
        Class::HostOnly("tools/buck2/full_gate_counts.py"),
    ),
    ("HIRSEL_GATE_SLOTS", Class::HostOnly("-")),
    (
        "HIRSEL_IO_REAL_TSAN",
        Class::HostOnly("tools/io_real_tsan.py"),
    ),
    (
        "HIRSEL_KEEP_TEST_SCRATCH",
        Class::HostOnly("tools/buck2/test_launcher.sh"),
    ),
    ("HIRSEL_LAND_ATTEMPT", Class::HostOnly("tools/land")),
    ("HIRSEL_LAND_BRANCH", Class::HostOnly("tools/land")),
    ("HIRSEL_LAND_CHECKOUT", Class::HostOnly("tools/land")),
    ("HIRSEL_LAND_EVENTS", Class::HostOnly("tools/land")),
    ("HIRSEL_LAND_LINTED", Class::HostOnly("tools/tests/land.py")),
    ("HIRSEL_LAND_LOG", Class::HostOnly("tools/land")),
    ("HIRSEL_LAND_RESULT_FILE", Class::HostOnly("tools/land")),
    ("HIRSEL_LAND_SOURCE", Class::HostOnly("tools/land")),
    ("HIRSEL_LAND_TICKET", Class::HostOnly("tools/land")),
    (
        "HIRSEL_LINK_HOSTS_LOCAL",
        Class::HostOnly("tools/buck2/link_hosts.py"),
    ),
    (
        "HIRSEL_MAIN_RUNS",
        Class::HostOnly("tools/compiler-bench/run-main"),
    ),
    (
        "HIRSEL_MAIN_RUN_PROGRESS",
        Class::HostOnly("tools/buck2/event_logs.py"),
    ),
    (
        "HIRSEL_MAIN_WORKTREES",
        Class::HostOnly("tools/compiler-bench/run-main"),
    ),
    ("HIRSEL_MODES", Class::HostOnly("tools/compiler-bench/run")),
    (
        "HIRSEL_NATIVE_CPU_HARNESS",
        Class::HostOnly("tools/workers-tsan"),
    ),
    (
        "HIRSEL_NATIVE_ENVIRONMENT",
        Class::HostOnly("tools/buck2/test_launcher.sh"),
    ),
    (
        "HIRSEL_PACKAGE_CACHE",
        Class::HostOnly("tools/arch_deletions.py"),
    ),
    ("HIRSEL_PRECHECK", Class::HostOnly("tools/runtime-corpus")),
    ("HIRSEL_PRIORITY", Class::HostOnly("tools/check-native.sh")),
    (
        "HIRSEL_PRIVATE_NATIVE_INPUTS",
        Class::HostOnly("tools/buck2/test_launcher.sh"),
    ),
    (
        "HIRSEL_PROGRAM_CACHE",
        Class::HostOnly("tools/compiler-bench/run"),
    ),
    (
        "HIRSEL_REPLAY_BACKEND",
        Class::HostOnly("tools/buck2/defs.bzl"),
    ),
    (
        "HIRSEL_REPLAY_CELL",
        Class::HostOnly("tools/buck2/defs.bzl"),
    ),
    (
        "HIRSEL_REPLAY_FIXTURE",
        Class::HostOnly("tools/buck2/defs.bzl"),
    ),
    (
        "HIRSEL_REPLAY_FIXTURE_OUT",
        Class::HostOnly("tools/buck2/defs.bzl"),
    ),
    ("HIRSEL_SANITIZER", Class::HostOnly("tools/buck2/defs.bzl")),
    (
        "HIRSEL_SCRATCH_OWNER",
        Class::HostOnly("tools/buck2/test_launcher.sh"),
    ),
    (
        "HIRSEL_STARTUP_DRIVER_TESTS",
        Class::HostOnly("tools/buck2/sync.py"),
    ),
    (
        "HIRSEL_STARTUP_NATIVE_TESTS",
        Class::HostOnly("tools/buck2/sync.py"),
    ),
    (
        "HIRSEL_TEST_BATCH_CONCURRENCY",
        Class::HostOnly("tools/buck2/event_logs.py"),
    ),
    (
        "HIRSEL_TEST_BATCH_SIZE",
        Class::HostOnly("tools/buck2/event_logs.py"),
    ),
    (
        "HIRSEL_TEST_BIN",
        Class::HostOnly("tools/tests/buck2_launcher.py"),
    ),
    (
        "HIRSEL_TEST_BINARY",
        Class::HostOnly("tools/buck2/test_launcher.sh"),
    ),
    (
        "HIRSEL_TEST_BIN_EXES",
        Class::HostOnly("tools/buck2/test_launcher.sh"),
    ),
    (
        "HIRSEL_TEST_BIN_EXE_DIR",
        Class::HostOnly("tools/buck2/test_launcher.sh"),
    ),
    (
        "HIRSEL_TEST_CASE_CPUS",
        Class::HostOnly("tools/buck2/test_launcher.sh"),
    ),
    (
        "HIRSEL_TEST_CASE_FORMATTER",
        Class::HostOnly("tools/tests/fmt.py"),
    ),
    (
        "HIRSEL_TEST_COSTS",
        Class::HostOnly("tools/buck2/test_launcher.sh"),
    ),
    (
        "HIRSEL_TEST_CPUS",
        Class::HostOnly("tools/buck2/test_launcher.sh"),
    ),
    (
        "HIRSEL_TEST_CPU_GROUP",
        Class::HostOnly("tools/buck2/test_launcher.sh"),
    ),
    (
        "HIRSEL_TEST_CWD",
        Class::HostOnly("tools/buck2/test_launcher.sh"),
    ),
    (
        "HIRSEL_TEST_DEDICATED_CASE",
        Class::HostOnly("tools/buck2/test_launcher.sh"),
    ),
    (
        "HIRSEL_TEST_EMPTY_PARTITION",
        Class::HostOnly("tools/buck2/test_launcher.sh"),
    ),
    (
        "HIRSEL_TEST_EXACT_CPUS",
        Class::HostOnly("tools/buck2/test_launcher.sh"),
    ),
    (
        "HIRSEL_TEST_EXECUTOR",
        Class::HostOnly("tools/tests/buck2_launcher.py"),
    ),
    (
        "HIRSEL_TEST_FINISHED",
        Class::HostOnly("tools/buck2/consumer_results.py"),
    ),
    (
        "HIRSEL_TEST_HISTORY",
        Class::HostOnly("tools/buck2/event_logs.py"),
    ),
    (
        "HIRSEL_TEST_HOST_CASES",
        Class::HostOnly("tools/tests/buck2_launcher.py"),
    ),
    (
        "HIRSEL_TEST_HOST_ONLY",
        Class::HostOnly("tools/tests/buck2_launcher.py"),
    ),
    (
        "HIRSEL_TEST_INNER_DEADLINE",
        Class::HostOnly("tools/buck2/consumer_results.py"),
    ),
    (
        "HIRSEL_TEST_INTERRUPTED",
        Class::HostOnly("tools/buck2/test_report.py"),
    ),
    (
        "HIRSEL_TEST_ISOLATED_CASES",
        Class::HostOnly("tools/buck2/test_launcher.sh"),
    ),
    ("HIRSEL_TEST_LAND", Class::HostOnly("tools/tests/land.py")),
    (
        "HIRSEL_TEST_LAUNCHER",
        Class::HostOnly("tools/tests/buck2_launcher.py"),
    ),
    (
        "HIRSEL_TEST_NATIVE_ENVIRONMENT",
        Class::HostOnly("tools/tests/buck2_launcher.py"),
    ),
    (
        "HIRSEL_TEST_PHASE_RUNNING",
        Class::HostOnly("tools/buck2/test_report.py"),
    ),
    (
        "HIRSEL_TEST_PHASE_TOTAL",
        Class::HostOnly("tools/buck2/test_report.py"),
    ),
    (
        "HIRSEL_TEST_POOL",
        Class::HostOnly("tools/buck2/test_launcher.sh"),
    ),
    (
        "HIRSEL_TEST_PRIVATE_NATIVE_INPUTS",
        Class::HostOnly("tools/tests/buck2_launcher.py"),
    ),
    (
        "HIRSEL_TEST_PRODUCTS",
        Class::HostOnly("tools/buck2/test_launcher.sh"),
    ),
    (
        "HIRSEL_TEST_PRODUCT_DEFINITIONS",
        Class::HostOnly("tools/buck2/defs.bzl"),
    ),
    (
        "HIRSEL_TEST_RELOCATE",
        Class::HostOnly("tools/buck2/test_launcher.sh"),
    ),
    (
        "HIRSEL_TEST_REPORTER",
        Class::HostOnly("tools/buck2/test_launcher.sh"),
    ),
    (
        "HIRSEL_TEST_REPORT_ACTIVE",
        Class::HostOnly("tools/buck2/test_launcher.sh"),
    ),
    (
        "HIRSEL_TEST_REPORT_FIXTURE",
        Class::HostOnly("tools/tests/buck2_launcher.py"),
    ),
    (
        "HIRSEL_TEST_RUNNING",
        Class::HostOnly("tools/buck2/consumer_results.py"),
    ),
    (
        "HIRSEL_TEST_STARTED",
        Class::HostOnly("tools/buck2/consumer_results.py"),
    ),
    (
        "HIRSEL_TEST_TMPDIR",
        Class::HostOnly("tools/buck2/test_launcher.sh"),
    ),
    ("HIRSEL_WORKERS_TSAN", Class::HostOnly("tools/workers-tsan")),
];
const PREFIXES: &[(&str, Class)] = &[
    ("LC_", Class::Key),
    ("HIRSEL_TIMING", Class::Telemetry),
    ("HIRSEL_FREE_", Class::Telemetry),
    ("HIRSEL_RT_", Class::Launch),
];

fn class(name: &str) -> Option<Class> {
    VARIABLES
        .iter()
        .find(|(known, _)| *known == name)
        .or_else(|| PREFIXES.iter().find(|(prefix, _)| name.starts_with(prefix)))
        .map(|(_, class)| *class)
}

/// The key and owner rows of this process's environment, sorted by name; the
/// bypass reason when a variable bypasses B.
fn environment() -> Result<Vec<(String, Vec<u8>)>, String> {
    use std::os::unix::ffi::OsStrExt;
    let mut rows = Vec::new();
    for (name, value) in std::env::vars_os() {
        let Some(name) = name.to_str().map(str::to_owned) else {
            return Err(format!(
                "variable {} has a non-UTF-8 name",
                name.to_string_lossy()
            ));
        };
        let class = match class(&name) {
            Some(class) => class,
            None if name.starts_with("HIRSEL_") => {
                return Err(format!("variable {name} is not in the inventory"));
            }
            None => continue,
        };
        match class {
            // `HIRSEL_VERIFY_CACHE=0` asks for no recheck; every other value rechecks.
            Class::Bypass if name == "HIRSEL_VERIFY_CACHE" && value == "0" => {}
            Class::Bypass => return Err(format!("variable {name} is a bypass mode")),
            Class::Key => rows.push((name, value.as_bytes().to_vec())),
            Class::Owner => {
                let path = std::fs::canonicalize(&value).unwrap_or_else(|_| PathBuf::from(&value));
                rows.push((name, path.as_os_str().as_bytes().to_vec()));
            }
            Class::Acquisition
            | Class::Telemetry
            | Class::Launch
            | Class::CrossTool
            | Class::HostOnly(_) => {}
        }
    }
    rows.sort();
    Ok(rows)
}

/// The cross-tool rows of this process's environment, sorted by name: a
/// build for the target that uses them keys them.
fn cross_tools() -> Vec<(String, Vec<u8>)> {
    use std::os::unix::ffi::OsStrExt;
    let mut rows: Vec<_> = VARIABLES
        .iter()
        .filter(|(_, class)| *class == Class::CrossTool)
        .filter_map(|(name, _)| {
            std::env::var_os(name).map(|value| (name.to_string(), value.as_bytes().to_vec()))
        })
        .collect();
    rows.sort();
    rows
}

/// Note that this request does not use B, in the timings report and, under
/// `HIRSEL_PROGRAM_RECORD_DEBUG`, on stderr.
fn bypassed(reason: String) {
    if std::env::var_os("HIRSEL_PROGRAM_RECORD_DEBUG").is_some() {
        eprintln!("hirsel: program record bypassed: {reason}");
    }
    timings::program_bypass(reason);
}

/// The running compiler's own bytes, by stat: this executable and every
/// libLLVM it mapped. A rebuilt or replaced compiler is another key even when
/// its source identity is equal.
fn compiler_guard() -> Option<Vec<u8>> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;
    let mut paths = vec![std::env::current_exe().ok()?];
    if let Ok(maps) = std::fs::read_to_string("/proc/self/maps") {
        for line in maps.lines() {
            if let Some(path) = line.split_whitespace().nth(5)
                && path.contains("libLLVM")
                && !paths.iter().any(|known| known.as_os_str() == path)
            {
                paths.push(PathBuf::from(path));
            }
        }
    }
    let mut guard = Vec::new();
    for path in paths {
        let stat = std::fs::metadata(&path).ok()?;
        guard.extend_from_slice(path.as_os_str().as_bytes());
        for value in [
            stat.dev(),
            stat.ino(),
            stat.size(),
            stat.mtime() as u64,
            stat.mtime_nsec() as u64,
            stat.ctime() as u64,
            stat.ctime_nsec() as u64,
        ] {
            guard.extend_from_slice(&value.to_le_bytes());
        }
    }
    Some(guard)
}

/// One request's selected record domain and keys.
struct Keyed {
    records: ProgramRecords,
    package: String,
    /// The source key: equal for a check and a build of the same inputs.
    source: ContentHash,
}

/// The source key P of the session's current observation, under its
/// invocation's registration, or why B is not used.
fn keyed(session: &Session, entry: &Path, policy: Policy) -> Result<Keyed, String> {
    if policy.verify_store == hirsel_engine_store::VerifyStore::Recompute {
        return Err("mode recompute".into());
    }
    let authority = {
        let _phase = timings::phase(Phase::ProgramRegistration);
        session
            .compiler()
            .configuration()
            .store
            .program_authority()
            .ok_or("no registration")?
    };
    let _phase = timings::phase(Phase::ProgramKey);
    let facts = session.program_facts().ok_or("no program facts")?;
    if facts.foreign {
        return Err("foreign links".into());
    }
    let inputs = session.current_inputs().ok_or("no observed inputs")?;
    let environment = environment()?;
    let guard = compiler_guard().ok_or("compiler guard unreadable")?;
    let directory = std::env::current_dir().map_err(|_| "no current directory")?;
    let compiler = session.compiler();
    let installation = compiler.installation_key(inputs);
    let identity = compiler.identity();
    let resolved = format!("{:?}", facts.entry);
    let settings = format!(
        "{:?}/{:?}/{:?}/{:?}",
        policy.verify,
        policy.results,
        policy.verify_store,
        compiler.configuration().lowering
    );
    use std::os::unix::ffi::OsStrExt;
    let mut parts: Vec<Vec<u8>> = vec![
        installation.0.to_vec(),
        identity.0.to_vec(),
        resolved.into_bytes(),
        guard,
        settings.into_bytes(),
        entry.as_os_str().as_bytes().to_vec(),
        directory.as_os_str().as_bytes().to_vec(),
    ];
    for (name, value) in environment {
        parts.push(name.into_bytes());
        parts.push(value);
    }
    let source = ContentHash::of_parts("hirsel.program.key.v1", parts.iter().map(Vec::as_slice));
    let records = authority
        .records()
        .map_err(|error| format!("record domain: {error}"))?;
    Ok(Keyed {
        records,
        package: format!("{:?}", facts.entry.0.package),
        source,
    })
}

fn check_address(keyed: &Keyed) -> ContentHash {
    ContentHash::of_parts(
        "hirsel.program.address.v1",
        [b"check".as_slice(), keyed.package.as_bytes()],
    )
}

/// D: what a replay prints, with the source text its diagnostics cite.
#[derive(serde::Serialize, serde::Deserialize)]
struct Replay {
    /// Path, logical path, text and ordinal of each cited source.
    files: Vec<(PathBuf, PathBuf, String, u32)>,
    /// Each anchor the diagnostics name, in codec order: (file index, base).
    anchors: Vec<(u32, u32)>,
    diagnostics: serde_json::Value,
    report: Vec<String>,
}

fn encode(
    diagnostics: &[hirsel_diag::Diagnostic],
    sources: &SourceMap,
    report: &[String],
) -> Option<Vec<u8>> {
    let mut codec = AnchorCodec::Collect(vec![]);
    let value =
        hirsel_span::with_anchor_codec(&mut codec, || serde_json::to_value(diagnostics)).ok()?;
    let AnchorCodec::Collect(collected) = codec else {
        return None;
    };
    let mut files: Vec<(PathBuf, PathBuf, String, u32)> = vec![];
    let mut ids = vec![];
    let mut anchors = vec![];
    for anchor in collected {
        let (file, base) = sources.anchor(anchor)?;
        let index = match ids.iter().position(|id| *id == file) {
            Some(index) => index,
            None => {
                let source = sources.file(file)?;
                files.push((
                    source.path.clone(),
                    source.logical_path().to_owned(),
                    source.text().to_owned(),
                    source.ordinal(),
                ));
                ids.push(file);
                ids.len() - 1
            }
        };
        anchors.push((index as u32, base));
    }
    serde_json::to_vec(&Replay {
        files,
        anchors,
        diagnostics: value,
        report: report.to_vec(),
    })
    .ok()
}

fn decode(bytes: &[u8]) -> Option<(Vec<hirsel_diag::Diagnostic>, SourceMap, Vec<String>)> {
    let replay: Replay = serde_json::from_slice(bytes).ok()?;
    let mut sources = SourceMap::default();
    let mut ids = vec![];
    for (path, logical, text, ordinal) in replay.files {
        ids.push(sources.add_replayed(path, logical, text, ordinal)?);
    }
    let mut anchors = vec![];
    for (index, base) in replay.anchors {
        let file = *ids.get(index as usize)?;
        anchors.push(sources.item_anchor(file, base));
    }
    let mut codec = AnchorCodec::Decode(anchors);
    let diagnostics = hirsel_span::with_anchor_codec(&mut codec, || {
        serde_json::from_value::<Vec<hirsel_diag::Diagnostic>>(replay.diagnostics)
    })
    .ok()?;
    Some((diagnostics, sources, replay.report))
}

fn rejected(diagnostics: &[hirsel_diag::Diagnostic]) -> bool {
    diagnostics.iter().any(|d| d.severity.as_str() == "error")
}

fn publish(keyed: &Keyed, record: ProgramRecord<'_>, generation: u64) {
    let _phase = timings::phase(Phase::ProgramPublish);
    if let Ok(hirsel_validate::programs::Published::Committed { .. }) =
        keyed.records.publish(&record, generation)
    {
        timings::count(Count::ProgramRecordPublications);
    }
}

/// `hirsel check` of `entry` through B.
pub(crate) fn check(
    session: &mut Session,
    entry: &Path,
    policy: Policy,
) -> Result<CheckOutput, ToolError> {
    session.invocation(|session| {
        let observed = {
            let _phase = timings::phase(Phase::ProgramKey);
            session.observe_inputs()
        };
        if let Err(error) = observed {
            bypassed("inputs not observed".into());
            return crate::resident::check_error(error);
        }
        let keyed = match keyed(session, entry, policy) {
            Ok(keyed) => keyed,
            Err(reason) => {
                bypassed(reason);
                return session.check_applied();
            }
        };
        let address = check_address(&keyed);
        let looked = {
            let _phase = timings::phase(Phase::ProgramLookup);
            keyed
                .records
                .lookup(address, ProgramOperation::Check, |_| Some(keyed.source))
        };
        let generation = match looked {
            Lookup::Hit(hit) => {
                let _phase = timings::phase(Phase::ProgramReplay);
                if let Some((diagnostics, sources, _)) = decode(&hit.diagnostics) {
                    timings::count(Count::ProgramRecordHits);
                    return Ok(CheckOutput {
                        completion: CheckCompletion::Complete,
                        sources,
                        diagnostics,
                        unsafe_ffi: vec![],
                    });
                }
                None
            }
            Lookup::Miss { generation } => Some(generation),
        };
        timings::count(Count::ProgramRecordMisses);
        let checked = session.check_applied();
        if let (Ok(output), Some(generation)) = (&checked, generation)
            && output.completion == CheckCompletion::Complete
            && output.unsafe_ffi.is_empty()
            && let Some(diagnostics) = encode(&output.diagnostics, &output.sources, &[])
        {
            let failed = rejected(&output.diagnostics);
            publish(
                &keyed,
                ProgramRecord {
                    operation: ProgramOperation::Check,
                    kind: if failed {
                        ProgramKind::SourceDiagnostics
                    } else {
                        ProgramKind::Success
                    },
                    status: failed as u8,
                    address,
                    key: keyed.source,
                    image: None,
                    diagnostics: &diagnostics,
                    executable: None,
                },
                generation,
            );
        }
        checked
    })
}

/// One `hirsel build` request.
pub(crate) struct Build<'a> {
    pub output: &'a Path,
    pub options: BuildOptions,
    pub evidence: Option<&'a hirsel_native::Evidence>,
    pub profile: hirsel_native::BuildProfile,
    pub backend: hirsel_native::Backend,
    pub validation: hirsel_native::Validation,
}

type Built = Result<Result<Vec<String>, NativeError>, ToolError>;

/// `hirsel build` of `entry` through B. A build record never answers a
/// check: its address names the operation. A source failure is keyed by the
/// source key alone and is decided before any platform capture; a success
/// is keyed by the source key, the native options, the evidence and the
/// platform this build captures fresh, under the build's own capture mode.
pub(crate) fn build(
    session: &mut Session,
    entry: &Path,
    policy: Policy,
    build: Build<'_>,
) -> Built {
    session.invocation(|session| {
        let mut request = crate::requests::NativeRequest {
            options: build.options.clone(),
            run: None,
            definition: None,
            test_entries: vec![],
            supplemental_sources: vec![],
            output: crate::requests::NativeOutput::Executable,
            evidence: build.evidence.cloned().map(std::sync::Arc::new),
            platform: None,
        };
        let run = |session: &mut Session, request: crate::requests::NativeRequest| {
            session.request_applied(crate::requests::CompileOperation::NativeBuild(request))
        };
        let observed = {
            let _phase = timings::phase(Phase::ProgramKey);
            session.observe_inputs()
        };
        if observed.is_err() {
            bypassed("inputs not observed".into());
        }
        observed?;
        // Without prebuilt runtime archives the link builds them with the
        // Cargo found on PATH (hirsel-native runtime_cache.rs), whose inputs
        // P does not hold.
        let keyed = if std::env::var_os("HIRSEL_RUNTIME_ARCHIVES").is_none() {
            Err("runtime archives from the Cargo fallback".to_owned())
        } else {
            keyed(session, entry, policy)
        };
        let keyed = match keyed {
            Ok(keyed) => keyed,
            Err(reason) => {
                bypassed(reason);
                return finish(run(session, request), build.output, None);
            }
        };
        let evidence = build
            .evidence
            .map(crate::requests::evidence_identity)
            .unwrap_or_default();
        let address = ContentHash::of_parts(
            "hirsel.program.address.v1",
            [
                b"build".as_slice(),
                keyed.package.as_bytes(),
                format!(
                    "{:?}/{:?}/{:?}/{:?}",
                    build.profile, build.options.passes.target, build.backend, build.validation
                )
                .as_bytes(),
                &evidence.0,
            ],
        );
        let mut options = format!("{:?}", build.options).into_bytes();
        if build.options.passes.target.triple.starts_with("aarch64") {
            for (name, value) in cross_tools() {
                options.extend_from_slice(name.as_bytes());
                options.push(0);
                options.extend_from_slice(&value);
                options.push(0);
            }
        }
        let success = |platform: ContentHash| {
            ContentHash::of_parts(
                "hirsel.program.build-key.v1",
                [
                    &keyed.source.0[..],
                    options.as_slice(),
                    &evidence.0,
                    &platform.0,
                ],
            )
        };
        // The lookup's selection, captured into the operation store, is handed
        // to a miss so the build does not capture the platform again.
        let store = session.compiler().configuration().store.clone();
        let mut captured = None;
        let looked = {
            let _phase = timings::phase(Phase::ProgramLookup);
            keyed
                .records
                .lookup(address, ProgramOperation::Build, |kind| match kind {
                    ProgramKind::SourceDiagnostics => Some(keyed.source),
                    ProgramKind::Success => {
                        let _phase = timings::phase(Phase::ProgramPlatform);
                        let handed =
                            crate::requests::host_executable_platform(&store, &build.options)
                                .ok()?;
                        let platform = handed.identity();
                        captured = Some(std::sync::Arc::new(handed));
                        Some(success(platform))
                    }
                })
        };
        let generation = match looked {
            Lookup::Hit(hit) => match replay_build(
                &hit,
                captured.as_ref().map(|handed| handed.identity()),
                build.output,
            ) {
                Some(replayed) => {
                    timings::count(Count::ProgramRecordHits);
                    return replayed;
                }
                None => None,
            },
            Lookup::Miss { generation } => Some(generation),
        };
        timings::count(Count::ProgramRecordMisses);
        request.platform = captured;
        let selected = run(session, request);
        let Some(generation) = generation else {
            return finish(selected, build.output, None);
        };
        finish(
            selected,
            build.output,
            Some(&|recorded: Recorded<'_>| {
                let (kind, status, key, image, executable, diagnostics) = match recorded {
                    Recorded::Source {
                        diagnostics,
                        sources,
                    } => (
                        ProgramKind::SourceDiagnostics,
                        1,
                        keyed.source,
                        None,
                        None,
                        encode(diagnostics, sources, &[]),
                    ),
                    Recorded::Success {
                        report,
                        executable,
                        platform,
                    } => (
                        ProgramKind::Success,
                        0,
                        success(platform),
                        Some(image_binding(ContentHash::of_bytes(executable), platform)),
                        Some(executable),
                        encode(&[], &SourceMap::default(), report),
                    ),
                };
                let Some(diagnostics) = diagnostics else {
                    return;
                };
                publish(
                    &keyed,
                    ProgramRecord {
                        operation: ProgramOperation::Build,
                        kind,
                        status,
                        address,
                        key,
                        image,
                        diagnostics: &diagnostics,
                        executable,
                    },
                    generation,
                );
            }),
        )
    })
}

/// What a fresh build may record.
enum Recorded<'a> {
    Source {
        diagnostics: &'a [hirsel_diag::Diagnostic],
        sources: &'a SourceMap,
    },
    Success {
        report: &'a [String],
        executable: &'a [u8],
        platform: ContentHash,
    },
}

/// Materialize a fresh build's output, recording what may be recorded: a
/// success after its executable is written and validated, or a request that
/// completed with source diagnostics. A native failure, another error or a
/// failed materialization is never recorded.
fn finish(
    selected: Result<std::sync::Arc<crate::requests::CompileOutput>, ToolError>,
    output: &Path,
    record: Option<&dyn Fn(Recorded<'_>)>,
) -> Built {
    let selected = match selected {
        Ok(selected) => selected,
        Err(ToolError::Query {
            error: QueryError::Diagnostics(diagnostics),
            phase,
            sources,
        }) => {
            if let Some(record) = record {
                record(Recorded::Source {
                    diagnostics: &diagnostics,
                    sources: &sources,
                });
            }
            return Err(ToolError::Query {
                error: QueryError::Diagnostics(diagnostics),
                phase,
                sources,
            });
        }
        Err(error) => return Err(error),
    };
    match &selected.result {
        crate::requests::CompileResult::Native(input) => {
            let report = crate::native::request_output::materialize_output(input, output);
            if let (Ok(report), Some(record)) = (&report, record)
                && let Ok(linked) = hirsel_native::object_queries::linked_product(
                    input.store.as_ref(),
                    input.product.clone(),
                )
                && let Ok(executable) =
                    hirsel_native::object_queries::executable_bytes(input.store.as_ref(), &linked)
            {
                record(Recorded::Success {
                    report,
                    executable: &executable,
                    platform: input.platform_identity(),
                });
            }
            Ok(report)
        }
        crate::requests::CompileResult::NativeError(error) => Ok(Err(error.clone())),
        _ => Err(ToolError::Runtime(
            "native build request returned another operation".into(),
        )),
    }
}

/// Answer a build from its record. A source failure replays its diagnostics.
/// A success writes the recorded executable and reuses its image validation
/// only when the record binds it to this executable and the platform this
/// build captured now. Anything else is a miss.
fn replay_build(hit: &ProgramHit, platform: Option<ContentHash>, output: &Path) -> Option<Built> {
    let (diagnostics, sources, report) = {
        let _phase = timings::phase(Phase::ProgramReplay);
        decode(&hit.diagnostics)?
    };
    match hit.header.kind {
        ProgramKind::SourceDiagnostics => Some(Err(ToolError::Query {
            error: QueryError::Diagnostics(diagnostics),
            phase: None,
            sources,
        })),
        ProgramKind::Success => {
            let (digest, _) = hit.header.executable?;
            if hit.header.image != Some(image_binding(digest, platform?)) {
                return None;
            }
            let _phase = timings::phase(Phase::ProgramMaterialize);
            hit.materialize(output).ok()?;
            Some(Ok(Ok(report)))
        }
    }
}
