//! One command/option catalogue drives parsing and user help.
use std::path::Path;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OptionId {
    Json,
    Recipe,
    CodeStore,
    ReleaseManifest,
    Recording,
    RecordingRuntime,
    ProfRuntime,
    ProfRecordingRuntime,
    Record,
    Tape,
    Replay,
    ExpectVerdict,
    IncludeCredentials,
    Diagnose,
    SourceCore,
    SourceStd,
    Name,
    CoreProduct,
    Dir,
    Grant,
    Filter,
    TestJobs,
    Interface,
    ForeignHeader,
    Dependents,
    Transitive,
    AffectedTests,
    Unused,
    InspectLibrary,
    InspectStd,
    InspectCore,
    TestTimeout,
    Rerun,
    PropertyCorpus,
    Sim,
    SimSeeds,
    SimSeed,
    Release,
    DebugLines,
    Backend,
    Runtime,
    LogicalChargeReport,
    Cc,
    LinkKit,
    Target,
    Emulator,
    Sysroot,
    Limit,
    Output,
    Entry,
    Libraries,
    Store,
    Inputs,
    PackageStore,
    Locked,
    Frozen,
    Metrics,
    ProfileFront,
    ProfOut,
    ProfTasks,
    ProfMem,
    ProfFrequency,
    ProfPmu,
    ProfStackBytes,
    ProfDuration,
    ProfDebug,
    ProfBy,
    ProfEvent,
    ProfGroup,
    ProfFilter,
    ProfRows,
    ProfDepth,
    ProfMinDeltaPct,
    ProfTimeout,
    ProfFormat,
    Image,
    Lib,
    Help,
    Install,
    FmtCheck,
    FmtStdin,
    Signatures,
    ListRules,
    PlaceTimeout,
    NPlace,
    Url,
    CallTimeout,
    Control,
    HostState,
    CgroupRoot,
    Workdir,
    Listen,
    Scratch,
    GrantAdded,
    MaxPause,
    MaxRecipeBytes,
    MaxClosure,
    MaxDecodeBytes,
    StagingBytes,
    AdmitTimeout,
    HostMemory,
    StartRetries,
    TapeRetention,
}
struct OptionSpec {
    id: OptionId,
    names: &'static [&'static str],
    value: Option<&'static str>,
    commands: &'static [&'static str],
    description: &'static str,
}
#[derive(Debug)]
pub struct CommandSpec {
    pub name: &'static str,
    usage: &'static str,
    description: &'static str,
}
macro_rules! commands {
    ($($name:literal, $usage:literal, $description:literal;)*) => {
        pub static COMMANDS: &[CommandSpec] = &[$(CommandSpec {name:$name,usage:$usage,description:$description}),*];
    }
}
commands! {
    "help", "help [command]", "Show commands or help for one command.";
    "check", "check [package-dir] [options]", "Check a package or an explicit input manifest.";
    "lock", "lock [package-dir] [options]", "Resolve a workspace and write or verify its lock.";
    "fetch", "fetch [package-dir] [options]", "Fetch the pinned libraries a workspace uses.";
    "pack", "pack [package-dir] -o FILE [options]", "Publish a library and its dependency closure to an archive.";
    "build", "build [package-dir | --recipe HASH] [options]", "Assemble a native executable from a release recipe.";
    "publish", "publish PACKAGE [options]", "Check a package, publish its code objects and print the recipe hash.";
    "manifest", "manifest show PATH", "Print a release manifest.";
    "host", "host RECIPE --control SOCKET --state DIR --listen ADDR [options] [-- arguments...]", "Supervise a place and accept deploy requests.";
    "launcher", "launcher --control SOCKET --state DIR [--cgroup-root DIR]", "Hold the groups' listeners and start their children for the platform host.";
    "deploy", "deploy RECIPE --control SOCKET --code-store DIR [options]", "Submit a recipe and its missing code objects to a host.";
    "run", "run [package-dir] [options] [-- arguments...]", "Build and run a native executable.";
    "prof", "prof <command>", "Capture or inspect a CPU profile.";
    "prof run", "prof run [package-dir] [build-opts] [--tasks] [--out FILE.hprof] [--freq 997] [--pmu LIST] [--stack-bytes 32768] [-- args...]", "Build and sample a native executable from exec until exit.";
    "prof replay", "prof replay [package-dir | --inputs MANIFEST] --tape TAPE [--entry DEFINITION] [--tasks] [--mem[=exact|sampled]] [--out FILE.hprof]", "Profile serial replay through the recording and profiling runtime.";
    "prof attach", "prof attach <PID> [--duration SECS] [--freq 997] [--pmu LIST] [--debug PATH] [--out FILE.hprof]", "Snapshot mappings, then sample a running Linux process without stopping it.";
    "prof tail", "prof tail <FILE.hprof> [--limit 20] [--json]", "Report completed and censored task latencies and the observed causal edges.";
    "prof summary", "prof summary <FILE.hprof> [--json]", "Report capture quality and the five hottest functions per event.";
    "prof schema", "prof schema <FILE.hprof> [--json]", "List the profile's tables and columns.";
    "prof top", "prof top <FILE.hprof> [--by self|inclusive] [--event NAME] [--group def_path|instance] [--filter GLOB] [--limit 20] [--json]", "Rank functions by attributed event periods.";
    "prof annotate", "prof annotate <FILE.hprof> <DEF_PATH|FILE:LINE> [--json]", "Show attributed periods and compiler remarks by source line.";
    "prof peak-mem", "prof peak-mem <FILE.hprof> [--limit 20] [--json]", "Report memory peaks, allocation attribution and budget refusals.";
    "prof callers", "prof callers <FILE.hprof> <DEF_OR_SYM> [--depth 5] [--json]", "Show sampled callers of a function.";
    "prof callees", "prof callees <FILE.hprof> <DEF_OR_SYM> [--depth 5] [--json]", "Show sampled callees of a function.";
    "prof diff", "prof diff <BASE.hprof> <HEAD.hprof> [--by self|inclusive] [--group def_path|instance] [--min-delta-pct 1.0] [--json]", "Compare compatible captures and report changed and unmatched functions.";
    "prof sql", "prof sql <FILE.hprof> \"<SQL>\" [--limit 500] [--timeout-ms 5000] [--json]", "Query the profile with one read-only SQL statement.";
    "prof export", "prof export <FILE.hprof> --format pprof --out <PATH>", "Export a pprof profile.";
    "replay", "replay PACKAGE --tape TAPE [--diagnose] [options]", "Replay a recorded program with no host grants.";
    "tape", "tape show TAPE --include-credentials", "Export the header and frames stored in a credential-bearing tape.";
    "test", "test [package-dir] [options]", "Run each discovered @test in an independent process. When hirsel.toml contains [tests], set limit, dirs and grants there.";
    "inspect", "inspect [package-dir | ARCHIVE.hpkg | DEPENDENCY | --core | --std] [--interface [--json] | --foreign-header MODULE | --dependents DEF [--transitive] | --affected-tests DEF | --unused [--library]]", "Inspect a package's declarations and dependencies. The default is --interface.";
    "image", "image [library-root] [options]", "Publish checked core and std source libraries.";
    "core-product", "core-product [options]", "Produce or inspect the published core package.";
    "std-product", "std-product [options]", "Produce or inspect the published core and std packages.";
    "new", "new <package-dir> [--lib]", "Create a runnable application or library with an example main.";
    "env", "env [--json] [options]", "Report the compiler and selected installation, products and runtime.";
    "fmt", "fmt [--check] [--signatures] <FILE|DIR|--stdin>", "Format a source file, a source directory, or standard input.";
    "explain", "explain RULE|MODULE|debug-lines|primer | explain --list [--json]", "Explain specification rules and find library declarations.";
    "primer", "primer", "Print the Hirsel primer: the language in one page.";
    "sdk", "sdk ts <program> -o DIR [options]", "Generate browser and Node ES modules and TypeScript declarations.";
    "call", "call SERVER --url ws://HOST:PORT/PATH [options] [-- ENTRY [--FLAG VALUE...]]", "List a running place's entries, or call one, typed from the server's Describe.";
}
macro_rules! option {
    ($id:ident, [$($name:literal),+], $value:expr, [$($command:literal),*], $description:literal) => {
        OptionSpec {id:OptionId::$id,names:&[$($name),+],value:$value,commands:&[$($command),*],description:$description}
    }
}
static OPTIONS: &[OptionSpec] = &[
    option!(
        PackageStore,
        ["--package-store"],
        Some("DIR"),
        [
            "check", "build", "publish", "run", "test", "inspect", "replay", "sdk", "lock",
            "fetch", "pack"
        ],
        "Select the verified library store."
    ),
    option!(
        Locked,
        ["--locked"],
        None,
        [
            "check", "build", "publish", "run", "test", "inspect", "replay", "sdk", "lock",
            "fetch", "pack"
        ],
        "Require a current workspace lock."
    ),
    option!(
        Frozen,
        ["--frozen"],
        None,
        [
            "check", "build", "publish", "run", "test", "inspect", "replay", "sdk", "lock",
            "fetch", "pack"
        ],
        "Require a current workspace lock and use only the package store."
    ),
    option!(
        Interface,
        ["--interface"],
        None,
        ["inspect"],
        "Print public declarations."
    ),
    option!(
        ForeignHeader,
        ["--foreign-header"],
        Some("MODULE"),
        ["inspect"],
        "Print the C header of a foreign module by its full definition path."
    ),
    option!(
        Dependents,
        ["--dependents"],
        Some("DEF"),
        ["inspect"],
        "List definitions that use DEF. Use a full or package-relative definition path."
    ),
    option!(
        Transitive,
        ["--transitive"],
        None,
        ["inspect"],
        "Follow indirect users with --dependents."
    ),
    option!(
        AffectedTests,
        ["--affected-tests"],
        Some("DEF"),
        ["inspect"],
        "List cache-eligible tests a semantic edit to DEF may rerun."
    ),
    option!(
        Unused,
        ["--unused"],
        None,
        ["inspect"],
        "List unreachable definitions. An app starts at main and its tests. A library starts at its public items."
    ),
    option!(
        InspectLibrary,
        ["--library"],
        None,
        ["inspect"],
        "Use library roots for --unused even if the package has a main function."
    ),
    option!(
        InspectStd,
        ["--std"],
        None,
        ["inspect"],
        "Inspect the source standard library from --libraries or the checkout."
    ),
    option!(
        InspectCore,
        ["--core"],
        None,
        ["inspect"],
        "Inspect core declarations from --libraries or the bundled sources."
    ),
    option!(
        Recipe,
        ["--recipe"],
        Some("HASH"),
        ["build"],
        "Assemble an existing recipe in --code-store."
    ),
    option!(
        CodeStore,
        ["--code-store"],
        Some("DIR"),
        ["build", "publish", "host", "deploy"],
        "Select the immutable code store."
    ),
    option!(
        Control,
        ["--control"],
        Some("SOCKET"),
        ["host", "deploy", "launcher"],
        "Host control socket."
    ),
    option!(
        HostState,
        ["--state"],
        Some("DIR"),
        ["host", "replay", "launcher"],
        "Private host state directory."
    ),
    option!(
        CgroupRoot,
        ["--cgroup-root"],
        Some("DIR"),
        ["launcher"],
        "Use a delegated cgroup v2 subtree for child lifecycle and contained groups."
    ),
    option!(
        TapeRetention,
        ["--tape-retention"],
        Some("DAYS"),
        ["host"],
        "Retain a recording's pinned image for this many days. Default 30."
    ),
    option!(
        Workdir,
        ["--workdir"],
        Some("DIR"),
        ["host"],
        "Working directory for each place launch."
    ),
    option!(
        Listen,
        ["--listen"],
        Some("ADDR"),
        ["host"],
        "TCP address for the inherited listener."
    ),
    option!(
        Scratch,
        ["--scratch"],
        Some("DIR"),
        ["host"],
        "Private scratch directory for exports and prepare."
    ),
    option!(
        GrantAdded,
        ["--grant-added"],
        None,
        ["deploy"],
        "Allow the proposed recipe to widen grants."
    ),
    option!(
        MaxPause,
        ["--max-pause"],
        Some("MS"),
        ["host"],
        "Bound the pre-install write pause. Default 2000 ms."
    ),
    option!(
        MaxRecipeBytes,
        ["--max-recipe-bytes"],
        Some("BYTES"),
        ["host"],
        "Maximum recipe size."
    ),
    option!(
        MaxClosure,
        ["--max-closure"],
        Some("COUNT"),
        ["host"],
        "Maximum number of code objects per deploy."
    ),
    option!(
        MaxDecodeBytes,
        ["--max-decode-bytes"],
        Some("BYTES"),
        ["host"],
        "Cumulative allocation budget for decoding one recipe."
    ),
    option!(
        StagingBytes,
        ["--staging-bytes"],
        Some("BYTES"),
        ["host"],
        "Total space available for incoming objects."
    ),
    option!(
        AdmitTimeout,
        ["--admit-timeout"],
        Some("SECONDS"),
        ["host", "deploy"],
        "Deadline for the recipe and Need exchange."
    ),
    option!(
        HostMemory,
        ["--memory"],
        Some("BYTES"),
        ["host"],
        "Combined limit for serving and candidate launches."
    ),
    option!(
        StartRetries,
        ["--start-retries"],
        Some("COUNT"),
        ["host"],
        "Retries before reporting a failed start."
    ),
    option!(
        ReleaseManifest,
        ["--manifest"],
        Some("PATH"),
        ["build"],
        "Write a release manifest from an empty-list preview."
    ),
    option!(
        PlaceTimeout,
        ["--place-timeout"],
        Some("SECONDS"),
        ["sdk", "call", "build"],
        "Describe and preview wall-clock deadline; default 10 seconds."
    ),
    option!(
        NPlace,
        ["--n-place"],
        Some("BYTES"),
        ["sdk", "call", "build", "publish"],
        "Place registration allowance; default 16777216 bytes."
    ),
    option!(
        Url,
        ["--url"],
        Some("URL"),
        ["call"],
        "The running server's ws:// address."
    ),
    option!(
        CallTimeout,
        ["--timeout"],
        Some("SECONDS"),
        ["call"],
        "Connection and call deadline; default 10 seconds."
    ),
    option!(
        Recording,
        ["--recording"],
        None,
        ["build"],
        "Include recording support. Set HIRSEL_RECORD when running the executable to record."
    ),
    option!(
        RecordingRuntime,
        ["--recording-runtime"],
        Some("ARCHIVE"),
        ["core-product", "std-product"],
        "Install this recording runtime archive alongside --runtime."
    ),
    option!(
        ProfRuntime,
        ["--prof-runtime"],
        Some("ARCHIVE"),
        ["core-product", "std-product"],
        "Install the profiling runtime archive alongside --runtime."
    ),
    option!(
        ProfRecordingRuntime,
        ["--prof-recording-runtime"],
        Some("ARCHIVE"),
        ["core-product", "std-product"],
        "Install the combined profiling and recording runtime archive."
    ),
    option!(
        Record,
        ["--record"],
        Some("TAPE"),
        ["run", "host"],
        "Build with recording enabled. Run writes TAPE. Host writes owner-only recordings in TAPE as a directory."
    ),
    option!(
        Tape,
        ["--tape"],
        Some("TAPE"),
        ["replay"],
        "Replay this tape without host grants."
    ),
    option!(
        Replay,
        ["--replay"],
        None,
        ["test"],
        "Run each selected test in the recording build with recording on, then replay it and require Sealed."
    ),
    option!(
        ExpectVerdict,
        ["--expect-verdict"],
        Some("VERDICT"),
        ["test"],
        "With --replay, require each selected test's replay to end with VERDICT."
    ),
    option!(
        IncludeCredentials,
        ["--include-credentials"],
        None,
        ["tape"],
        "Allow tape output that may contain credentials."
    ),
    option!(
        Diagnose,
        ["--diagnose"],
        None,
        ["replay"],
        "Allow a changed revision and report its first divergence; never resume live."
    ),
    option!(
        FmtCheck,
        ["--check"],
        None,
        ["fmt"],
        "Exit 1 on differences without writing. For files, list paths that differ."
    ),
    option!(
        FmtStdin,
        ["--stdin"],
        None,
        ["fmt"],
        "Read source from standard input and write the formatted bytes to standard output."
    ),
    option!(
        Signatures,
        ["--signatures"],
        None,
        ["fmt"],
        "Allow core declaration headers without bodies."
    ),
    option!(
        ListRules,
        ["--list"],
        None,
        ["explain"],
        "List every rule in the current specification."
    ),
    option!(
        Install,
        ["--install"],
        Some("ROOT"),
        ["core-product", "std-product"],
        "Register new products and an optional runtime in an installation root."
    ),
    option!(
        LinkKit,
        ["--link-kit"],
        Some("DIRECTORY"),
        ["core-product", "std-product"],
        "Install the pinned link kit alongside the host runtime."
    ),
    option!(Help, ["--help", "-h"], None, [], "Show this help."),
    option!(
        ProfTasks,
        ["--tasks"],
        None,
        ["prof run", "prof replay"],
        "Capture runtime task, wake, host-stack and I/O observations."
    ),
    option!(
        ProfMem,
        ["--mem"],
        Some("[exact|sampled]"),
        ["prof replay"],
        "Capture memory and budgets. A bare --mem selects sampled mode."
    ),
    option!(
        ProfOut,
        ["--out"],
        Some("PATH"),
        ["prof run", "prof replay", "prof attach", "prof export"],
        "Write the profile to this path."
    ),
    option!(
        ProfFrequency,
        ["--freq"],
        Some("HZ"),
        ["prof run", "prof attach"],
        "Sample at this frequency per event (default 997)."
    ),
    option!(
        ProfPmu,
        ["--pmu"],
        Some("LIST"),
        ["prof run", "prof attach"],
        "Sample a comma-separated list of events (default cycles)."
    ),
    option!(
        ProfStackBytes,
        ["--stack-bytes"],
        Some("BYTES"),
        ["prof run"],
        "Capture this many bytes of user stack (default 32768)."
    ),
    option!(
        ProfDuration,
        ["--duration"],
        Some("SECS"),
        ["prof attach"],
        "Stop sampling after this many seconds."
    ),
    option!(
        ProfDebug,
        ["--debug"],
        Some("PATH"),
        ["prof attach"],
        "Use detached DWARF verified against the sampled image."
    ),
    option!(
        ProfBy,
        ["--by"],
        Some("self|inclusive"),
        ["prof top", "prof diff"],
        "Rank by leaf periods or periods anywhere in the stack."
    ),
    option!(
        ProfEvent,
        ["--event"],
        Some("NAME"),
        ["prof top"],
        "Rank samples of this event (default primary event)."
    ),
    option!(
        ProfGroup,
        ["--group"],
        Some("def_path|instance"),
        ["prof top", "prof diff"],
        "Combine instances of one definition or show them separately."
    ),
    option!(
        ProfFilter,
        ["--filter"],
        Some("GLOB"),
        ["prof top"],
        "Keep matching definitions or symbols."
    ),
    option!(
        ProfRows,
        ["--limit"],
        Some("ROWS"),
        [
            "prof summary",
            "prof schema",
            "prof top",
            "prof annotate",
            "prof peak-mem",
            "prof callers",
            "prof callees",
            "prof diff",
            "prof sql",
            "prof tail"
        ],
        "Return at most this many rows (default 20, or 500 for SQL)."
    ),
    option!(
        ProfDepth,
        ["--depth"],
        Some("FRAMES"),
        ["prof callers", "prof callees"],
        "Include at most this many frames above or below the function (default 5)."
    ),
    option!(
        ProfMinDeltaPct,
        ["--min-delta-pct"],
        Some("PERCENT"),
        ["prof diff"],
        "Omit matched functions below this absolute change (default 1.0)."
    ),
    option!(
        ProfTimeout,
        ["--timeout-ms"],
        Some("MS"),
        ["prof sql"],
        "Interrupt the statement after this many milliseconds (default 5000)."
    ),
    option!(
        ProfFormat,
        ["--format"],
        Some("pprof"),
        ["prof export"],
        "Write standard pprof protobuf."
    ),
    option!(
        Json,
        ["--json"],
        None,
        [
            "check",
            "image",
            "build",
            "run",
            "replay",
            "test",
            "inspect",
            "core-product",
            "std-product",
            "env",
            "explain"
        ],
        "Print structured JSON."
    ),
    option!(
        SourceCore,
        ["--source-core"],
        None,
        [
            "check", "build", "publish", "run", "replay", "test", "inspect", "sdk"
        ],
        "Check supplied core and std sources."
    ),
    option!(
        SourceStd,
        ["--source-std"],
        None,
        ["check", "build", "publish", "run", "replay", "test", "sdk"],
        "Check std from source instead of its product."
    ),
    option!(
        Name,
        ["--name"],
        Some("DEFINITION"),
        ["core-product", "std-product"],
        "Demand one published declaration."
    ),
    option!(
        CoreProduct,
        ["--core-product"],
        Some("CACHE"),
        ["check", "build", "run", "replay", "test", "env", "sdk"],
        "Select the published package cache; enables products for check."
    ),
    option!(
        Dir,
        ["--dir"],
        Some("PATH"),
        ["build", "publish", "run", "replay", "test"],
        "Grant a confined directory. Repeat for more directories."
    ),
    option!(
        Grant,
        ["--grant"],
        Some("KIND"),
        ["build", "publish", "run", "replay", "test", "deploy"],
        "Grant a capability kind. Net takes explicit Connect, Listen, Tls or Resolve rules separated by semicolons. Env requires Env=<pattern>[,<pattern>]. Repeat for more kinds."
    ),
    option!(
        Filter,
        ["--filter"],
        Some("TEXT"),
        ["test"],
        "Select test definition paths containing TEXT."
    ),
    option!(
        TestJobs,
        ["-j"],
        Some("N"),
        ["test"],
        "Run up to N isolated test children at once. Defaults to the number of CPUs in the process affinity mask."
    ),
    option!(
        TestTimeout,
        ["--test-timeout"],
        Some("SECONDS"),
        ["test"],
        "Bound each test's execution, excluding build time."
    ),
    option!(
        Rerun,
        ["--rerun"],
        None,
        ["test"],
        "Execute every selected test, reusing no recorded pass."
    ),
    option!(
        PropertyCorpus,
        ["--property-corpus"],
        Some("DIR"),
        ["test"],
        "Replay and save minimized failures from opt-in properties in DIR."
    ),
    option!(
        Sim,
        ["--sim"],
        None,
        ["test"],
        "Run fresh simulations and grant their seed through Env as HIRSEL_TEST_SIM_SEED."
    ),
    option!(
        SimSeeds,
        ["--seeds"],
        Some("N"),
        ["test"],
        "With --sim, run once per seed starting at zero."
    ),
    option!(
        SimSeed,
        ["--seed"],
        Some("S"),
        ["test"],
        "Run exactly simulation seed S."
    ),
    option!(
        Release,
        ["--release"],
        None,
        ["build", "publish", "run", "replay", "test", "sdk"],
        "Optimize native code; defaults to LLVM."
    ),
    option!(
        DebugLines,
        ["--debug-lines"],
        Some("on|off"),
        ["build", "run", "replay", "test", "sdk"],
        "Embed DWARF in the executable (default off). Builds also write detached DWARF. Failure positions stay enabled."
    ),
    option!(
        Backend,
        ["--backend"],
        Some("cranelift|llvm"),
        ["build", "publish", "run", "replay", "test", "sdk"],
        "Select the native engine (dev defaults to Cranelift). Both include unwind tables with fiber boundaries."
    ),
    option!(
        Runtime,
        ["--runtime"],
        Some("ARCHIVE"),
        [
            "build",
            "run",
            "replay",
            "test",
            "env",
            "core-product",
            "std-product",
            "sdk",
            "call"
        ],
        "Select the core runtime archive for the requested recording mode."
    ),
    option!(
        LogicalChargeReport,
        ["--logical-charge-report"],
        Some("FILE"),
        ["build", "run", "replay", "test"],
        "Instrument root logical reservation; unsuitable for timings."
    ),
    option!(
        Cc,
        ["--cc"],
        Some("PROGRAM"),
        ["build", "run", "replay", "test", "sdk", "call"],
        "Select the C linker driver for this request."
    ),
    option!(
        Target,
        ["--target"],
        Some("TRIPLE"),
        [
            "build",
            "publish",
            "run",
            "replay",
            "test",
            "env",
            "core-product",
            "std-product",
            "sdk"
        ],
        "Select the full native target triple."
    ),
    option!(
        Emulator,
        ["--emulator"],
        Some("FILE"),
        ["run", "replay", "test"],
        "Run through this explicit QEMU user executable."
    ),
    option!(
        Sysroot,
        ["--sysroot"],
        Some("DIR"),
        ["run", "replay", "test"],
        "Use this QEMU runtime sysroot with --emulator."
    ),
    option!(
        Limit,
        ["--limit"],
        Some("BYTES"),
        ["build", "publish", "run", "replay", "test", "sdk"],
        "Set the root limit; test requires it when the manifest does not declare one."
    ),
    option!(
        Output,
        ["-o", "--output"],
        Some("PATH"),
        ["build", "run", "replay", "test", "pack", "sdk"],
        "Write the command output to this path."
    ),
    option!(
        Entry,
        ["--entry"],
        Some("DEFINITION"),
        ["check", "build", "publish", "run", "replay", "sdk"],
        "Select an entry by its full definition path."
    ),
    option!(
        Libraries,
        ["--libraries"],
        Some("ROOT"),
        [
            "check", "image", "build", "publish", "run", "replay", "test", "inspect", "sdk"
        ],
        "Select a root containing core/ and std/."
    ),
    option!(
        Store,
        ["--store"],
        Some("DIRECTORY"),
        [
            "check",
            "image",
            "build",
            "publish",
            "run",
            "replay",
            "test",
            "inspect",
            "core-product",
            "std-product",
            "sdk"
        ],
        "Select the request or product store."
    ),
    option!(
        Inputs,
        ["--inputs"],
        Some("MANIFEST"),
        [
            "check", "image", "build", "publish", "run", "replay", "test", "sdk"
        ],
        "Read an explicit CorpusManifest JSON file."
    ),
    option!(
        Metrics,
        ["--metrics"],
        Some("FILE"),
        [
            "check",
            "image",
            "build",
            "publish",
            "run",
            "replay",
            "test",
            "core-product",
            "std-product"
        ],
        "Write exclusive stage instruction measurements."
    ),
    option!(
        ProfileFront,
        ["--profile-front"],
        None,
        ["check", "build", "publish", "run"],
        "Include nested published-image work in --metrics (adds profiling overhead)."
    ),
    option!(
        Image,
        ["--image"],
        Some("DIRECTORY"),
        [
            "check",
            "build",
            "publish",
            "run",
            "replay",
            "test",
            "core-product",
            "std-product"
        ],
        "Load a published library image."
    ),
    option!(
        Lib,
        ["--lib"],
        None,
        ["new"],
        "Create lib.hirsel and a runnable main that exercises it."
    ),
];
#[derive(Debug)]
pub struct Invocation {
    pub command: &'static CommandSpec,
    pub options: Vec<(OptionId, Option<String>)>,
    pub positional: Option<String>,
    pub arguments: Vec<String>,
}
pub enum Action {
    Help(String),
    Version,
    Invoke(Invocation),
}
pub(crate) fn named(name: &str) -> Result<&'static CommandSpec, String> {
    COMMANDS
        .iter()
        .find(|c| c.name == name)
        .ok_or_else(|| format!("unknown command `{name}`; use help"))
}
fn applies(option: &OptionSpec, command: &CommandSpec) -> bool {
    option.commands.is_empty()
        || option.commands.contains(&command.name)
        || (option.id == OptionId::Json
            && command.name.starts_with("prof ")
            && !matches!(
                command.name,
                "prof run" | "prof replay" | "prof attach" | "prof export"
            ))
        || (command.name == "prof replay"
            && option.commands.contains(&"replay")
            && !matches!(
                option.id,
                OptionId::HostState
                    | OptionId::Diagnose
                    | OptionId::Emulator
                    | OptionId::Sysroot
                    | OptionId::Json
            ))
        || (command.name == "prof run"
            && option.id != OptionId::Output
            && option.commands.contains(&"run"))
}
pub fn help(command: Option<&CommandSpec>) -> String {
    if let Some(command) = command {
        let mut result = format!(
            "Usage: hirsel {}\n\n{}\n\nOptions:\n",
            command.usage, command.description
        );
        if command.name == "prof" {
            for subcommand in COMMANDS.iter().filter(|c| c.name.starts_with("prof ")) {
                result.push_str(&format!(
                    "  {}\n    {}\n",
                    subcommand.usage, subcommand.description
                ));
            }
            return result;
        }
        for option in OPTIONS.iter().filter(|o| applies(o, command)) {
            let label = format!(
                "{}{}",
                option.names.join(", "),
                option.value.map_or(String::new(), |v| format!(" {v}"))
            );
            result.push_str(&format!("  {label:<34} {}\n", option.description));
        }
        result
    } else {
        let mut result =
            "Usage: hirsel <command> [options]\n       hirsel --version\n\nCommands:\n".to_owned();
        for command in COMMANDS.iter().filter(|c| !c.name.starts_with("prof ")) {
            result.push_str(&format!("  {:<16} {}\n", command.name, command.description));
        }
        result.push_str("\nUse help <command>, <command> --help, or <command> -h for options.\n");
        result
    }
}
fn is_help(name: &str) -> bool {
    OPTIONS
        .iter()
        .any(|option| option.id == OptionId::Help && option.names.contains(&name))
}
pub fn parse(arguments: impl IntoIterator<Item = String>) -> Result<Action, String> {
    let mut arguments = arguments.into_iter().peekable();
    let Some(mut name) = arguments.next() else {
        return Ok(Action::Help(help(None)));
    };
    if is_help(&name) {
        return Ok(Action::Help(help(None)));
    }
    if name == "--version" {
        return if arguments.next().is_none() {
            Ok(Action::Version)
        } else {
            Err("--version takes no arguments".into())
        };
    }
    if name == "prof" {
        match arguments.next() {
            None => return Ok(Action::Help(help(Some(named("prof")?)))),
            Some(subcommand) if is_help(&subcommand) => {
                return Ok(Action::Help(help(Some(named("prof")?))));
            }
            Some(subcommand) => name.push_str(&format!(" {subcommand}")),
        }
    }
    let command = named(&name)?;
    if name == "sdk" {
        match arguments.next().as_deref() {
            Some("ts") => {}
            Some("--help" | "-h") | None => return Ok(Action::Help(help(Some(command)))),
            _ => return Err("sdk requires the ts generator".into()),
        }
    }
    if name == "help" {
        let target = arguments
            .next()
            .map(|mut name| {
                if is_help(&name) {
                    Ok(command)
                } else {
                    if name == "prof"
                        && let Some(subcommand) = arguments.next()
                    {
                        name.push_str(&format!(" {subcommand}"));
                    }
                    named(&name)
                }
            })
            .transpose()?;
        if arguments.next().is_some() {
            return Err("help takes at most one command".into());
        }
        return Ok(Action::Help(help(target)));
    }
    if matches!(name.as_str(), "tape" | "manifest") {
        match arguments.next().as_deref() {
            Some("show") => {}
            Some("--help" | "-h") => return Ok(Action::Help(help(Some(command)))),
            _ => return Err(format!("expected: hirsel {}", command.usage)),
        }
    }
    let mut invocation = Invocation {
        command,
        options: Vec::new(),
        positional: None,
        arguments: Vec::new(),
    };
    while let Some(argument) = arguments.next() {
        if matches!(name.as_str(), "prof run" | "prof attach")
            && (argument == "--mem" || argument.starts_with("--mem="))
        {
            return Err(
                "record a tape and use 'hirsel prof replay --mem' to profile memory".into(),
            );
        }
        if name == "prof replay" && (argument == "--mem" || argument.starts_with("--mem=")) {
            let value = if let Some((_, value)) = argument.split_once('=') {
                value.to_owned()
            } else if arguments
                .peek()
                .is_some_and(|v| matches!(v.as_str(), "exact" | "sampled"))
            {
                arguments.next().unwrap()
            } else {
                "sampled".to_owned()
            };
            if !matches!(value.as_str(), "exact" | "sampled") {
                return Err("--mem requires exact or sampled".into());
            }
            invocation.options.push((OptionId::ProfMem, Some(value)));
            continue;
        }
        if argument == "--" && matches!(name.as_str(), "run" | "prof run" | "call" | "host") {
            invocation.arguments.extend(arguments);
            break;
        }
        if argument.starts_with('-')
            && !(name == "prof sql"
                && argument.starts_with("--")
                && argument.chars().any(char::is_whitespace)
                && invocation.positional.is_some()
                && invocation.arguments.is_empty())
        {
            let option = OPTIONS
                .iter()
                .find(|o| o.names.contains(&argument.as_str()) && applies(o, command))
                .ok_or_else(|| {
                    format!("unknown option `{argument}` for {name}; use help {name}")
                })?;
            if option.id == OptionId::Help {
                return Ok(Action::Help(help(Some(command))));
            }
            let value = option
                .value
                .map(|value| {
                    arguments
                        .next()
                        .ok_or_else(|| format!("{argument} requires {value}"))
                })
                .transpose()?;
            invocation.options.push((option.id, value));
        } else if invocation.positional.is_none()
            && !matches!(name.as_str(), "core-product" | "std-product" | "env")
        {
            invocation.positional = Some(argument);
        } else if invocation.arguments.is_empty()
            && matches!(
                name.as_str(),
                "prof annotate" | "prof callers" | "prof callees" | "prof diff" | "prof sql"
            )
        {
            invocation.arguments.push(argument);
        } else {
            return Err(format!("unexpected argument `{argument}` for {name}"));
        }
    }
    Ok(Action::Invoke(invocation))
}
/// Refuse to replace an existing directory, even when it contains no sources.
pub fn scaffold(directory: &Path, library: bool) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(parent) = directory.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::create_dir(directory)?;
    let main = if library {
        "pub fn main() -> Result[(), Exit] {\n    if lib.answer() == 42 {\n        Ok(())\n    } else {\n        Error(Exit(status: 1))\n    }\n}\n"
    } else {
        "pub fn main() -> Result[(), Exit] {\n    Ok(())\n}\n\npub fn twice(n: u64) -> u64 {\n    n + n\n}\n"
    };
    let lib = "pub fn answer() -> u64 {\n    42\n}\n";
    let test = if library {
        "@test\nfn test_answer() -> Result[(), ()] {\n    if lib.answer() == 42 {\n        Ok(())\n    } else {\n        Error(())\n    }\n}\n"
    } else {
        "@test\nfn test_twice() -> Result[(), ()] {\n    if main.twice(21) == 42 {\n        Ok(())\n    } else {\n        Error(())\n    }\n}\n"
    };
    let sources = ["main.hirsel"]
        .into_iter()
        .chain(library.then_some("lib.hirsel"));
    let manifest = crate::package_dir::scaffold_manifest(sources);
    for (name, source) in [
        ("main.hirsel", main),
        ("tests.hirsel", test),
        (crate::package_dir::MANIFEST, &manifest),
    ]
    .into_iter()
    .chain(library.then_some(("lib.hirsel", lib)))
    {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(directory.join(name))?
            .write_all(source.as_bytes())?;
    }
    Ok(())
}
