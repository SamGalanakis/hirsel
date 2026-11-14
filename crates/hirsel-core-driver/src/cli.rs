//! One command/option catalogue drives parsing and user help.
use std::path::Path;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OptionId {
    Json,
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
    Image,
    Lib,
    Help,
    Install,
    FmtCheck,
    Signatures,
    ListRules,
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
    "build", "build [package-dir] [options]", "Build a native executable from its reachable definitions.";
    "run", "run [package-dir] [options] [-- arguments...]", "Build and run a native executable.";
    "test", "test [package-dir] --limit BYTES --runtime ARCHIVE [options]", "Run each discovered @test in an independent process.";
    "image", "image [library-root] [options]", "Publish checked core and std source libraries.";
    "core-product", "core-product [options]", "Produce or inspect the published core package.";
    "std-product", "std-product [options]", "Produce or inspect the published core and std packages.";
    "new", "new <package-dir> [--lib]", "Create a runnable application or library with an example main.";
    "env", "env [--json] [options]", "Report the compiler and selected installation, products and runtime.";
    "fmt", "fmt [--check] <package-dir>", "Format all new source files; refuse malformed input without partial edits.";
    "explain", "explain RULE|debug-lines | explain --list [--json]", "Explain current specification rules or the debug-lines build option.";
}
macro_rules! option {
    ($id:ident, [$($name:literal),+], $value:expr, [$($command:literal),*], $description:literal) => {
        OptionSpec {id:OptionId::$id,names:&[$($name),+],value:$value,commands:&[$($command),*],description:$description}
    }
}
static OPTIONS: &[OptionSpec] = &[
    option!(
        FmtCheck,
        ["--check"],
        None,
        ["fmt"],
        "List differing files without writing; exit 1 when any differ."
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
        ["check", "build", "run", "test"],
        "Check supplied core and std sources."
    ),
    option!(
        SourceStd,
        ["--source-std"],
        None,
        ["check", "build", "run", "test"],
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
        ["check", "build", "run", "test", "env"],
        "Select the published package cache; enables products for check."
    ),
    option!(
        Dir,
        ["--dir"],
        Some("PATH"),
        ["build", "run", "test"],
        "Grant a confined directory. Repeat for more directories."
    ),
    option!(
        Grant,
        ["--grant"],
        Some("KIND"),
        ["build", "run", "test"],
        "Grant a capability kind. Repeat for more kinds."
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
        ["build", "run", "test"],
        "Optimize native code; defaults to LLVM."
    ),
    option!(
        DebugLines,
        ["--debug-lines"],
        Some("on|off"),
        ["build", "run", "test"],
        "Emit native line tables; default on for dev, off for release. Failure positions stay enabled."
    ),
    option!(
        Backend,
        ["--backend"],
        Some("cranelift|llvm"),
        ["build", "run", "test"],
        "Select the native engine; dev defaults to Cranelift."
    ),
    option!(
        Runtime,
        ["--runtime"],
        Some("ARCHIVE"),
        ["build", "run", "test", "env", "core-product", "std-product"],
        "Select the core runtime archive."
    ),
    option!(
        LogicalChargeReport,
        ["--logical-charge-report"],
        Some("FILE"),
        ["build", "run", "test"],
        "Instrument root logical reservation; unsuitable for timings."
    ),
    option!(
        Cc,
        ["--cc"],
        Some("PROGRAM"),
        ["build", "run", "test"],
        "Select the C linker driver."
    ),
    option!(
        Target,
        ["--target"],
        Some("TRIPLE"),
        ["build", "run", "test", "env", "core-product", "std-product"],
        "Select the full native target triple."
    ),
    option!(
        Emulator,
        ["--emulator"],
        Some("FILE"),
        ["run", "test"],
        "Run through this explicit QEMU user executable."
    ),
    option!(
        Sysroot,
        ["--sysroot"],
        Some("DIR"),
        ["run", "test"],
        "Use this QEMU runtime sysroot with --emulator."
    ),
    option!(
        Limit,
        ["--limit"],
        Some("BYTES"),
        ["build", "run", "test"],
        "Set the root limit; mandatory for test."
    ),
    option!(
        Output,
        ["-o", "--output"],
        Some("PATH"),
        ["build", "run", "test"],
        "Write the executable, or test executables, here."
    ),
    option!(
        Entry,
        ["--entry"],
        Some("DEFINITION"),
        ["check", "build", "run"],
        "Select an entry by its full definition path."
    ),
    option!(
        Libraries,
        ["--libraries"],
        Some("ROOT"),
        ["check", "image", "build", "run", "test"],
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
            "run",
            "test",
            "core-product",
            "std-product"
        ],
        "Select the request or product store."
    ),
    option!(
        Inputs,
        ["--inputs"],
        Some("MANIFEST"),
        ["check", "image", "build", "run", "test"],
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
            "run",
            "test",
            "core-product",
            "std-product"
        ],
        "Write exclusive stage instruction measurements."
    ),
    option!(
        Image,
        ["--image"],
        Some("DIRECTORY"),
        [
            "check",
            "build",
            "run",
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
            "Usage: hirsel-core {}\n\n{}\n\nOptions:\n",
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
        let mut result = "Usage: hirsel-core <command> [options]\n\nCommands:\n".to_owned();
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
    let mut invocation = Invocation {
        command,
        options: Vec::new(),
        positional: None,
        arguments: Vec::new(),
    };
    while let Some(argument) = arguments.next() {
        if argument == "--" && name == "run" {
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
