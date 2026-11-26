use super::{OptionId, OptionSpec};

pub(super) static OPTIONS: &[OptionSpec] = &[
    option!(
        Interface,
        ["--interface"],
        None,
        ["inspect"],
        "Print public declarations."
    ),
    option!(
        InspectModule,
        ["--module"],
        Some("MODULE"),
        ["inspect"],
        "Restrict --interface to declarations in MODULE (for example json or sim/fs)."
    ),
    option!(
        InspectFilter,
        ["--filter"],
        Some("TEXT"),
        ["inspect"],
        "Restrict --interface to declarations whose path, signature or doc contains TEXT."
    ),
    option!(
        ForeignHeader,
        ["--foreign-header"],
        Some("MODULE"),
        ["inspect"],
        "Print the C header of a foreign module by its full definition path."
    ),
    option!(
        ForeignAbi,
        ["--foreign-abi"],
        Some("MODULE"),
        ["inspect"],
        "Print the ABI text of a foreign module by its full definition path."
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
        ["build", "publish", "register", "deploy"],
        "Select the immutable code store."
    ),
    option!(
        Control,
        ["--control"],
        Some("SOCKET"),
        ["launcher"],
        "Launcher control socket."
    ),
    option!(
        HostState,
        ["--state"],
        Some("DIR"),
        ["host", "register", "deploy", "decide", "replay", "launcher"],
        "Private host state directory; the operator socket is STATE/operator.sock."
    ),
    option!(
        OperatorListen,
        ["--operator-listen"],
        Some("SOCKET"),
        ["launcher"],
        "Operator socket the platform host accepts deploys on."
    ),
    option!(
        PlatformHost,
        ["--platform-host"],
        Some("FILE"),
        ["launcher"],
        "Installed platform host executable to start as the first child."
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
        ["sdk", "call", "build", "publish", "dev"],
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
        CallStream,
        ["--stream"],
        Some("32-HEX"),
        ["call"],
        "Send once at position zero on this saved stream ID."
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
        ObservedRuntime,
        ["--observed-runtime"],
        Some("ARCHIVE"),
        ["core-product", "std-product"],
        "Install the runtime archive for logical charge reports."
    ),
    option!(
        ObservedRecordingRuntime,
        ["--observed-recording-runtime"],
        Some("ARCHIVE"),
        ["core-product", "std-product"],
        "Install the combined charge observation and recording runtime archive."
    ),
    option!(
        Record,
        ["--record"],
        Some("TAPE"),
        ["run", "register"],
        "Build with recording enabled. Run writes TAPE. Register writes owner-only recordings in TAPE as a directory."
    ),
    option!(
        GroupName,
        ["--group"],
        Some("NAME"),
        ["register", "deploy"],
        "The group to register or deploy to."
    ),
    option!(
        Tenant,
        ["--tenant"],
        Some("NAME"),
        ["register"],
        "The tenant that owns the group. Default operator."
    ),
    option!(
        GroupStore,
        ["--store"],
        Some("DIR"),
        ["register"],
        "The group's durable store directory."
    ),
    option!(
        Trusted,
        ["--trusted"],
        None,
        ["register"],
        "Let the group's recipes use grants that need operator trust."
    ),
    option!(
        RequestId,
        ["--request-id"],
        Some("HEX"),
        ["register", "deploy", "decide"],
        "Resend this request instead of starting a new one; for decide, the request to decide."
    ),
    option!(
        RequestStatus,
        ["--status"],
        Some("HEX"),
        ["deploy"],
        "Print where a request stands."
    ),
    option!(
        Tape,
        ["--tape"],
        Some("TAPE"),
        ["replay"],
        "Replay this tape without host grants."
    ),
    option!(
        ReplayUnit,
        ["--unit"],
        Some("NAME"),
        ["replay"],
        "Replay this unit's stream alone (root) and read no other stream."
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
        Laws,
        ["--laws"],
        None,
        ["test"],
        "Observe settled law verdicts in the recording build."
    ),
    option!(
        WriteLaws,
        ["--write-laws"],
        None,
        ["test"],
        "Observe laws and update the executed tests' rows in hirsel.laws."
    ),
    option!(
        LawsBaseline,
        ["--laws-baseline"],
        Some("FILE"),
        ["test"],
        "Compare a full observed test run with the law lock at FILE."
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
        Resident,
        ["--resident"],
        None,
        ["build", "run", "test"],
        "Serve the request from this toolchain's resident compiler, starting one if none runs."
    ),
    option!(
        MaxMemory,
        ["--max-memory"],
        Some("BYTES"),
        ["serve"],
        "After a request, evict least recently used working sets other than its own above BYTES (default 1.75 GiB)."
    ),
    option!(
        Stop,
        ["--stop"],
        None,
        ["serve"],
        "Stop this toolchain's resident compiler."
    ),
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
        ["prof run", "prof replay"],
        "Capture memory; replay also captures budgets. A bare --mem selects sampled mode, the only live mode."
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
            "add",
            "explain",
            "coverage merge",
            "coverage diff"
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
        [
            "build", "publish", "run", "replay", "test", "register", "deploy"
        ],
        "Grant a capability kind. Net takes explicit Connect, Listen, UdpBind, UdpSend, Tls or Resolve rules separated by semicolons. Env requires Env=<pattern>[,<pattern>]. Repeat for more kinds."
    ),
    option!(
        Filter,
        ["--filter"],
        Some("TEXT"),
        ["test"],
        "Select test definition paths containing TEXT."
    ),
    option!(
        TestExact,
        ["--exact"],
        Some("DEFINITION"),
        ["test"],
        "Select a full test definition path (package@version/module/name). Repeat to select more tests. Conflicts with --filter."
    ),
    option!(
        TestJobs,
        ["-j"],
        Some("N"),
        ["test"],
        "Run up to N isolated test children at once. Defaults to the number of CPUs in the process affinity mask."
    ),
    option!(
        LinkOnly,
        ["--link-only"],
        None,
        ["test"],
        "Link each selected test without running it. Check Mach-O structure for Darwin."
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
        TestCache,
        ["--test-cache"],
        Some("DIR"),
        ["test"],
        "Read and write recorded passes under DIR instead of the user's cache."
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
            "sdk"
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
        ["build", "run", "replay", "test", "sdk"],
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
            "sdk",
            "lean extract"
        ],
        "Select the full native target triple."
    ),
    option!(
        CoreIr,
        ["--coreir"],
        None,
        ["lean extract"],
        "Export linked checked CoreIR, layouts and source annexes as JSON."
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
        [
            "build",
            "run",
            "replay",
            "test",
            "pack",
            "sdk",
            "lean extract",
            "lean status",
            "coverage merge"
        ],
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
            "check",
            "image",
            "build",
            "publish",
            "run",
            "replay",
            "test",
            "inspect",
            "sdk",
            "laws check",
            "laws write"
        ],
        "Select a root containing core/ and std/."
    ),
    option!(
        Fresh,
        ["--fresh"],
        None,
        ["dev"],
        "Start a new dev-owned store generation."
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
            "sdk",
            "dry-run",
            "dev",
            "laws check",
            "laws write",
            "lean extract",
            "lean status",
            "lean probe"
        ],
        "Select the database for dry-run, or the request or product store."
    ),
    option!(
        DryRunExe,
        ["--exe"],
        Some("NEW"),
        ["dry-run"],
        "The built candidate executable."
    ),
    option!(
        StoreRelease,
        ["--release"],
        Some("EXE"),
        ["store verify", "store repair"],
        "The release executable whose extractors check or rebuild the store's indexes."
    ),
    option!(
        Inputs,
        ["--inputs"],
        Some("MANIFEST"),
        [
            "check",
            "image",
            "build",
            "publish",
            "run",
            "replay",
            "test",
            "sdk",
            "lean extract",
            "lean status",
            "lean probe"
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
