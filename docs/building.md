# Building and running Hirsel programs

Hirsel is a systems-level, general-purpose language whose primary user is a
coding agent. Its fixed goals:

- native performance;
- every definition checked only against the signatures it uses;
- one way per job;
- all authority passed as parameters (capabilities);
- reference counting with no lifetime variables;
- deterministic behaviour outside capabilities.

The [specification](spec/index.md) defines the language, its tasks and its
capabilities. To work on the compiler, runtime or libraries, see
[Contributing to Hirsel](contributing.md); the driver's internals are in
[its README](../crates/hirsel-core-driver/README.md).

## The installation

An installation is one self-contained prefix: the `hirsel` executable, the published
core and std packages (`core-packages`), the runtime archive for each supported target
(`runtime/`), the link kit, a static `bin/ld.lld` and the shared libraries the compiler
loads (`lib/`). It reads nothing outside its prefix and may be copied or moved.
`hirsel env` reports the compiler, installed root, package cache, target and runtime a
request would use; `--json` prints the same as JSON and `--target` selects another
target's runtime.

`hirsel help` lists the commands and `hirsel help COMMAND`, `COMMAND --help` or
`COMMAND -h` prints one command's options. A command name that is not in that list
exits 2 with `hirsel: unknown command`.

## Start a program

```sh
hirsel new app          # app/main.hirsel, a runnable main
hirsel new tools --lib  # tools/lib.hirsel and a main.hirsel that exercises it
hirsel run app
```

A package is a directory of `.hirsel` files. `hirsel check DIR` checks every
definition and declaration in it; `--json` prints the outcome, its diagnostics, its
incomplete features and its resource refusals. `hirsel explain RULE` prints a
specification rule (for example `hirsel explain CAP-4`), and `hirsel explain --list`
lists them all.

`hirsel fmt FILE` formats one file in place. `hirsel fmt DIR` formats every
`.hirsel` file in the directory tree. A source refusal prevents writes to the
whole batch. `hirsel fmt --stdin` reads one source from standard input and sends
the formatted bytes to standard output. `--check` exits 1 when the input differs
from its formatted form. For a file or directory it lists differing paths, and
for stdin it writes no formatted bytes. Use `--signatures` for core declarations
that omit function bodies.

## Build and run

`hirsel build DIR -o OUT` builds a native executable from the definitions `main`
reaches; `hirsel run DIR [-- arguments...]` builds it and runs it. Development builds
default to Cranelift and `--release` to LLVM; `--backend cranelift|llvm` selects the
engine explicitly. `--debug-lines on` emits native line tables; failure positions are
reported either way.

```sh
hirsel build app --release -o hello
hirsel run app --dir . --grant Stdout -- input.txt normal
```

`main` receives its authority as parameters ([CAP-4](spec/capabilities.md#cap-4-entry-grants)).
`--dir PATH` grants a confined directory, one per Dir parameter in order, and
`--grant KIND` grants Clock, Random, Stdout, Stderr, Stdin, Process or
Tasks. Net requires `--grant 'Net=<dir> <prefix>/<len> <lo>-<hi>[;<rule>]'`
with explicit rules ([Net sockets](spec/net.md)). Env requires
`--grant 'Env=<pattern>[,<pattern>]'`, where a pattern is a variable name or a prefix
followed by `*` ([Env](../core/std/api/env.md)). A missing grant is refused before native code is emitted, naming the parameter
and the option it needs. Everything after `--` is passed to the program as `Args`.

The installation links with its own `bin/ld.lld` through the C driver it recorded at
install; `--cc PROGRAM` selects another. `--target aarch64-unknown-linux-gnu` builds
for AArch64; `run` and `test` then need `--emulator QEMU` and `--sysroot DIR` for the
AArch64 runtime libraries.

## Record and replay

`hirsel run app --record session.tape [grants] [-- arguments...]` runs the
recording build of `app` with recording on, into a new, owner-only file. The
file may hold bearer tokens and other credentials, so choose a path that does
not exist yet.

```sh
hirsel run app --record session.tape --dir inputs --grant Stdout -- input.json
hirsel replay app --tape session.tape
hirsel tape show session.tape --include-credentials
hirsel replay app --tape session.tape --diagnose
hirsel build app --recording -o app-rec
hirsel test app --limit 64M --replay
```

Recording is an optional switch, off by default, independent of `--release`
and `--backend`. A build without it has no recording machinery; `--recording`,
`--record`, `replay` and `test --replay` use a recording build of the same program.
Recording and replay, what a recording holds and the replay verdicts are in
[Recording and replay](recording.md). A recording is for diagnosis and testing.
Work that must survive a crash goes through the store and its step library
([CAP-9](spec/capabilities.md#cap-9-recording-is-outside-the-program)).

## Symbolize a runtime panic

The shipping native runtime prints raw PCs when `RUST_BACKTRACE=1` or
`RUST_BACKTRACE=full`. Keep the unstripped executable and match its
`readelf -n` build ID with the ID in the trace. For a frame in that
executable, subtract the reported executable load bias and one byte from
the return PC before passing the resulting ELF address to LLVM.

```sh
llvm-symbolizer --obj=./program "$(printf '%#x' "$((pc - load_bias - 1))")"
```

Use the return PC and load bias from the same panic. Frames from other shared
objects need the corresponding object and mapping from the process.
The trace can stop at the first generated Hirsel frame. Build with
`--debug-lines on` to resolve that frame to a Hirsel source position.

## Run tests

```sh
hirsel test app --limit 1048576 --grant Stdout
```

`hirsel test` discovers each `@test` function ([PROG-4](spec/programs.md#prog-4-tests))
in the package, builds it and runs it in its own process. `--limit BYTES` is
mandatory and is each test run's root limit; `--dir` and `--grant` configure
capabilities as for `run`. `--filter TEXT` selects the tests whose full definition
path contains TEXT, and `--test-timeout SECONDS` bounds each test's execution. Every
selected test runs whatever an earlier one did; no matching test exits 2.

## Limits

### Core parser nesting

`hirsel_core_syntax::PARSER_NESTING_LIMIT` is **128 simultaneously active recursive
productions**. It is an implementation resource ceiling, not a restriction on Hirsel
programs. Exhaustion returns `ResourceRefusal::ParserStack` with that limit and the
file-relative byte span of the token where the next production would start. The CLI
and driver report it as a resource refusal (below), never a source-language refusal or
a successful parse. The formatter does not print an incomplete tree.

### Resource refusals

A resource refusal ([PROG-6](spec/programs.md)) is the implementation declining a
program that passes one of its limits. It names no rule and is not a meaning of the
program, so the driver keeps it apart from both rule refusals and incomplete (unbuilt)
features: `CheckProduct.resources` and `Reachable.resources`, each a
`ResourceRefusal { primary, message }` with no rule field. Four sources feed it, each
with an exact primary site and a message carrying the measured amount and the limit:

| Source | Limit | Primary site |
| --- | --- | --- |
| parser | `PARSER_NESTING_LIMIT` productions; u32-indexed source bytes | the token where the next production starts; the file start |
| resolver | `RESOLVE_NESTING_LIMIT` expression depth | the expression past the limit |
| front layout | the engine's 64-bit native size range | the field whose placement passes it |
| instance collection | `NativeOptions.instance_limit` (65 536 in the CLI) | the definition of the instance past the limit |

`check --json` and `build --json` carry them in a `resources` array of
`{primary, message}`; stderr prints `resource <module> <start>..<end>: <message>`.
The command exits **2**, as for an incomplete outcome; a rule refusal exits 1.
"0 gaps" in census reports counts only unbuilt features: the instance limit is not a
gap stop.

Reuse: the parser, resolver and layout limits are compiler constants (layout recipes
are identical on every supported target), so a stored product, keyed by compiler
identity, is never served across a change to them. The instance limit belongs to the
build: instance collection runs per build from the stored product and its refusal is
never stored, so the same stored product is refused under a small limit and accepted
under a large one.

One shared guard runs before recursive expressions, compound expressions, types,
patterns and blocks. Compound expressions include labels and `else if`; type recursion
includes generic arguments and callable parameter/result types. Several guarded
productions may be active for one pair of source delimiters. Attributes, qualified
paths, lists of declarations/parameters and postfix chains use loops; they have no
recursive nesting charge. The limit applies to mixed constructs through one counter.

On x86-64 with Rust 1.99, stack-address probes across those paths measured at most
9,120 bytes between guarded entries in an unoptimized build, 1,488 in the checked
build and 1,977 in release. The unoptimized main-thread and default Rust-worker
measurements agree. A conservative allowance of 12 KiB per entry gives 1.5 MiB at
128 entries, leaving 512 KiB for caller and leaf frames on the smallest supported
stack, a 2 MiB default Rust worker (also used by unit tests). The measured Linux main
thread has an 8 MiB stack, and the CLI parses on it. No stack size is enlarged
to accommodate parsing. The boundary and 100,000-depth cases cover each recursive
path, and long iterative paths remain accepted. Recheck the stack allowance when
changing parser frames, compiler profiles or supported platforms.

