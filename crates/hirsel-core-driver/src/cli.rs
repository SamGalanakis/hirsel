//! One command/option catalogue drives parsing and user help.
pub use crate::cli_tools::scaffold;

#[path = "cli_types.rs"]
mod types;
pub use types::{CommandSpec, Invocation, OptionId};

struct OptionSpec {
    id: OptionId,
    names: &'static [&'static str],
    value: Option<&'static str>,
    commands: &'static [&'static str],
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
    "lean", "lean <extract|status|probe>", "Extract checked functions for optional Lean proofs.";
    "lean extract", "lean extract MODULE [--coreir] --inputs MANIFEST --store DIRECTORY -o FILE -- FUNCTION...", "Generate Lean definitions or export a checked CoreIR closure.";
    "lean status", "lean status MODULE --inputs MANIFEST --store DIRECTORY -o FILE -- FUNCTION...", "Report whether a saved extraction is stale.";
    "lean probe", "lean probe DEFINITION --inputs MANIFEST --store DIRECTORY", "Report instructions the Lean extractor excludes.";
    "laws", "laws <check|write> [package-dir] [options]", "Check or refresh the generated law lock.";
    "laws check", "laws check [package-dir] [options]", "Check the static columns of the law lock without running tests.";
    "laws write", "laws write [package-dir] [options]", "Refresh the static columns of the law lock without running tests.";
    "fetch", "fetch [package-dir] [options]", "Fetch the pinned libraries a workspace uses.";
    "add", "add SOURCE [--root sha256:HEX] [--workspace] [--update] [--dry-run] [--package-store DIR] [--json]", "Bind a verified library in the current package.";
    "pack", "pack [package-dir] -o FILE [options]", "Publish a library and its dependency closure to an archive.";
    "build", "build [package-dir | --recipe HASH] [options]", "Assemble a native executable from a release recipe.";
    "publish", "publish PACKAGE [options]", "Check a package, publish its code objects and print the recipe hash.";
    "manifest", "manifest show PATH", "Print a release manifest.";
    "host", "host --state DIR [--max-places N] [--idle-ms MS]", "Run the launcher and the installed platform host on STATE.";
    "launcher", "launcher --control SOCKET --state DIR [--operator-listen SOCKET --platform-host FILE --max-places N --idle-ms MS]", "Hold the groups' listeners and start their children for the platform host.";
    "register", "register RECIPE --state DIR --group NAME --listen ADDR --store DIR --code-store DIR [options] [-- arguments...]", "Add a group to a running host and deploy its first release.";
    "deploy", "deploy RECIPE --state DIR --group NAME --code-store DIR [options] | deploy --state DIR --status HEX", "Deploy a recipe to a registered group, or report a request.";
    "dev", "dev SERVER [--listen ADDR] [options] [-- arguments...]", "Rebuild a local place and restore its open subscriptions.";
    "dry-run", "dry-run --exe NEW --store DIR [-- arguments...]", "Preview a candidate’s migration against a store.";
    "store", "store <backup|restore>", "Back up a store or restore one from a backup.";
    "store backup", "store backup DIR TARGET", "Copy the store at DIR, at a durable head, into the empty or new directory TARGET.";
    "store restore", "store restore BACKUP DIR", "Restore a backup into the empty or new directory DIR, keeping the store's identity.";
    "run", "run [package-dir] [options] [-- arguments...]", "Build and run a native executable.";
    "serve", "serve [package-dir] [--store DIRECTORY] [--max-memory BYTES] [--stop]", "Serve the package's build, run and test requests from a resident compiler.";
    "prof", "prof <command>", "Capture or inspect a CPU profile.";
    "prof run", "prof run [package-dir] [build-opts] [--tasks] [--mem[=sampled]] [--out FILE.hprof] [--freq 997] [--pmu LIST] [--stack-bytes 32768] [-- args...]", "Build and sample a native executable from exec until exit.";
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
    "inspect", "inspect [package-dir | ARCHIVE.hpkg | DEPENDENCY | --core | --std] [--interface [--module MODULE] [--filter TEXT] [--json] | --foreign-header MODULE | --dependents DEF [--transitive] | --affected-tests DEF | --unused [--library]]", "Inspect a package's declarations and dependencies. The default is --interface.";
    "image", "image [library-root] [options]", "Publish checked core and std source libraries.";
    "core-product", "core-product [options]", "Produce or inspect the published core package.";
    "std-product", "std-product [options]", "Produce or inspect the published core and std packages.";
    "new", "new <package-dir> [--lib]", "Create a runnable application or library with an example main.";
    "env", "env [--json] [options]", "Report the compiler and selected installation, products and runtime.";
    "fmt", "fmt [--check] [--signatures] <FILE|DIR|--stdin>", "Format a source file, a source directory, or standard input.";
    "explain", "explain RULE|MODULE|debug-lines|primer | explain --list [--json]", "Explain specification rules and find library declarations.";
    "primer", "primer", "Print the Hirsel primer: the language in one page.";
    "sdk", "sdk <ts|json>", "Export bindings or the schema for a place.";
    "sdk ts", "sdk ts <program> -o DIR [options]", "Generate TypeScript bindings and manifest.json.";
    "sdk json", "sdk json <program> [options]", "Write the SDK schema manifest to stdout.";
    "call", "call SERVER --url ws://HOST:PORT/PATH [options] [-- ENTRY [--FLAG VALUE...]]", "List a running place's entries, or call one, typed from the server's Describe.";
}
macro_rules! option {
    ($id:ident, [$($name:literal),+], $value:expr, [$($command:literal),*], $description:literal) => {
        OptionSpec {id:OptionId::$id,names:&[$($name),+],value:$value,commands:&[$($command),*],description:$description}
    }
}
#[path = "cli_options.rs"]
mod options;
use options::OPTIONS;
#[path = "cli_package_options.rs"]
mod cli_package_options;
fn options() -> impl Iterator<Item = &'static OptionSpec> {
    cli_package_options::OPTIONS.iter().chain(OPTIONS)
}
#[path = "cli_help.rs"]
mod help_text;
pub use help_text::help;
pub(crate) use help_text::named;
#[path = "cli_parse.rs"]
mod parsing;
pub use parsing::{Action, parse};
