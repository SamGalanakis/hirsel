//! Program record B (native rerun design v4.1, N1 and N1b). A check, build
//! or `run --native` computes its key P from the observed inputs without
//! opening a stored product, then looks up one record in the domain its
//! registration authenticates. A hit replays the record's diagnostics, report
//! and status; a build hit also materializes the recorded executable, and a
//! run hit launches it from its execution descriptor R with no request. A
//! miss runs as before and publishes one record. Only the registration grants authority: with no registration,
//! a copied store or a replaced one, every lookup misses.
//!
//! P is an explicit inventory. Every `HIRSEL_*` and tool variable is in one
//! class: key (its value is in P), native (its value is in a build's and a
//! run's success key, never a check's), owner (its canonical identity is in
//! P), bypass (B is not used), acquisition (how work is scheduled or
//! admitted, never what it produces), telemetry, or launch-only. An
//! unclassified `HIRSEL_*` that is set bypasses B.
use crate::launch_binding;
use crate::session::{Policy, Session};
use hirsel_artifact_store::programs::{
    Lookup, ProgramAuthority, ProgramHit, ProgramKind, ProgramOperation, ProgramRecord,
    ProgramRecords, image_binding,
};
use hirsel_engine::{CheckCompletion, CheckOutput, QueryError, ToolError};
use hirsel_intern::timings::{self, Count, Phase};
use hirsel_native::{BuildOptions, NativeError};
use hirsel_span::{AnchorCodec, SourceMap};
use hirsel_syntax::ContentHash;
use std::path::{Path, PathBuf};

/// How P treats one environment variable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Class {
    /// Its value selects what is produced: the value is in P.
    Key,
    /// A native tool or archive a build or run links with and a check never
    /// reads: its value is in a build's and a run's success key, not in the
    /// source key a check and a failing build or run share.
    Native,
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
    ("HIRSEL_AUDITOR_PACKAGE", Class::Native),
    ("HIRSEL_CAPTURE_LINKER", Class::Native),
    ("HIRSEL_GLIBC_FLOOR", Class::Native),
    ("HIRSEL_GUARD_ORACLE", Class::Key),
    ("HIRSEL_LINK_KIT", Class::Native),
    ("HIRSEL_RUNTIME_ARCHIVES", Class::Native),
    (
        "HIRSEL_CORE_RT_ARCHIVE",
        Class::HostOnly("CoreIR LLVM executable test runtime"),
    ),
    // The emulator runs a built aarch64 program; no build reads it, and
    // `run --native` launches only x86_64 programs.
    ("HIRSEL_QEMU_AARCH64", Class::Launch),
    // Memcheck runtimes are built and launched only by `test --native`,
    // which does not use B.
    ("HIRSEL_VALGRIND", Class::Launch),
    ("HIRSEL_VALGRIND_RUNTIME", Class::Launch),
    ("HIRSEL_LLVM_NO_FACTS", Class::Native),
    ("LLVM_SYS_231_PREFIX", Class::Native),
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
    ("HIRSEL_VALIDATE_STAGES", Class::Bypass),
    ("HIRSEL_CARRIED_ROW_RECHECK", Class::Bypass),
    ("HIRSEL_INSTANCE_RECOMPUTATION_DIR", Class::Bypass),
    ("HIRSEL_NATIVE_RETENTION", Class::Bypass),
    ("HIRSEL_OPERATION_OWNER_TRACE", Class::Bypass),
    ("HIRSEL_VALIDATE_PROFILE", Class::Bypass),
    ("HIRSEL_OWNERSHIP_TRACE", Class::Bypass),
    // Keeping native scratch is for inspecting a build's work, which a hit
    // does not do; a failed hit's kept scratch would be reported as the
    // build's.
    ("HIRSEL_KEEP_NATIVE_SCRATCH", Class::Bypass),
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
    ("HIRSEL_CAPPED_SLOTS", Class::HostOnly("tools/capped")),
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
        "HIRSEL_MAIN_RUN_PROGRESS",
        Class::HostOnly("tools/buck2/event_logs.py"),
    ),
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
    ("HIRSEL_PRIORITY", Class::HostOnly("tools/check-native.sh")),
    (
        "HIRSEL_PRIVATE_NATIVE_INPUTS",
        Class::HostOnly("tools/buck2/test_launcher.sh"),
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
        Class::HostOnly("tools/buck2/test_launcher.sh"),
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
        "HIRSEL_CORE_RUNTIME_ARCHIVE",
        Class::HostOnly("CoreIR native executable tests"),
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
    // Read by qemu when it runs a built aarch64 program. Platform discovery
    // runs the emulator with a cleared environment, so no build reads them.
    ("QEMU_", Class::Launch),
];

fn class(name: &str) -> Option<Class> {
    VARIABLES
        .iter()
        .find(|(known, _)| *known == name)
        .or_else(|| PREFIXES.iter().find(|(prefix, _)| name.starts_with(prefix)))
        .map(|(_, class)| *class)
}

/// The rows of this process's environment, sorted by name: the key and owner
/// rows, then the native rows; the bypass reason when a variable bypasses B.
type Rows = Vec<(String, Vec<u8>)>;
fn environment() -> Result<(Rows, Rows), String> {
    use std::os::unix::ffi::OsStrExt;
    let mut rows = Vec::new();
    let mut native = Vec::new();
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
            // The CLI validates stages only for `HIRSEL_VALIDATE_STAGES=1`.
            Class::Bypass if name == "HIRSEL_VALIDATE_STAGES" && value != "1" => {}
            Class::Bypass => return Err(format!("variable {name} is a bypass mode")),
            Class::Key => rows.push((name, value.as_bytes().to_vec())),
            Class::Native => native.push((name, value.as_bytes().to_vec())),
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
    native.sort();
    Ok((rows, native))
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

/// The running compiler's own bytes (native rerun design §15 R3): every
/// file-backed executable mapping of this process (the executable, the
/// loader, libc, libLLVM and every other library the backend loaded at
/// startup; nothing is loaded later), each by the content digest of the very
/// inode it mapped. Only code generation runs the backend (the libLLVM
/// mappings), so every request keys the rest (the compiler) and a build or
/// run also keys the backend. A rebuilt or replaced
/// compiler or library is another key even when its source identity is
/// equal; a file whose stat changed but whose bytes did not (a hard link
/// added elsewhere moves a shared inode's ctime) keeps its key at the cost of
/// one rehash. A file replaced or deleted after this process loaded it, or
/// unreadable, bypasses B: the bytes on disk are then not the running code.
/// The descriptors hashed are held, and rechecked before any record is
/// published.
fn compiler_guard(authority: &ProgramAuthority) -> Result<(Vec<u8>, Vec<u8>, Vec<Held>), String> {
    use std::os::unix::ffi::OsStrExt;
    let cache = hirsel_artifact_store::registration::Installation::default_state()
        .map(|state| state.join("compiler-digests"))
        .filter(|dir| {
            use std::os::unix::fs::DirBuilderExt;
            let _ = std::fs::DirBuilder::new().mode(0o700).create(dir);
            std::fs::symlink_metadata(dir).is_ok_and(|meta| meta.is_dir())
        });
    let started = std::time::SystemTime::now();
    let (mut compiler, mut backend, mut held) = (Vec::new(), Vec::new(), Vec::new());
    for loaded in loaded_compiler()? {
        let digested = compiler_file_digest(
            cache.as_deref().map(|cache| (cache, authority)),
            &loaded,
            started,
        )?;
        let guard = if loaded.path.to_string_lossy().contains("libLLVM") {
            &mut backend
        } else {
            &mut compiler
        };
        guard.extend_from_slice(loaded.path.as_os_str().as_bytes());
        guard.extend_from_slice(&digested.digest.0);
        held.push(digested.held);
    }
    Ok((compiler, backend, held))
}

/// One compiler file this process runs: the path it was loaded from and the
/// (device, inode) it mapped.
struct Loaded {
    path: PathBuf,
    mapped: (u64, u64),
}

/// The running executable, through `/proc/self/exe`, and every other
/// file-backed executable mapping, each bound to the device and inode
/// `/proc/self/maps` names for it; in path order.
fn loaded_compiler() -> Result<Vec<Loaded>, String> {
    use std::os::unix::fs::MetadataExt;
    let running =
        std::fs::metadata("/proc/self/exe").map_err(|_| "compiler executable unreadable")?;
    let path =
        std::fs::read_link("/proc/self/exe").map_err(|_| "compiler executable unreadable")?;
    if path.to_string_lossy().ends_with(" (deleted)") {
        return Err("compiler executable deleted after load".into());
    }
    let mut loaded = vec![Loaded {
        path,
        mapped: (running.dev(), running.ino()),
    }];
    let maps =
        std::fs::read_to_string("/proc/self/maps").map_err(|_| "compiler mappings unreadable")?;
    for line in maps.lines() {
        // address, permissions, offset, device (major:minor, hex), inode, path.
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 6 || !fields[1].contains('x') {
            continue;
        }
        let path = fields[5..].join(" ");
        // Pseudo mappings ([vdso], [vsyscall]) are the kernel's, not a file.
        if !path.starts_with('/') {
            continue;
        }
        if path.ends_with(" (deleted)") {
            return Err(format!("compiler file {path} deleted after load"));
        }
        let unreadable = || format!("compiler mapping of {path} unreadable");
        let (major, minor) = fields[3].split_once(':').ok_or_else(unreadable)?;
        let major = u32::from_str_radix(major, 16).map_err(|_| unreadable())?;
        let minor = u32::from_str_radix(minor, 16).map_err(|_| unreadable())?;
        let inode: u64 = fields[4].parse().map_err(|_| unreadable())?;
        let mapped = (libc::makedev(major, minor), inode);
        let path = PathBuf::from(path);
        match loaded.iter().find(|known| known.path == path) {
            Some(known) if known.mapped != mapped => {
                return Err(format!("compiler file {} mapped twice", path.display()));
            }
            Some(_) => {}
            None => loaded.push(Loaded { path, mapped }),
        }
    }
    loaded.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(loaded)
}

/// A hashed compiler file's descriptor and the stamp its digest was taken
/// under, held until the request's record is published.
struct Held {
    path: PathBuf,
    file: std::fs::File,
    stamp: hirsel_native::file_digest::Stamp,
}

impl Held {
    /// Whether the held descriptor and the path still show the stamp the
    /// digest was taken under.
    fn unchanged(&self) -> bool {
        use hirsel_native::file_digest::stamp;
        self.file
            .metadata()
            .is_ok_and(|meta| stamp(&meta) == self.stamp)
            && std::fs::metadata(&self.path).is_ok_and(|meta| stamp(&meta) == self.stamp)
    }
}

/// One loaded compiler file's content digest, whether it was rehashed, and
/// its held descriptor.
struct Digested {
    digest: ContentHash,
    rehashed: bool,
    held: Held,
}

/// One loaded compiler file's content digest. The file opened at its path
/// must be the inode the process mapped. The digest is cached under the
/// file's full stamp (ctime included) and the registration's MAC, issued
/// only for a stamp equal before and after a full read and old enough to be
/// stable (hirsel-native `file_digest::stable`: same-tick writes and clock
/// rollback are never issued and never reused); any other stamp rehashes the
/// bytes for this run only, so changed bytes are a new digest (a miss) and
/// equal bytes the same one.
fn compiler_file_digest(
    cache: Option<(&Path, &ProgramAuthority)>,
    loaded: &Loaded,
    started: std::time::SystemTime,
) -> Result<Digested, String> {
    use hirsel_native::file_digest::{stable, stamp, verified_digests};
    use std::io::Write;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;
    let path = &loaded.path;
    let unreadable = || format!("compiler file {} unreadable", path.display());
    let mut file = std::fs::File::open(path).map_err(|_| unreadable())?;
    let meta = file.metadata().map_err(|_| unreadable())?;
    if (meta.dev(), meta.ino()) != loaded.mapped {
        return Err(format!(
            "compiler file {} replaced after load",
            path.display()
        ));
    }
    let state = stamp(&meta);
    let encode = |state: &hirsel_native::file_digest::Stamp| -> Vec<u8> {
        state.iter().flat_map(|word| word.to_le_bytes()).collect()
    };
    let seal = |authority: &ProgramAuthority, state: &[u8], digest: &[u8; 32]| {
        let mut payload = path.as_os_str().as_bytes().to_vec();
        payload.push(0);
        payload.extend_from_slice(state);
        payload.extend_from_slice(digest);
        authority.compiler_digest_mac(&payload)
    };
    let entry = cache.map(|(dir, authority)| {
        let name = ContentHash::of_parts(
            "hirsel.program.compiler-file.v1",
            [path.as_os_str().as_bytes()],
        );
        let name = name
            .0
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        (dir.join(name), authority)
    });
    let known = encode(&state);
    if stable(&state, started)
        && let Some((entry, authority)) = entry.as_ref()
        && let Ok(cached) = std::fs::read(entry)
        && cached.len() == known.len() + 64
        && cached[..known.len()] == known[..]
    {
        let digest: [u8; 32] = cached[known.len()..known.len() + 32]
            .try_into()
            .expect("length checked");
        if cached[known.len() + 32..] == seal(authority, &known, &digest) {
            return Ok(Digested {
                digest: ContentHash(digest),
                rehashed: false,
                held: Held {
                    path: path.clone(),
                    file,
                    stamp: state,
                },
            });
        }
    }
    let (state, digests) = verified_digests(path, &mut file, || {})
        .map_err(|_| format!("compiler file {} changed while hashed", path.display()))?;
    timings::count(Count::ProgramCompilerRehashes);
    // Only a verified, stable (stamp, digest) pair is issued.
    if stable(&state, started)
        && let Some((entry, authority)) = entry
        && let Ok(mut temporary) =
            tempfile::NamedTempFile::new_in(entry.parent().expect("cache entry"))
    {
        let known = encode(&state);
        let _ = temporary
            .write_all(&known)
            .and_then(|()| temporary.write_all(&digests.content.0))
            .and_then(|()| temporary.write_all(&seal(authority, &known, &digests.content.0)))
            .map_err(drop)
            .and_then(|()| temporary.persist(&entry).map(drop).map_err(drop));
    }
    Ok(Digested {
        digest: digests.content,
        rehashed: true,
        held: Held {
            path: path.clone(),
            file,
            stamp: state,
        },
    })
}

/// One request's selected record domain and keys.
struct Keyed {
    records: ProgramRecords,
    package: String,
    /// The source key: equal for a check and a build of the same inputs.
    source: ContentHash,
    /// What only code generation reads, which a build's and a run's success
    /// keys add: the native rows and the loaded libLLVM files.
    native: Vec<u8>,
    /// The compiler files the key's guard hashed, rechecked before publication.
    held: Vec<Held>,
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
    let (environment, native_rows) = environment()?;
    let (guard, backend, held) = compiler_guard(&authority)?;
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
    let mut native = backend;
    for (name, value) in native_rows {
        native.extend_from_slice(name.as_bytes());
        native.push(0);
        native.extend_from_slice(&value);
        native.push(0);
    }
    let records = authority
        .records()
        .map_err(|error| format!("record domain: {error}"))?;
    Ok(Keyed {
        records,
        package: format!("{:?}", facts.entry.0.package),
        source,
        native,
        held,
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
    // A compiler file that changed after it was hashed may not be the code
    // that ran: the record is withheld (design §15 R3).
    if let Some(changed) = keyed.held.iter().find(|held| !held.unchanged()) {
        bypassed(format!(
            "compiler file {} changed during the request",
            changed.path.display()
        ));
        return;
    }
    if let Ok(hirsel_artifact_store::programs::Published::Committed { .. }) =
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
                hit.generation
            }
            Lookup::Miss { generation } => generation,
        };
        timings::count(Count::ProgramRecordMisses);
        let checked = session.check_applied();
        if let Ok(output) = &checked
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
                    execution: None,
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
        // Stage validation must revalidate every build: it neither reads nor
        // publishes a record.
        let keyed = if build.validation == hirsel_native::Validation::EveryStage {
            Err("validate stages".to_owned())
        } else if std::env::var_os("HIRSEL_RUNTIME_ARCHIVES").is_none() {
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
                    keyed.native.as_slice(),
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
                        let host =
                            crate::requests::host_executable_platform(&store, &build.options)
                                .ok()?;
                        let platform = host.identity();
                        captured = Some(std::sync::Arc::new(host.into_handed()));
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
                None => hit.generation,
            },
            Lookup::Miss { generation } => generation,
        };
        timings::count(Count::ProgramRecordMisses);
        request.platform = captured;
        let selected = run(session, request);
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
                        execution: None,
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

/// One `hirsel run --native` request.
pub(crate) struct Run<'a, 'o> {
    pub launch: crate::HostLaunch<'a>,
    pub evidence: Option<&'a hirsel_native::Evidence>,
    pub backend: hirsel_native::Backend,
    pub validation: hirsel_native::Validation,
    pub output: crate::native::Output<'o>,
    pub trace_events: Option<&'a Path>,
}

type Ran = Result<Result<crate::RunOutcome, NativeError>, ToolError>;

/// `hirsel run --native` of `entry` through B. A run record is its own
/// operation: a build record never answers a run, nor the reverse. A source
/// failure is keyed by the source key alone; a success by the source key, the
/// native options, the evidence, the platform the lookup captures and the
/// launch inputs (the linker embeds the bound run's identity in the
/// executable). A hit binds the launch to the program facts R holds, writes
/// the recorded executable and launches it with no request. The program's
/// exit status is never part of a record.
pub(crate) fn run(
    session: &mut Session,
    entry: &Path,
    policy: Policy,
    mut run: Run<'_, '_>,
) -> Ran {
    session.invocation(|session| {
        let target = hirsel_native::TargetSpec::x86_64_unknown_linux_gnu();
        let options = hirsel_native::BuildProfile::run_options(run.backend, target, run.validation);
        let observed = {
            let _phase = timings::phase(Phase::ProgramKey);
            session.observe_inputs()
        };
        if observed.is_err() {
            bypassed("inputs not observed".into());
        }
        observed?;
        let launch_key = match run.launch {
            crate::HostLaunch::Live { config, .. } => config.key(),
            _ => None,
        };
        // A replay binds a launch the program record does not hold.
        let keyed = if !matches!(run.launch, crate::HostLaunch::Live { .. }) {
            Err("replay launch".to_owned())
        } else if run.validation == hirsel_native::Validation::EveryStage {
            Err("validate stages".to_owned())
        } else if std::env::var_os("HIRSEL_RUNTIME_ARCHIVES").is_none() {
            Err("runtime archives from the Cargo fallback".to_owned())
        } else if launch_key.is_none() {
            Err("launch inputs unreadable".to_owned())
        } else {
            keyed(session, entry, policy)
        };
        let keyed = match keyed {
            Ok(keyed) => keyed,
            Err(reason) => {
                bypassed(reason);
                return fresh(session, &mut run, options, None, None);
            }
        };
        let launch_key = launch_key.unwrap_or_default();
        let evidence = run
            .evidence
            .map(crate::requests::evidence_identity)
            .unwrap_or_default();
        let address = ContentHash::of_parts(
            "hirsel.program.address.v1",
            [
                b"run".as_slice(),
                keyed.package.as_bytes(),
                format!(
                    "{:?}/{:?}/{:?}",
                    options.passes.target, run.backend, run.validation
                )
                .as_bytes(),
                &evidence.0,
            ],
        );
        let rendered = format!("{options:?}").into_bytes();
        let success = |platform: ContentHash| {
            ContentHash::of_parts(
                "hirsel.program.run-key.v1",
                [
                    &keyed.source.0[..],
                    keyed.native.as_slice(),
                    rendered.as_slice(),
                    &evidence.0,
                    &platform.0,
                    launch_key.as_slice(),
                ],
            )
        };
        let store = session.compiler().configuration().store.clone();
        let mut captured = None;
        let looked = {
            let _phase = timings::phase(Phase::ProgramLookup);
            keyed
                .records
                .lookup(address, ProgramOperation::Run, |kind| match kind {
                    ProgramKind::SourceDiagnostics => Some(keyed.source),
                    ProgramKind::Success => {
                        let _phase = timings::phase(Phase::ProgramPlatform);
                        let host =
                            crate::requests::host_executable_platform(&store, &options).ok()?;
                        let platform = host.identity();
                        captured = Some(host);
                        Some(success(platform))
                    }
                })
        };
        let generation = match looked {
            Lookup::Hit(hit) => match replay_run(&hit, captured.as_ref(), &mut run) {
                Ok(ran) => {
                    timings::count(Count::ProgramRecordHits);
                    return ran;
                }
                Err(()) => hit.generation,
            },
            Lookup::Miss { generation } => generation,
        };
        timings::count(Count::ProgramRecordMisses);
        let handed = captured.map(|host| std::sync::Arc::new(host.into_handed()));
        let record = |kind: ProgramKind,
                      key: ContentHash,
                      image: Option<ContentHash>,
                      diagnostics: &[u8],
                      executable: Option<&[u8]>,
                      execution: Option<&[u8]>| {
            publish(
                &keyed,
                ProgramRecord {
                    operation: ProgramOperation::Run,
                    kind,
                    status: (kind == ProgramKind::SourceDiagnostics) as u8,
                    address,
                    key,
                    image,
                    diagnostics,
                    executable,
                    execution,
                },
                generation,
            )
        };
        fresh(
            session,
            &mut run,
            options,
            handed,
            Some(&|recorded: RunRecorded<'_>| match recorded {
                RunRecorded::Source {
                    diagnostics,
                    sources,
                } => {
                    if let Some(diagnostics) = encode(diagnostics, sources, &[]) {
                        record(
                            ProgramKind::SourceDiagnostics,
                            keyed.source,
                            None,
                            &diagnostics,
                            None,
                            None,
                        );
                    }
                }
                RunRecorded::Success {
                    input,
                    descriptor,
                    executable,
                } => {
                    let platform = descriptor.platform;
                    if let (Some(diagnostics), Some(execution)) = (
                        encode(&[], &SourceMap::default(), &input.report),
                        descriptor.encode(),
                    ) {
                        record(
                            ProgramKind::Success,
                            success(platform),
                            Some(image_binding(ContentHash::of_bytes(executable), platform)),
                            &diagnostics,
                            Some(executable),
                            Some(&execution),
                        );
                    }
                }
            }),
        )
    })
}

/// What a fresh run may record.
enum RunRecorded<'a> {
    /// The source request completed with diagnostics.
    Source {
        diagnostics: &'a [hirsel_diag::Diagnostic],
        sources: &'a SourceMap,
    },
    /// The executable is written and validated, and the launch has not begun.
    Success {
        input: &'a crate::requests::NativeExecution,
        descriptor: Descriptor<'a>,
        executable: &'a [u8],
    },
}

/// Run `run` with no record answering it: select the source facts, bind the
/// launch to them, compile, link and launch. `record` sees a source failure,
/// or a success once its image is written and validated; a native failure, a
/// launch refusal before the image and any other error are never recorded.
fn fresh(
    session: &mut Session,
    run: &mut Run<'_, '_>,
    mut options: BuildOptions,
    platform: Option<
        std::sync::Arc<hirsel_native::object_queries::platform::record::HandedPlatform>,
    >,
    record: Option<&dyn Fn(RunRecorded<'_>)>,
) -> Ran {
    let launch = run.launch;
    let selection = launch_binding::LaunchSelection::read(launch)?;
    let definition = selection.entry().cloned();
    // Source launch facts and native roots select exactly the same frozen
    // observations. Do not poll disk between binding and native selection.
    let selected_facts = match session.request_applied(crate::requests::CompileOperation::Tir {
        definition: definition.clone(),
    }) {
        Ok(selected) => selected,
        Err(ToolError::Query {
            error: QueryError::Diagnostics(diagnostics),
            phase,
            sources,
        }) => {
            if let Some(record) = record {
                record(RunRecorded::Source {
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
    let crate::requests::CompileResult::Tir(facts) = &selected_facts.result else {
        return Err(ToolError::Runtime(
            "native launch facts returned another operation".into(),
        ));
    };
    let unmodeled = facts
        .foreign
        .unmodeled
        .first_key_value()
        .map(|(user, extern_)| (user.to_string(), extern_.to_string()));
    if selection.requires_replayable(launch)
        && let Some((user, extern_)) = &unmodeled
    {
        return Err(ToolError::Runtime(format!(
            "launch: {user} calls the extern {extern_} and is not a wrapper with a \
             deterministic model, so this run cannot be recorded or replayed (PROG-4)"
        )));
    }
    let entry_facts = selection.facts(
        launch,
        &facts.runtime.types,
        &facts.signature,
        &facts.revision,
        facts.code.clone(),
    )?;
    let bound = selection.bind_facts(launch, &entry_facts)?;
    let revision = facts.revision.digest();
    // Binding owns the compact launch record. Release the completed TIR
    // closure before the native request admits its own source graph.
    drop(selected_facts);
    let bound_run = bound.run();
    options.passes.launch = bound_run.record().inputs.clone();
    let selected = session.request_applied(crate::requests::CompileOperation::NativeRun(
        crate::requests::NativeRequest {
            definition,
            options,
            run: Some(std::sync::Arc::new(bound_run.clone())),
            evidence: run.evidence.cloned().map(std::sync::Arc::new),
            test_entries: vec![],
            supplemental_sources: vec![],
            output: crate::requests::NativeOutput::Executable,
            platform,
        },
    ))?;
    match &selected.result {
        crate::requests::CompileResult::Native(input) => {
            // The launch was bound before compilation against these exact
            // observations; the compiler cannot substitute another revision.
            if input.revision.digest() != revision {
                return Err(ToolError::Runtime(
                    "native launch revision changed during selection".into(),
                ));
            }
            let published = |input: &crate::requests::NativeExecution| {
                let Some(record) = record else { return };
                let (Some(facts), Some(run)) = (entry_facts.program_facts(), &input.run) else {
                    return;
                };
                let Ok(linked) = hirsel_native::object_queries::linked_product(
                    input.store.as_ref(),
                    input.product.clone(),
                ) else {
                    return;
                };
                let Ok(executable) =
                    hirsel_native::object_queries::executable_bytes(input.store.as_ref(), &linked)
                else {
                    return;
                };
                record(RunRecorded::Success {
                    input,
                    descriptor: Descriptor {
                        platform: input.platform_identity(),
                        run: run.identity(),
                        emulator: input.emulator().map(Path::to_path_buf),
                        facts,
                        unmodeled: unmodeled.clone(),
                        observation: std::borrow::Cow::Borrowed(&**input.observation),
                        sources: std::borrow::Cow::Borrowed(&input.sources),
                    },
                    executable: &executable,
                });
            };
            let mut execution = crate::native::request_output::Execution::request(input);
            execution.validated = Some(&published);
            crate::native::request_output::run_bound_output(
                execution,
                launch,
                bound,
                run.output.reborrow(),
                run.trace_events,
            )
        }
        crate::requests::CompileResult::NativeError(error) => Ok(Err(error.clone())),
        _ => Err(ToolError::Runtime(
            "native run request returned another operation".into(),
        )),
    }
}

/// Answer a run from its record, or `Err` for a miss. A source failure
/// replays its diagnostics. A success answers only when R decodes, R's
/// platform is the one this lookup captured and its runtime still validates,
/// the record binds its image validation to that platform, the emulator is
/// unchanged, the launch binds to R's program facts with R's run identity,
/// and the executable and runtime are written with the record's checks.
/// Nothing is printed or launched before that.
fn replay_run(
    hit: &ProgramHit,
    host: Option<&crate::requests::HostPlatform>,
    run: &mut Run<'_, '_>,
) -> Result<Ran, ()> {
    let (diagnostics, sources, report) = {
        let _phase = timings::phase(Phase::ProgramReplay);
        decode(&hit.diagnostics).ok_or(())?
    };
    if hit.header.kind == ProgramKind::SourceDiagnostics {
        return Ok(Err(ToolError::Query {
            error: QueryError::Diagnostics(diagnostics),
            phase: None,
            sources,
        }));
    }
    let _phase = timings::phase(Phase::ProgramMaterialize);
    let descriptor = Descriptor::decode(hit.execution.as_deref().ok_or(())?).ok_or(())?;
    let host = host.ok_or(())?;
    let (digest, _) = hit.header.executable.ok_or(())?;
    if descriptor.platform != host.identity()
        || hit.header.image != Some(image_binding(digest, descriptor.platform))
    {
        return Err(());
    }
    let launcher = host.launcher().map_err(drop)?;
    if launcher.identity() != descriptor.platform
        || launcher.emulator() != descriptor.emulator.as_deref()
    {
        return Err(());
    }
    let selection = launch_binding::LaunchSelection::read(run.launch).map_err(drop)?;
    if selection.requires_replayable(run.launch) && descriptor.unmodeled.is_some() {
        // The fresh path refuses this launch before binding it.
        return Err(());
    }
    let bound = selection
        .bind_facts(
            run.launch,
            &launch_binding::EntryFacts::recorded(descriptor.facts),
        )
        .map_err(drop)?;
    if bound.run().identity() != descriptor.run {
        return Err(());
    }
    let execution = crate::native::request_output::Execution::recorded(
        hit,
        &launcher,
        &report,
        descriptor.run,
        descriptor.unmodeled,
        &descriptor.observation,
        &descriptor.sources,
    )
    .map_err(drop)?;
    drop(_phase);
    Ok(crate::native::request_output::run_bound_output(
        execution,
        run.launch,
        bound,
        run.output.reborrow(),
        run.trace_events,
    ))
}

/// R, the execution descriptor (`Section::R`, inner version 2): what a run
/// record needs to bind and launch its executable without the build's
/// products.
struct Descriptor<'a> {
    platform: ContentHash,
    /// The identity of the bound run the executable embeds.
    run: [u8; 32],
    emulator: Option<PathBuf>,
    facts: hirsel_run_config::ProgramFacts,
    unmodeled: Option<(String, String)>,
    observation: std::borrow::Cow<'a, hirsel_native::NativeObservation>,
    sources: std::borrow::Cow<'a, SourceMap>,
}

const EXECUTION_VERSION: u32 = 2;

/// R's launch binding facts.
#[derive(serde::Serialize, serde::Deserialize)]
struct Facts {
    program: [u8; 32],
    entry: String,
    launch: hirsel_run_schema::LaunchKind,
    /// `Root`s as Io 0, Args 1, Env 2, Unsafe 3, Test 4.
    roots: Vec<u8>,
    /// None is `CodeReach::Safe`.
    code: Option<Vec<String>>,
}

/// R's observation tables, with the source files their anchors cite.
#[derive(serde::Serialize, serde::Deserialize)]
struct Tables {
    /// Path, logical path, text and ordinal of each cited source.
    files: Vec<(PathBuf, PathBuf, String, u32)>,
    failure_rules: Vec<String>,
    /// Site code, definition and range (source, start, end).
    sites: Vec<(u64, hirsel_syntax::DefPath, u32, u32, u32)>,
    /// Each sited definition's anchors, as (file index, base).
    anchors: Vec<(hirsel_syntax::DefPath, Vec<(u32, u32)>)>,
}

impl Descriptor<'_> {
    fn encode(&self) -> Option<Vec<u8>> {
        use hirsel_run_config::Root;
        use std::os::unix::ffi::OsStrExt;
        let facts = Facts {
            program: self.facts.program,
            entry: self.facts.entry.clone(),
            launch: self.facts.launch,
            roots: self
                .facts
                .roots
                .iter()
                .map(|root| match root {
                    Root::Io => 0,
                    Root::Args => 1,
                    Root::Env => 2,
                    Root::Unsafe => 3,
                    Root::Test => 4,
                })
                .collect(),
            code: match &self.facts.code {
                hirsel_run_schema::CodeReach::Safe => None,
                hirsel_run_schema::CodeReach::Escapes(path) => Some(path.clone()),
            },
        };
        // Only a sited definition's anchors are read when a launch is observed.
        let sited: std::collections::BTreeSet<&hirsel_syntax::DefPath> = self
            .observation
            .sites
            .values()
            .map(|(id, _)| id.path())
            .collect();
        let mut files: Vec<(PathBuf, PathBuf, String, u32)> = vec![];
        let mut ids = vec![];
        let mut anchors = vec![];
        for (path, list) in &self.observation.anchors {
            if !sited.contains(path) {
                continue;
            }
            let mut placed = vec![];
            for &anchor in list {
                let (file, base) = self.sources.anchor(anchor)?;
                let index = match ids.iter().position(|id| *id == file) {
                    Some(index) => index,
                    None => {
                        let source = self.sources.file(file)?;
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
                placed.push((index as u32, base));
            }
            anchors.push((path.clone(), placed));
        }
        let tables = Tables {
            files,
            failure_rules: self.observation.failure_rules.clone(),
            sites: self
                .observation
                .sites
                .iter()
                .map(|(code, (id, range))| {
                    (
                        *code,
                        id.path().clone(),
                        range.source,
                        range.start,
                        range.end,
                    )
                })
                .collect(),
            anchors,
        };
        let mut bytes = EXECUTION_VERSION.to_le_bytes().to_vec();
        bytes.extend_from_slice(&self.platform.0);
        bytes.extend_from_slice(&self.run);
        let mut part = |part: &[u8]| {
            bytes.extend_from_slice(&(u32::try_from(part.len()).ok()?).to_le_bytes());
            bytes.extend_from_slice(part);
            Some(())
        };
        part(
            self.emulator
                .as_deref()
                .map_or(&[][..], |path| path.as_os_str().as_bytes()),
        )?;
        part(&serde_json::to_vec(&facts).ok()?)?;
        part(
            self.unmodeled
                .as_ref()
                .map_or(vec![], |(user, extern_)| {
                    format!("{user}\0{extern_}").into_bytes()
                })
                .as_slice(),
        )?;
        part(&serde_json::to_vec(&tables).ok()?)?;
        Some(bytes)
    }

    fn decode(bytes: &[u8]) -> Option<Descriptor<'static>> {
        use hirsel_run_config::Root;
        use std::os::unix::ffi::OsStrExt;
        let mut rest = bytes;
        let mut take = |n: usize| -> Option<&[u8]> {
            let (head, tail) = (rest.get(..n)?, rest.get(n..)?);
            rest = tail;
            Some(head)
        };
        if u32::from_le_bytes(take(4)?.try_into().ok()?) != EXECUTION_VERSION {
            return None;
        }
        let platform = ContentHash(take(32)?.try_into().ok()?);
        let run: [u8; 32] = take(32)?.try_into().ok()?;
        let mut part = || -> Option<&[u8]> {
            let length = u32::from_le_bytes(take(4)?.try_into().ok()?) as usize;
            take(length)
        };
        let emulator = part()?;
        let emulator =
            (!emulator.is_empty()).then(|| PathBuf::from(std::ffi::OsStr::from_bytes(emulator)));
        let facts: Facts = serde_json::from_slice(part()?).ok()?;
        let unmodeled = part()?;
        let unmodeled = if unmodeled.is_empty() {
            None
        } else {
            let (user, extern_) = std::str::from_utf8(unmodeled).ok()?.split_once('\0')?;
            Some((user.to_owned(), extern_.to_owned()))
        };
        let tables: Tables = serde_json::from_slice(part()?).ok()?;
        if !rest.is_empty() {
            return None;
        }
        let roots = facts
            .roots
            .iter()
            .map(|root| {
                Some(match root {
                    0 => Root::Io,
                    1 => Root::Args,
                    2 => Root::Env,
                    3 => Root::Unsafe,
                    4 => Root::Test,
                    _ => return None,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        let mut sources = SourceMap::default();
        let mut ids = vec![];
        for (path, logical, text, ordinal) in tables.files {
            ids.push(sources.add_replayed(path, logical, text, ordinal)?);
        }
        let mut anchors = std::collections::BTreeMap::new();
        for (path, placed) in tables.anchors {
            let mut list = vec![];
            for (index, base) in placed {
                list.push(sources.item_anchor(*ids.get(index as usize)?, base));
            }
            anchors.insert(path, list);
        }
        let observation = hirsel_native::NativeObservation {
            failure_rules: tables.failure_rules,
            sites: tables
                .sites
                .into_iter()
                .map(|(code, path, source, start, end)| {
                    (
                        code,
                        (
                            hirsel_syntax::DefId::intern(&path),
                            hirsel_lir::ir::SiteRange { source, start, end },
                        ),
                    )
                })
                .collect(),
            anchors,
        };
        Some(Descriptor {
            platform,
            run,
            emulator,
            facts: hirsel_run_config::ProgramFacts {
                program: facts.program,
                entry: facts.entry,
                launch: facts.launch,
                roots,
                code: facts.code.map_or(
                    hirsel_run_schema::CodeReach::Safe,
                    hirsel_run_schema::CodeReach::Escapes,
                ),
            },
            unmodeled,
            observation: std::borrow::Cow::Owned(observation),
            sources: std::borrow::Cow::Owned(sources),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{Descriptor, EXECUTION_VERSION, Loaded, compiler_file_digest};
    use hirsel_syntax::ContentHash;

    /// Bug caught: R decoded under another inner version (a record written by
    /// an older or newer layout answers this one), trailing bytes accepted,
    /// or a part that does not survive its own encoding: the emulator's raw
    /// path bytes, every root, the escape path or the unmodeled symbol.
    #[test]
    fn r_decodes_only_its_own_version_and_round_trips() {
        use std::os::unix::ffi::OsStrExt;
        let emulator = std::path::PathBuf::from(std::ffi::OsStr::from_bytes(b"/q\xffemu"));
        let descriptor = Descriptor {
            platform: hirsel_syntax::ContentHash([1; 32]),
            run: [2; 32],
            emulator: Some(emulator.clone()),
            facts: hirsel_run_config::ProgramFacts {
                program: [3; 32],
                entry: "main".into(),
                launch: hirsel_run_schema::LaunchKind::Test,
                roots: vec![
                    hirsel_run_config::Root::Test,
                    hirsel_run_config::Root::Io,
                    hirsel_run_config::Root::Unsafe,
                    hirsel_run_config::Root::Args,
                    hirsel_run_config::Root::Env,
                ],
                code: hirsel_run_schema::CodeReach::Escapes(vec!["a@1".into(), "b@1".into()]),
            },
            unmodeled: Some(("user".into(), "extern".into())),
            observation: std::borrow::Cow::Owned(Default::default()),
            sources: std::borrow::Cow::Owned(Default::default()),
        };
        let bytes = descriptor.encode().unwrap();
        let decoded = Descriptor::decode(&bytes).expect("its own encoding");
        assert_eq!(decoded.platform, descriptor.platform);
        assert_eq!(decoded.run, descriptor.run);
        assert_eq!(decoded.emulator, Some(emulator));
        assert_eq!(decoded.facts, descriptor.facts);
        assert_eq!(decoded.unmodeled, descriptor.unmodeled);
        assert_eq!(decoded.encode().unwrap(), bytes);
        for version in [EXECUTION_VERSION - 1, EXECUTION_VERSION + 1] {
            let mut other = bytes.clone();
            other[..4].copy_from_slice(&version.to_le_bytes());
            assert!(Descriptor::decode(&other).is_none(), "version {version}");
        }
        let mut longer = bytes.clone();
        longer.push(0);
        assert!(Descriptor::decode(&longer).is_none(), "trailing byte");
        assert!(
            Descriptor::decode(&bytes[..bytes.len() - 1]).is_none(),
            "truncated"
        );
    }
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::time::{Duration, SystemTime};

    /// As the loader saw `path` now.
    fn mapped(path: &std::path::Path) -> Loaded {
        let meta = std::fs::metadata(path).unwrap();
        Loaded {
            path: path.to_owned(),
            mapped: (meta.dev(), meta.ino()),
        }
    }

    /// Bug caught: a guard keyed by inode metadata instead of the mapped
    /// compiler's bytes. A ctime-only change must keep the key after one
    /// rehash (not miss forever on a shared inode); a same-size rewrite with
    /// its mtime restored must be a new key; a file replaced on disk after
    /// the process mapped it must not be keyed at all.
    #[test]
    fn a_compiler_file_is_keyed_by_the_bytes_of_the_inode_it_mapped() {
        let home = tempfile::tempdir().unwrap();
        let cache = home.path().join("cache");
        std::fs::create_dir(&cache).unwrap();
        let path = home.path().join("libLLVM.so");
        std::fs::write(&path, b"compiler bytes one").unwrap();
        let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        let loaded = mapped(&path);
        let authority = registered(home.path());
        // Stamps written now are stable only seconds later.
        let later = || SystemTime::now() + Duration::from_secs(10);
        let digest = |started| {
            let digested =
                compiler_file_digest(Some((&cache, &authority)), &loaded, started).unwrap();
            (digested.digest, digested.rehashed)
        };
        // File times are taken from a coarse clock: a change in the same tick
        // as the cached stamp would leave it equal. Only stamps two seconds
        // old are cached in use, so each change here first lets a tick pass.
        let tick = || std::thread::sleep(Duration::from_millis(20));

        let (first, rehashed) = digest(later());
        assert!(rehashed, "a first sight hashes");
        assert_eq!(digest(later()), (first, false), "an equal stamp reuses");
        // Bug: an issued pair reused without the registration's MAC (a
        // forged or pre-R3 entry naming other bytes under the same stamp).
        // Oracle: the bytes on disk.
        let entry = std::fs::read_dir(&cache)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let mut forged = std::fs::read(&entry).unwrap();
        forged[64..96].copy_from_slice(&ContentHash::of_bytes(b"other").0);
        std::fs::write(&entry, &forged).unwrap();
        assert_eq!(digest(later()), (first, true), "an unauthenticated entry");
        assert_eq!(digest(later()), (first, false));
        // Within the same tick a stamp never reuses: the write may be racing.
        assert_eq!(digest(SystemTime::now()), (first, true), "unstable stamp");

        // ctime moves (as when another hard link to the inode is made), bytes do not.
        tick();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            digest(later()),
            (first, true),
            "equal bytes after a stat change: the same compiler, one rehash"
        );
        assert_eq!(digest(later()), (first, false));

        // Equal size, other bytes, mtime restored: only ctime shows the rewrite.
        tick();
        std::fs::write(&path, b"compiler bytes two").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
        let (second, rehashed) = digest(later());
        assert!(rehashed);
        assert_ne!(second, first, "rewritten compiler bytes must miss");
        let uncached = compiler_file_digest(None, &loaded, later()).unwrap();
        assert_eq!((uncached.digest, uncached.rehashed), (second, true));

        // Bug: a record published after a compiler file changed under its
        // hashed descriptor. Oracle: the held stamp no longer matches.
        assert!(uncached.held.unchanged());
        tick();
        std::fs::write(&path, b"compiler bytes 3!!").unwrap();
        assert!(!uncached.held.unchanged(), "a change after hashing");
        std::fs::write(&path, b"compiler bytes two").unwrap();

        // Replaced after the mapping (an atomic upgrade renames a new inode in).
        let next = home.path().join("libLLVM.so.new");
        std::fs::write(&next, b"compiler bytes two").unwrap();
        std::fs::rename(&next, &path).unwrap();
        let replaced = compiler_file_digest(Some((&cache, &authority)), &loaded, later())
            .err()
            .unwrap();
        assert!(replaced.contains("replaced after load"), "{replaced}");
        // The new inode, once loaded, is keyed by its own bytes.
        let next =
            compiler_file_digest(Some((&cache, &authority)), &mapped(&path), later()).unwrap();
        assert_eq!((next.digest, next.rehashed), (second, true));
    }

    /// A registration whose authority seals this test's digest entries.
    fn registered(
        directory: &std::path::Path,
    ) -> hirsel_artifact_store::programs::ProgramAuthority {
        use hirsel_artifact_store::{Action, Address, Association, Domain, Store};
        let root = directory.join("registered");
        let state = directory.join("registration-state");
        std::fs::create_dir_all(&root).unwrap();
        let verifier = ContentHash::of_bytes(b"compiler digest test verifier");
        let store = Store::new(root.clone());
        let reference = store
            .put(
                Domain {
                    kind: 31,
                    schema: 1,
                },
                b"compiler digest test header",
            )
            .unwrap();
        let address = Address(ContentHash::of_bytes(b"ordinary"));
        let action = Action(ContentHash::default());
        let install =
            hirsel_artifact_store::registration::Installation::begin(&root, &state, verifier)
                .unwrap();
        store.associate(address, action, reference).unwrap();
        install
            .finish((address, Association { action, reference }))
            .unwrap();
        let registered =
            hirsel_artifact_store::registration::Installation::registered(&root, &state, verifier)
                .unwrap();
        hirsel_artifact_store::programs::ProgramAuthority::new(vec![registered.authority]).unwrap()
    }
}
