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
    TestTimeout,
    Release,
    DebugLines,
    Backend,
    Runtime,
    LogicalChargeReport,
    Cc,
    Target,
    Emulator,
    Sysroot,
    Limit,
    Output,
    Entry,
    Libraries,
    Store,
    Inputs,
    Metrics,
    ProfileFront,
    Image,
    Lib,
    Help,
    Install,
    FmtCheck,
    FmtStdin,
    Signatures,
    ListRules,
    SdkOut,
    PlaceTimeout,
    NPlace,
    Url,
    CallTimeout,
    Control,
    HostState,
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
    "build", "build [package-dir | --recipe HASH] [options]", "Assemble a native executable from a release recipe.";
    "publish", "publish PACKAGE [options]", "Check a package, publish its code objects and print the recipe hash.";
    "manifest", "manifest show PATH", "Print a release manifest.";
    "host", "host RECIPE --control SOCKET --state DIR --listen ADDR [options] [-- arguments...]", "Supervise a place and accept deploy requests.";
    "deploy", "deploy RECIPE --control SOCKET --code-store DIR [options]", "Submit a recipe and its missing code objects to a host.";
    "run", "run [package-dir] [options] [-- arguments...]", "Build and run a native executable.";
    "replay", "replay PACKAGE --tape TAPE [--diagnose] [options]", "Replay a recorded program with no host grants.";
    "tape", "tape show TAPE --include-credentials", "Export the header and frames stored in a credential-bearing tape.";
    "test", "test [package-dir] --limit BYTES --runtime ARCHIVE [options]", "Run each discovered @test in an independent process.";
    "image", "image [library-root] [options]", "Publish checked core and std source libraries.";
    "core-product", "core-product [options]", "Produce or inspect the published core package.";
    "std-product", "std-product [options]", "Produce or inspect the published core and std packages.";
    "new", "new <package-dir> [--lib]", "Create a runnable application or library with an example main.";
    "env", "env [--json] [options]", "Report the compiler and selected installation, products and runtime.";
    "fmt", "fmt [--check] [--signatures] <FILE|DIR|--stdin>", "Format a source file, a source directory, or standard input.";
    "explain", "explain RULE|debug-lines | explain --list [--json]", "Explain current specification rules or the debug-lines build option.";
    "sdk", "sdk ts <program> --out DIR [options]", "Generate browser and Node ES modules and TypeScript declarations.";
    "call", "call SERVER --url ws://HOST:PORT/PATH [options] [-- ENTRY [--FLAG VALUE...]]", "List a running place's entries, or call one, typed from the server's Describe.";
}
macro_rules! option {
    ($id:ident, [$($name:literal),+], $value:expr, [$($command:literal),*], $description:literal) => {
        OptionSpec {id:OptionId::$id,names:&[$($name),+],value:$value,commands:&[$($command),*],description:$description}
    }
}
static OPTIONS: &[OptionSpec] = &[
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
        ["host", "deploy"],
        "Host control socket."
    ),
    option!(
        HostState,
        ["--state"],
        Some("DIR"),
        ["host", "replay"],
        "Private host state directory."
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
        SdkOut,
        ["--out"],
        Some("DIR"),
        ["sdk"],
        "Write the generated SDK to this directory."
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
    option!(Help, ["--help", "-h"], None, [], "Show this help."),
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
        ["check", "build", "publish", "run", "replay", "test", "sdk"],
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
        ["build", "publish", "run", "replay", "test"],
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
        TestTimeout,
        ["--test-timeout"],
        Some("SECONDS"),
        ["test"],
        "Bound each test's execution, excluding build time."
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
        "Emit native line tables; default off on every profile. Failure positions stay enabled."
    ),
    option!(
        Backend,
        ["--backend"],
        Some("cranelift|llvm"),
        ["build", "publish", "run", "replay", "test", "sdk"],
        "Select the native engine; dev defaults to Cranelift."
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
        [
            "build",
            "run",
            "replay",
            "test",
            "core-product",
            "std-product",
            "sdk",
            "call"
        ],
        "Select the C linker driver; an install records it for its requests."
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
        "Set the root limit; mandatory for test."
    ),
    option!(
        Output,
        ["-o", "--output"],
        Some("PATH"),
        ["build", "run", "replay", "test"],
        "Write the executable, or test executables, here."
    ),
    option!(
        Entry,
        ["--entry"],
        Some("DEFINITION"),
        ["check", "build", "publish", "run", "sdk"],
        "Select an entry by its full definition path."
    ),
    option!(
        Libraries,
        ["--libraries"],
        Some("ROOT"),
        [
            "check", "image", "build", "publish", "run", "replay", "test", "sdk"
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
    Invoke(Invocation),
}
fn named(name: &str) -> Result<&'static CommandSpec, String> {
    COMMANDS
        .iter()
        .find(|c| c.name == name)
        .ok_or_else(|| format!("unknown command `{name}`; use help"))
}
pub fn help(command: Option<&CommandSpec>) -> String {
    if let Some(command) = command {
        let mut result = format!(
            "Usage: hirsel {}\n\n{}\n\nOptions:\n",
            command.usage, command.description
        );
        for option in OPTIONS
            .iter()
            .filter(|o| o.commands.is_empty() || o.commands.contains(&command.name))
        {
            let label = format!(
                "{}{}",
                option.names.join(", "),
                option.value.map_or(String::new(), |v| format!(" {v}"))
            );
            result.push_str(&format!("  {label:<34} {}\n", option.description));
        }
        result
    } else {
        let mut result = "Usage: hirsel <command> [options]\n\nCommands:\n".to_owned();
        for command in COMMANDS {
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
    let mut arguments = arguments.into_iter();
    let Some(name) = arguments.next() else {
        return Ok(Action::Help(help(None)));
    };
    if is_help(&name) {
        return Ok(Action::Help(help(None)));
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
            .map(|name| {
                if is_help(&name) {
                    Ok(command)
                } else {
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
        if argument == "--" && matches!(name.as_str(), "run" | "call" | "host") {
            invocation.arguments.extend(arguments);
            break;
        }
        if argument.starts_with('-') {
            let option = OPTIONS
                .iter()
                .find(|o| o.names.contains(&argument.as_str()))
                .filter(|o| o.commands.is_empty() || o.commands.contains(&command.name))
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
        "pub fn main() -> Result[(), Exit] {\n    if lib.answer() == 42 { Ok(()) } else { Error(Exit(status: 1)) }\n}\n"
    } else {
        "pub fn main() -> Result[(), Exit] {\n    Ok(())\n}\n"
    };
    for (name, source) in std::iter::once(("main.hirsel", main))
        .chain(library.then_some(("lib.hirsel", "pub fn answer() -> u64 {\n    42\n}\n")))
    {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(directory.join(name))?
            .write_all(source.as_bytes())?;
    }
    Ok(())
}
