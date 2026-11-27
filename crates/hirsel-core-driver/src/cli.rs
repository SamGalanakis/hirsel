//! Each command selects its argument schema and the request it constructs.
pub use crate::cli_tools::scaffold;

#[path = "cli_requests.rs"]
mod requests;
pub use requests::*;
#[path = "cli_types.rs"]
mod types;
use types::{Argument, CommandSchema, Flag, Kind, NoOption, Positionals, Schema, Switch, Value};
pub use types::{CommandSpec, Invocation, Request};

macro_rules! flags {
    ($($name:ident [$($spelling:literal),+] $($value:literal)? => $description:literal;)*) => {
        $(flags!(@flag $name [$($spelling),+] $($value)? => $description);)*
    };
    (@flag $name:ident [$($spelling:literal),+] => $description:literal) => {
        #[allow(non_upper_case_globals)]
        pub(super) const $name: Switch = Switch(Flag {
            names: &[$($spelling),+], description: $description,
        });
    };
    (@flag $name:ident [$($spelling:literal),+] $value:literal => $description:literal) => {
        #[allow(non_upper_case_globals)]
        pub(super) const $name: Value = Value(Flag {
            names: &[$($spelling),+], description: $description,
        }, $value);
    };
}
#[path = "cli_flags.rs"]
mod flags;

macro_rules! argument {
    ($ty:ident, HELP) => {
        Argument {
            flag: HELP,
            kind: Kind::Help,
        }
    };
    ($ty:ident, REFUSE_MEM) => {
        Argument {
            flag: flags::ProfMem.0,
            kind: Kind::RefuseMem,
        }
    };
    ($ty:ident, ProfMem) => {
        Argument {
            flag: flags::ProfMem.0,
            kind: Kind::OptionalMem(flags::ProfMem.1, $ty::ProfMem),
        }
    };
    ($ty:ident, $flag:ident) => {
        flags::$flag.bind($ty::$flag)
    };
}

const HELP: Flag = Flag {
    names: &["--help", "-h"],
    description: "Show this help.",
};

macro_rules! commands {
    ($($name:literal, $usage:literal, $description:literal;
        $request:ident($ty:ident), $positionals:ident: $($option:ident),*;)*) => {
        pub static COMMANDS: &[CommandSpec] = &[$(CommandSpec {
            name: $name,
            usage: $usage,
            description: $description,
            schema: &Schema::<$ty> {
                positionals: Positionals::$positionals,
                options: &[$(argument!($ty, $option)),*],
                wrap: Invocation::$request,
            },
        }),*];
    };
}

commands! {
    "help", "help [command]", "Show commands or help for one command.";
        Simple(NoOption), One: HELP;
    "check", "check [package-dir] [options]", "Check a package or an explicit input manifest.";
        Compiler(CompilerOption), One:
            PackageStore, Locked, Frozen, HELP, Json, SourceCore, SourceStd, CoreProduct, Entry, Libraries,
            Store, Inputs, Metrics, ProfileFront, Image;
    "lsp", "lsp", "Serve editor diagnostics over the Language Server Protocol on stdio.";
        Lsp(NoOption), None: HELP;
    "lock", "lock [package-dir] [options]", "Resolve a workspace and write or verify its lock.";
        Compiler(CompilerOption), One: PackageStore, Locked, Frozen, HELP;
    "lean", "lean <extract|status|probe>", "Extract checked functions for optional Lean proofs.";
        Simple(NoOption), One: HELP;
    "lean extract", "lean extract MODULE [--coreir] --inputs MANIFEST --store DIRECTORY -o FILE -- FUNCTION...", "Generate Lean definitions or export a checked CoreIR closure.";
        Lean(LeanOption), Tail: HELP, Target, CoreIr, Output, Store, Inputs;
    "lean status", "lean status MODULE --inputs MANIFEST --store DIRECTORY -o FILE -- FUNCTION...", "Report whether a saved extraction is stale.";
        Lean(LeanOption), Tail: HELP, Output, Store, Inputs;
    "lean probe", "lean probe DEFINITION --inputs MANIFEST --store DIRECTORY", "Report instructions the Lean extractor excludes.";
        Lean(LeanOption), One: HELP, Store, Inputs;
    "laws", "laws <check|write> [package-dir] [options]", "Check or refresh the generated law lock.";
        Simple(NoOption), One: HELP;
    "laws check", "laws check [package-dir] [options]", "Check the static columns of the law lock without running tests.";
        Compiler(CompilerOption), One: PackageStore, Locked, Frozen, HELP, Libraries, Store;
    "laws write", "laws write [package-dir] [options]", "Refresh the static columns of the law lock without running tests.";
        Compiler(CompilerOption), One: PackageStore, Locked, Frozen, HELP, Libraries, Store;
    "fetch", "fetch [package-dir] [options]", "Fetch the pinned libraries a workspace uses.";
        Compiler(CompilerOption), One: PackageStore, Locked, Frozen, HELP;
    "add", "add SOURCE [--as ALIAS] [--root sha256:HEX] [--workspace] [--dry-run] [--package-store DIR] [--json]", "Bind a verified library in the current package.";
        Add(AddOption), One: AddAs, AddRoot, AddWorkspace, AddDryRun, PackageStore, Locked, Frozen, HELP, Json;
    "update", "update [ALIAS|ID[@VERSION]] [--dry-run] [--package-store DIR] [--json]", "Refresh dependency bindings and select compatible roots.";
        Update(UpdateOption), One: AddDryRun, PackageStore, Locked, Frozen, HELP, Json;
    "audit", "audit [DIR] [--frozen] [--package-store DIR] [--json]", "Report the authority reached by dependencies.";
        Audit(AuditOption), One: PackageStore, Frozen, HELP, Json;
    "pack", "pack [package-dir] -o FILE [options]", "Publish a library and its dependency closure to an archive.";
        Compiler(CompilerOption), One: PackageStore, Locked, Frozen, HELP, Output;
    "build", "build [package-dir | --recipe HASH] [options]", "Build a native executable or static library from a release recipe.";
        Compiler(CompilerOption), One:
            PackageStore, Locked, Frozen, Recipe, CodeStore, ReleaseManifest, PlaceTimeout, NPlace,
            Recording, HELP, Resident, Json, SourceCore, SourceStd, CoreProduct, Dir, Grant, Release,
            DebugLines, Backend, Runtime, LogicalChargeReport, Cc, Target, Limit, Output, Entry, Libraries,
            Store, Inputs, Metrics, ProfileFront, Image, Emit;
    "publish", "publish PACKAGE [options]", "Check a package, publish its code objects and print the recipe hash.";
        Compiler(CompilerOption), One:
            PackageStore, Locked, Frozen, CodeStore, NPlace, HELP, SourceCore, SourceStd, Dir, Grant,
            Release, Backend, Target, Limit, Entry, Libraries, Store, Inputs, Metrics, ProfileFront, Image;
    "manifest", "manifest show PATH", "Print a release manifest.";
        Simple(NoOption), One: HELP;
    "host", "host --state DIR --admission FILE --max-places N --lifecycle managed|leader [--idle-ms MS]", "Run the launcher and the installed platform host on STATE.";
        Host(HostOption), One: HostState, HELP, Admission, MaxPlaces, IdleMs, Lifecycle;
    "launcher", "launcher --control SOCKET --state DIR --lifecycle managed|leader [--operator-listen SOCKET --admission FILE --max-places N [--platform-host FILE] [--idle-ms MS]]", "Hold the groups' listeners and start their children for the platform host.";
        Launcher(LauncherOption), One:
            Control, HostState, OperatorListen, PlatformHost, HELP, Admission, MaxPlaces, IdleMs, Lifecycle;
    "register", "register RECIPE --state DIR --group NAME --listen ADDR --store DIR --code-store DIR [options] [-- arguments...]", "Add a group to a running host and deploy its first release.";
        Register(RegisterOption), Tail:
            CodeStore, HostState, Record, GroupName, Tenant, GroupStore, Trusted, RequestId, HELP, Grant,
            TapeRetention, Workdir, Listen, FrontHttp, FrontStream, SourceDir, PackageRoot, HostMemory;
    "deploy", "deploy RECIPE --state DIR --group NAME --code-store DIR [options] | deploy --state DIR --status HEX", "Deploy a recipe to a registered group, or report a request.";
        Deploy(DeployOption), One:
            CodeStore, HostState, GroupName, RequestId, RequestStatus, HELP, Grant, ApproveGrant, HeldAll,
            HeldEntry, SourceDir;
    "decide", "decide --state DIR --request-id HEX [--approve-grant +LINE|-LINE ...] [--held release|cancel] [--held-entry PATH=release|cancel ...] [--refuse]", "Approve or refuse the decision a held request awaits.";
        Decide(DecideOption), One: HostState, RequestId, HELP, ApproveGrant, Refuse, HeldAll, HeldEntry;
    "dev", "dev SERVER [--listen ADDR] [options] [-- arguments...]", "Rebuild a local place and restore its open subscriptions.";
        Dev(DevOption), Tail:
            NPlace, HELP, SourceCore, SourceStd, Dir, Grant, Backend, Runtime, Limit, Libraries, Fresh,
            Store, Listen;
    "dry-run", "dry-run --exe NEW --store DIR [-- arguments...]", "Preview a candidate’s migration against a store.";
        Store(StoreOption), Tail: HELP, Store, DryRunExe;
    "store", "store <inspect|stats|verify|repair|backup|restore>", "Inspect, verify, repair, back up or restore a store.";
        Simple(NoOption), One: HELP;
    "store inspect", "store inspect DIR", "List the store's tables, indexes (kind, state, revision), cells and logs, beside a live writer.";
        Store(StoreOption), One: HELP;
    "store stats", "store stats DIR", "Print each collection's rows and bytes and the store's journal and pack files, beside a live writer.";
        Store(StoreOption), One: HELP;
    "store verify", "store verify DIR [--release EXE]", "Scrub a copy of the store; with --release, also walk every index the release binds against its table.";
        Store(StoreOption), One: HELP, StoreRelease;
    "store repair", "store repair DIR --release EXE", "With the store stopped, demote each index the release proves wrong and rebuild every unbuilt index.";
        Store(StoreOption), One: HELP, StoreRelease;
    "store backup", "store backup DIR TARGET | store backup --state DIR --group NAME TARGET", "Copy a stopped store at DIR, or a served group's store through its running host, into the empty or new directory TARGET.";
        Store(StoreOption), Pair: HostState, GroupName, HELP;
    "store restore", "store restore BACKUP DIR", "Restore a backup into the empty or new directory DIR, keeping the store's identity.";
        Store(StoreOption), Pair: HELP;
    "fleet", "fleet <sim>", "Run deterministic Fleet and Directory[K] simulations.";
        Simple(NoOption), One: HELP;
    "fleet sim", "fleet sim [package-dir] [--hosts N] [--seed S] [--rounds R] [--places P] [--partitions K] [--json] [options]", "Drive the hirsel.ai/fleet@1 move kernel and Directory[K] in std.sim with seeded faults and check invariants and SM1 traces.";
        Compiler(CompilerOption), One:
            HELP, Json, SimSeed, FleetHosts, FleetRounds, FleetPlaces, FleetPartitions, FleetPlacement,
            Release, Backend, Runtime, Limit, Libraries, Store;
    "run", "run [package-dir] [options] [-- arguments...]", "Build and run a native executable.";
        Compiler(CompilerOption), Tail:
            PackageStore, Locked, Frozen, Record, HELP, Resident, Json, SourceCore, SourceStd, CoreProduct,
            Dir, Grant, Release, DebugLines, Backend, Runtime, LogicalChargeReport, Cc, Target, Emulator,
            Sysroot, Limit, Output, Entry, Libraries, Store, Inputs, Metrics, ProfileFront, Image, Emit;
    "serve", "serve [--max-memory BYTES] [--stop]", "Serve this toolchain's build, run and test requests from every directory.";
        Serve(ServeOption), One: HELP, MaxMemory, Stop;
    "prof", "prof <command>", "Capture or inspect a CPU profile.";
        Simple(NoOption), One: HELP;
    "prof run", "prof run [package-dir] [build-opts] [--tasks] [--mem[=sampled]] [--out FILE.hprof] [--freq 997] [--pmu LIST] [--stack-bytes 32768] [-- args...]", "Build and sample a native executable from exec until exit.";
        Compiler(CompilerOption), Tail:
            PackageStore, Locked, Frozen, Record, HELP, Resident, ProfTasks, ProfMem, ProfOut, ProfFrequency,
            ProfPmu, ProfStackBytes, Json, SourceCore, SourceStd, CoreProduct, Dir, Grant, Release,
            DebugLines, Backend, Runtime, LogicalChargeReport, Cc, Target, Emulator, Sysroot, Limit, Entry,
            Libraries, Store, Inputs, Metrics, ProfileFront, Image, Emit;
    "prof replay", "prof replay [package-dir | --inputs MANIFEST] --tape TAPE [--entry DEFINITION] [--tasks] [--mem[=exact|sampled]] [--out FILE.hprof]", "Profile serial replay through the recording and profiling runtime.";
        Compiler(CompilerOption), One:
            PackageStore, Locked, Frozen, Tape, ReplayUnit, HELP, ProfTasks, ProfMem, ProfOut, SourceCore,
            SourceStd, CoreProduct, Dir, Grant, Release, DebugLines, Backend, Runtime, LogicalChargeReport,
            Cc, Target, Limit, Output, Entry, Libraries, Store, Inputs, Metrics, Image, Emit;
    "prof attach", "prof attach <PID> [--duration SECS] [--freq 997] [--pmu LIST] [--debug PATH] [--out FILE.hprof]", "Snapshot mappings, then sample a running Linux process without stopping it.";
        Attach(AttachOption), One: HELP, ProfOut, ProfFrequency, ProfPmu, ProfDuration, ProfDebug, REFUSE_MEM;
    "prof tail", "prof tail <FILE.hprof> [--limit 20] [--json]", "Report completed and censored task latencies and the observed causal edges.";
        ProfileQuery(ProfileQueryOption), One: HELP, ProfRows, Json;
    "prof summary", "prof summary <FILE.hprof> [--json]", "Report capture quality and the five hottest functions per event.";
        ProfileQuery(ProfileQueryOption), One: HELP, ProfRows, Json;
    "prof schema", "prof schema <FILE.hprof> [--json]", "List the profile's tables and columns.";
        ProfileQuery(ProfileQueryOption), One: HELP, ProfRows, Json;
    "prof top", "prof top <FILE.hprof> [--by self|inclusive] [--event NAME] [--group def_path|instance] [--filter GLOB] [--limit 20] [--json]", "Rank functions by attributed event periods.";
        ProfileQuery(ProfileQueryOption), One: HELP, ProfBy, ProfEvent, ProfGroup, ProfFilter, ProfRows, Json;
    "prof annotate", "prof annotate <FILE.hprof> <DEF_PATH|FILE:LINE> [--json]", "Show attributed periods and compiler remarks by source line.";
        ProfileQuery(ProfileQueryOption), Pair: HELP, ProfRows, Json;
    "prof peak-mem", "prof peak-mem <FILE.hprof> [--limit 20] [--json]", "Report memory peaks, allocation attribution and budget refusals.";
        ProfileQuery(ProfileQueryOption), One: HELP, ProfRows, Json;
    "prof callers", "prof callers <FILE.hprof> <DEF_OR_SYM> [--depth 5] [--json]", "Show sampled callers of a function.";
        ProfileQuery(ProfileQueryOption), Pair: HELP, ProfRows, ProfDepth, Json;
    "prof callees", "prof callees <FILE.hprof> <DEF_OR_SYM> [--depth 5] [--json]", "Show sampled callees of a function.";
        ProfileQuery(ProfileQueryOption), Pair: HELP, ProfRows, ProfDepth, Json;
    "prof diff", "prof diff <BASE.hprof> <HEAD.hprof> [--by self|inclusive] [--group def_path|instance] [--min-delta-pct 1.0] [--json]", "Compare compatible captures and report changed and unmatched functions.";
        ProfileQuery(ProfileQueryOption), Pair: HELP, ProfBy, ProfGroup, ProfRows, ProfMinDeltaPct, Json;
    "prof sql", "prof sql <FILE.hprof> \"<SQL>\" [--limit 500] [--timeout-ms 5000] [--json]", "Query the profile with one read-only SQL statement.";
        ProfileQuery(ProfileQueryOption), Sql: HELP, ProfRows, ProfTimeout, Json;
    "prof export", "prof export <FILE.hprof> --format pprof --out <PATH>", "Export a pprof profile.";
        ProfileExport(ProfileExportOption), One: HELP, ProfOut, ProfFormat;
    "coverage", "coverage <merge|diff>", "Merge, report and diff saved coverage profiles without running anything.";
        Simple(NoOption), One: HELP;
    "coverage merge", "coverage merge FILE|DIR|@LIST... [--map FILE]... [--base FILE] [--targets FILE] [--json] [-o FILE]", "Merge coverage profiles, and raw files read against their site maps, then report.";
        Coverage(CoverageOption), Many: HELP, Json, Output, CoverageMap, MergeBase, MergeTargets;
    "coverage diff", "coverage diff BASE NEW [--json] | coverage diff PROFILE --test DEFINITION [--json]", "Report the sites one coverage profile reached and the other did not, or one test and the rest of its profile.";
        Coverage(CoverageOption), Pair: HELP, Json, CoverageTest;
    "replay", "replay PACKAGE --tape TAPE [--diagnose] [--outputs DIR] [options]", "Replay a recorded program with no host grants.";
        Compiler(CompilerOption), One:
            PackageStore, Locked, Frozen, HostState, Tape, ReplayUnit, ReplayOutputs, Diagnose, HELP, Json, SourceCore,
            SourceStd, CoreProduct, Dir, Grant, Release, DebugLines, Backend, Runtime, LogicalChargeReport,
            Cc, Target, Emulator, Sysroot, Limit, Output, Entry, Libraries, Store, Inputs, Metrics, Image, Emit;
    "tape", "tape show TAPE --include-credentials [--json]", "Export the header and frames stored in a credential-bearing tape.";
        Tape(TapeOption), One: IncludeCredentials, Json, HELP;
    "tapes", "tapes <check>", "Check recorded calls in an offline Place store.";
        Simple(NoOption), One: HELP;
    "tapes check", "tapes check DIR --recorded EXE --release EXE", "Re-execute calls in the live journal with the other compiler backend.";
        Store(StoreOption), One: HELP, StoreRelease, RecordedExecutable;
    "test", "test [package-dir] [options]", "Run each discovered @test in an independent process. When hirsel.toml contains [tests], set limit, dirs and grants there.";
        Compiler(CompilerOption), One:
            PackageStore, Locked, Frozen, Replay, ExpectVerdict, Laws, WriteLaws, LawsBaseline, HELP,
            Resident, Json, SourceCore, SourceStd, CoreProduct, Dir, Grant, Filter, TestExact, TestJobs,
            LinkOnly, TestTimeout, Rerun, TestCache, PropertyCorpus, Sim, SimSeeds, SimSeed, Release,
            DebugLines, Backend, Runtime, LogicalChargeReport, Cc, Target, Emulator, Sysroot, Limit, Output,
            Libraries, Store, Inputs, Metrics, Image, Coverage, CoverageOut, CoverageBase, CoverageTargets,
            CoverageRuns, SeedBase, Emit;
    "inspect", "inspect [package-dir | ARCHIVE.hpkg | DEPENDENCY | --core | --std] [--interface [--module MODULE] [--filter TEXT] [--json] | --foreign-header MODULE | --foreign-abi MODULE | --dependents DEF [--transitive] | --affected-tests DEF | --unused [--library]]", "Inspect a package's declarations and dependencies. The default is --interface.";
        Compiler(CompilerOption), One:
            PackageStore, Locked, Frozen, Interface, InspectModule, InspectFilter, ForeignHeader, ForeignAbi,
            Dependents, Transitive, AffectedTests, Unused, InspectLibrary, InspectStd, InspectCore, HELP,
            Json, SourceCore, Libraries, Store;
    "image", "image [library-root] [options]", "Publish checked core and std source libraries.";
        Compiler(CompilerOption), One: HELP, Json, Libraries, Store, Inputs, Metrics;
    "core-product", "core-product [options]", "Produce or inspect the published core package.";
        Compiler(CompilerOption), None:
            EmbedRuntime, RecordingRuntime, ProfRuntime, ProfRecordingRuntime, ObservedRuntime, ObservedRecordingRuntime,
            Install, LinkKit, HELP, Json, Name, Runtime, Target, Store, Metrics, Image;
    "std-product", "std-product [options]", "Produce or inspect the published core and std packages.";
        Compiler(CompilerOption), None:
            EmbedRuntime, ExecHelper, RecordingRuntime, ProfRuntime, ProfRecordingRuntime, ObservedRuntime, ObservedRecordingRuntime,
            Install, LinkKit, HELP, Json, Name, Runtime, Target, Store, Metrics, Image;
    "new", "new <package-dir> [--lib]", "Create a runnable application or library with an example main.";
        New(NewOption), One: HELP, Lib;
    "env", "env [--json] [options]", "Report the compiler and selected installation, products and runtime.";
        Compiler(CompilerOption), None: HELP, Json, CoreProduct, Runtime, Target;
    "fmt", "fmt [--check] [--signatures|--data] <FILE|DIR|--stdin>", "Format a source file, a source directory, or standard input.";
        Fmt(FmtOption), One: FmtCheck, FmtStdin, FmtData, Signatures, HELP;
    "explain", "explain RULE|MODULE|debug-lines|primer | explain --list [--json]", "Explain specification rules and find library declarations.";
        Explain(ExplainOption), One: ListRules, HELP, Json;
    "primer", "primer", "Print the Hirsel primer: the language in one page.";
        Simple(NoOption), One: HELP;
    "sdk", "sdk <ts|json|swift>", "Export bindings or the schema for a place.";
        Simple(NoOption), One: HELP;
    "sdk ts", "sdk ts <program> -o DIR [options]", "Generate TypeScript bindings and manifest.json.";
        Sdk(SdkOption), One:
            PackageStore, Locked, Frozen, PlaceTimeout, NPlace, HELP, SourceCore, SourceStd, CoreProduct,
            Release, DebugLines, Backend, Runtime, Cc, Target, Limit, Output, Entry, Libraries, Store,
            Inputs, Emit;
    "sdk json", "sdk json <program> [options]", "Write the SDK schema manifest to stdout.";
        Sdk(SdkOption), One:
            PackageStore, Locked, Frozen, PlaceTimeout, NPlace, HELP, SourceCore, SourceStd, CoreProduct,
            Release, DebugLines, Backend, Runtime, Cc, Target, Limit, Entry, Libraries, Store, Inputs, Emit;
    "sdk swift", "sdk swift <program> -o DIR [options]", "Generate Swift bindings and an Apple XCFramework.";
        Sdk(SdkOption), One:
            PackageStore, Locked, Frozen, NPlace, HELP, SourceCore, SourceStd, CoreProduct,
            Release, DebugLines, Backend, Runtime, Cc, Target, Limit, Output, Entry, Libraries, Store,
            Inputs, Emit;
    "call", "call SERVER --url ws://HOST:PORT/PATH [options] [-- ENTRY [--FLAG VALUE...]]", "List a running place's entries, or call one, typed from the server's Describe.";
        Call(CallOption), Tail: PlaceTimeout, NPlace, Url, CallTimeout, CallStream, HELP;
}
#[path = "cli_help.rs"]
mod help_text;
pub use help_text::help;
pub(crate) use help_text::named;
#[path = "cli_parse.rs"]
mod parsing;
pub use parsing::{Action, parse};
