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
capabilities. The driver's internals are in
[its README](../crates/hirsel-core-driver/README.md).

## The installation

Install a Linux release with `curl -fsSL https://hirsel.ai/install.sh | sh` (requires glibc 2.28+).
An installation is a relocatable prefix containing `hirsel`, the published `core`, `std`, `store@1` and
`runner@1` packages (including prebuilt release standard-library objects), target runtime archives,
the bundled native link kit (`link/kit/`), `bin/ld.lld`, `libexec/hirsel/`, and the LLVM libraries
shipped with the compiler. Installed toolchains use the bundled link kit and do **not** require
host C development packages (`libc6-dev` / `libgcc-N-dev`). Host C development packages are only
needed when linking with a custom `--cc` driver outside the bundled link kit. If any link input is
missing, Hirsel names the first missing input before invoking the linker.

`hirsel --version` prints the toolchain version, compiler identity and registered
runtime archive identities by target.
`hirsel env` reports the compiler, installed root, package cache, target and runtime a
request would use; `--json` prints the same as JSON and `--target` selects another
target's runtime.

### macOS from source

On Apple silicon the compiler builds from a checkout and targets `aarch64-apple-darwin`,
its default target there. It needs the Xcode command line tools and Homebrew's LLVM 23.1
(23.1.5 or a later 23.1 patch release) in place of the pinned Linux LLVM tree:

```sh
xcode-select --install
brew install llvm
ln -s "$(brew --prefix llvm)" tools/buck2/toolchains/llvm-files
cargo build --release --bin hirsel
cargo rustc -p hirsel-core-rt --release --lib --crate-type staticlib \
  --features standalone,mimalloc --target-dir target/runtime
export HIRSEL_CORE_RT_ARCHIVE="$PWD/target/runtime/release/libhirsel_core_rt.a"
```

An uninstalled compiler links with `/usr/bin/clang` (or `--cc`) and the runtime archive that
`HIRSEL_CORE_RT_ARCHIVE` (or `--runtime`) names. `check`, `fmt`, `run`, `test` and `build`
work with both backends. To record and replay, build a second archive with
`--features standalone,mimalloc,record` and pass it with `--runtime`.

macOS provides the Linux mechanisms these features use in its own way:

- **Debug info.** ld64 never copies DWARF into an executable, so every build writes
  `PROGRAM.dSYM` beside the executable, matched by its UUID; `lldb` reads Hirsel source
  lines from it. `--debug-lines on` writes the same bundle.
- **Exec.** Children start with `posix_spawn` and are watched by a kqueue `NOTE_EXIT` in
  place of a pidfd. An image on a read-only volume (such as `/bin` or `/usr/bin` on the
  sealed system volume) runs in place, since macOS kills a copy of a platform binary.
  Any other image runs from a private, read-only, user-immutable copy; unlike a sealed
  memfd, the run's own user could clear that flag. There is no subreaper, so a child's own
  children are not adopted.
- **Net Unix.** Bind, connect and send-to resolve the socket's name beneath its pinned
  directory with a per-thread working directory (`pthread_fchdir_np`). macOS cannot hold a
  socket's inode, so a connect or send-to refuses a socket that replaced the one resolved.
- **The launcher.** A launched run is forked with the descriptors it holds closed one by one
  (there is no `close_range`), runs a private copy of its verified image, and kills itself
  when the launcher exits (there is no `PR_SET_PDEATHSIG`). Without cgroups, ending a run
  kills its session's process group.
- **File watching.** The code store watches its directories and each cached object with
  kqueue.
- **Foreign archives.** Archives hold arm64 Mach-O objects with ABI text in
  `__TEXT,__hirsel_abi`; `HIRSEL_TARGET=aarch64-apple-darwin sh build.sh` builds the
  `sqlite` and `ffi_laws` archives.
- **Profiling.** `hirsel prof run` needs a profiling runtime archive (build one with
  `--features standalone,mimalloc,prof` and pass it with `--runtime`): the run samples its
  own running threads by frame records, since macOS lets a process read only its own
  threads' registers. `hirsel prof replay` works as on Linux; `hirsel prof attach` and
  `--mem` are not available.
- **Memory limit.** macOS refuses every `RLIMIT_DATA` change, so the guard checks
  `HIRSEL_MEMORY_LIMIT` against the process's physical footprint (which counts LLVM's
  allocations too) after every MiB a thread allocates and before any larger allocation.
  Guarded children inherit the variable; other children are not limited.
- **Failure positions.** ld64 keeps no relocations to show which failure sites a release
  link kept, so a release executable carries every site's source position, as a
  development build does.

Contained groups need Linux namespaces and are refused on macOS.

The driver's unit tests take one runtime archive per variant, each built as above with
its features: `HIRSEL_CORE_RT_ARCHIVE` (`standalone,mimalloc`),
`HIRSEL_CORE_RT_RECORD_ARCHIVE` (`standalone,mimalloc,record`),
`HIRSEL_CORE_RT_OBSERVED_ARCHIVE` (`standalone,charge-observation`),
`HIRSEL_CORE_RT_OBSERVED_RECORD_ARCHIVE` (`standalone,charge-observation,record`),
`HIRSEL_CORE_RT_PROF_ARCHIVE` (`standalone,mimalloc,prof`),
`HIRSEL_CORE_RT_PROF_RECORD_ARCHIVE` (`standalone,prof,record`) and
`HIRSEL_CORE_RT_SCRIPTED_RECORD_ARCHIVE` (`testing,memcheck,charge-observation,record`).
Tests of Linux mechanisms (`ld.lld` links, GNU `--wrap` interposition, `/proc`, perf
attach) are compiled only on Linux.

`hirsel help` lists the commands and `hirsel help <command>`, `COMMAND --help` or
`COMMAND -h` prints one command's options. A command name that is not in that list
exits 2 with `hirsel: unknown command`.

## Start a program

```sh
hirsel new app
cd app
hirsel check
hirsel test
hirsel run
hirsel build
./program
```

A package is a directory of `.hirsel` files. `check`, `test`, `run` and `build`
default to the current directory. `new` creates a runnable `main`, a test and its
limit in `hirsel.toml`. With `--lib` it also creates `lib.hirsel`.
`hirsel check DIR` checks every
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
engine explicitly. Native builds always emit debug rows and write detached DWARF
alongside the executable; `--debug-lines on` additionally embeds DWARF in the
executable (see [CLI specification](spec/cli.md)). Failure positions are reported
either way.

```sh
hirsel build app --release -o hello
hirsel run app --dir . --grant Stdout -- input.txt normal
```

`main` receives its authority as parameters ([CAP-4](spec/capabilities.md#cap-4-entry-grants)).
`--dir PATH` grants a confined directory, one per Dir parameter in order, and
`--grant KIND` grants `Clock`, `Random`, `Stdout`, `Stderr`, `Stdin`, `Process`, or
`Tasks`, as well as parameterized rules for `Net` (TCP/UDP/TLS/Resolve and Unix-domain
sockets, [Net sockets](spec/net.md)), `Env` ([Env](../core/std/api/env.md)), `Secret`,
`Cred`, `Foreign`, and `Exec` (run `hirsel help run` for exact syntax). A missing grant is refused before native code is emitted, naming the parameter
and the option it needs. Everything after `--` is passed to the program as `Args`.
`run` exits with the program's status. A `main` that returns `Error(e)` writes
`e` as Hirsel source after `error: `, with every `Secret` shown as `[secret]`; an
`Exit` writes nothing ([PROG-3](spec/programs.md#prog-3-entry)). For every nonzero
status `run` then adds one stderr line naming the outcome, so
`Error(Serve(Configuration))` ends the run with `error: Serve(Configuration)` and
`hirsel: main returned an error, exit status 1`. A directory grant that cannot be
opened stops the program before `main` with status 103 and
``not started: directory grant `PATH` (RESOLVED): ERROR``, where the resolved path
appears for a relative grant.

The installation invokes its own `bin/ld.lld` using the link recipe captured at
packaging. `--cc PROGRAM` selects a C driver to supply another recipe.
`--target aarch64-unknown-linux-gnu` builds
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
hirsel test app --limit 67108864 --replay
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
The trace can stop at the first generated Hirsel frame. Detached DWARF is written
alongside native executables by default; pass `--debug-lines on` to also embed DWARF
in the executable for self-contained symbolization.

## Run tests

```sh
hirsel test app --limit 1048576 --grant Stdout
```

`hirsel test` discovers each `@test` function ([PROG-4](spec/programs.md#prog-4-tests))
in the package and runs each execution in its own process. A package whose
`hirsel.toml` declares its tests' run configuration (PROG-4) runs under it, and
`--limit`, `--dir` and `--grant` are refused. Use the `limit`, `dirs` and
`grants` keys in `[tests]`. Otherwise `--limit BYTES` is
mandatory and is each test run's root limit; `--dir` and `--grant` configure
capabilities as for `run`. A test receives only the grants its parameters take.
`--filter TEXT` selects the tests whose full definition
path contains TEXT, and `--test-timeout SECONDS` bounds each test's execution. An
earlier failure does not stop later tests, and no matching test exits 2.
A failing row names the outcome (`returned an error, exit status N`, `failed`,
`stopped`, `did not start` or the terminating signal), then the test's stderr, and
the lines below it repeat the test's stdout.
Hirsel reuses passing results for capability-free native tests with matching code
and configuration, and each row says `cached` or `executed`.
Use `--rerun` to execute every selected test and refresh passing results, `-j N` to
run tests in parallel, and `--sim` / `--seed` / `--seeds` / `--property-corpus` for
deterministic simulation and property campaigns.

Additional CLI commands cover package workflows (`lock`, `fetch`, `pack`, `publish`,
`--locked`, `--frozen`; [Packages](spec/packages.md)), inspection and profiling
(`inspect`, `prof`, `primer`; [Profiling](spec/profiling.md)), resident builds
(`serve`, `--resident`), TypeScript SDK generation and RPC calls (`sdk ts`, `call`),
and platform hosting (`host`, `launcher`, `register`, `deploy`, `manifest show`;
[Launch](spec/launch.md), [CLI](spec/cli.md)).

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

