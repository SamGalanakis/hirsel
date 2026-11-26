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

Install a Linux x86_64 release with `curl -fsSL https://hirsel.ai/install.sh | sh`
(requires glibc 2.28+; Apple Silicon macOS support is in progress). Re-run the installer
to upgrade, or set `HIRSEL_VERSION` to pin a specific release tag.
An installation is a relocatable prefix containing `hirsel`, the published `core` and `std` images
(including prebuilt release standard-library objects), the toolchain packages, target runtime archives,
the bundled native link kit (`link/kit/`), `bin/ld.lld`, `libexec/hirsel/`, and the LLVM libraries
shipped with the compiler. Installed toolchains use the bundled link kit and do **not** require
host C development packages (`libc6-dev` / `libgcc-N-dev`). Host C development packages are only
needed when linking with a custom `--cc` driver outside the bundled link kit. If any link input is
missing, Hirsel names the first missing input before invoking the linker.

The toolchain packages are package archives in `core-packages/<name>-<major>.hpkg`: `store@1`
and `runner@1`, and the WebRTC stack, `webrtc_sfu@1` with `turn@1`, `dtls@1`, `srtp@1`,
`rtp@1`, `stun_ice@1`, `media_bits@1` and `rtp_jitter@1`; the media packages `sframe@1`
(end-to-end media encryption, [SFrame](spec/sframe.md)), `media_container@1` (Ogg, WebM and
MP4 framing) and `media_record@1` (the [room recorder](spec/webrtc.md#rtc-9-recording)); and
`platform@1`, `search@1`, `mcp@1` and `cli@1` (command-line parsing, help, errors and bash
completion, [its README](../core/programs/cli/README.md)). A program binds one by major alone,
for example `webrtc_sfu = { major = 1 }` in its `[dependencies]`. The toolchain supplies it with
every toolchain package it binds, so a program outside the repository builds against the WebRTC
SFU, TURN, SFrame, the recorder, the platform, search, MCP and cli from the installed prefix and
nothing else
([Packages](spec/packages.md), section 2). `buck2 build //:hirsel-toolchain` packs each one
from its directory under `core/programs/`, binding its dependencies by major.

`hirsel --version` prints the toolchain version, compiler identity and registered
runtime archive identities by target.
`hirsel env` reports the compiler, installed root, package cache, target and runtime a
request would use; `--json` prints the same as JSON and `--target` selects another
target's runtime.

### Compiler identity

The compiler identity is what the compiler is, not where it was built or linked: two
builds of one source tree with the same pins are one compiler on every host, so a
Linux-built cross compiler and a Mac-built native one emit the same bytes for the same
Mac program. The identity keys native objects, the image root and the product caches.

It is a SHA-256 the driver's build script computes over:

- the pins: `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml` (the Rust release),
  `tools/native-toolchain/llvm-23.lock` and `tools/native-toolchain/native.lock.json`
  (the LLVM tree and the link kit);
- every file of the driver's crate closure (its path dependencies, normal and build, on
  every target), except each crate's `tests`, `benches` and `examples`;
- hirsel-llvm's C++ bridge, which hirsel-llvm-lazy compiles;
- the bundled `core`, `std`, `store` and `runner` sources.

It leaves out:

- the build profile (opt level, debug assertions, overflow checks), host C++ compiler
  flags and the Buck or Cargo recipes. These preserve semantics: an assertion either
  holds or stops the compiler, so they change whether it finishes, never what it emits;
- Cargo features. Every first-party feature is a test-only or runtime option
  (`tools/buck2/production-features.json`), and the installed driver enables none;
- the release version, which reaches only `--version` and documentation links;
- the host's libm. The compile-time evaluator uses only `sqrt`, which IEEE 754 rounds
  exactly on every host.

The LLVM engine library and each target's runtime archive are keyed by their own
identities, beside this one. The engine library's key is the content of its file, which
differs by host, so what LLVM lowers (the release std, LLVM units and objects) is cached
per host. A linked program takes no byte of that key: a program's source bindings hash the
std's points, which are all the source note takes from it, not the std's key. A Mach-O's
debug rows name each function. An ELF's debug rows name a part by an alias symbol,
`__hirsel_debug_<key>_<section>`, whose key derives from content the engine file never
enters (what a part's bitcode records, or the instance an object is emitted for), so no
executable differs by the engine file. A resident compiler (`hirsel serve`) is still named by its
executable's link ID, because a client must talk to the same file it would run.

A Mach-O's `LC_UUID` is ld64.lld's digest of the linked image and the ad-hoc code
signature hashes its pages, so both follow from the bytes.

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
incomplete features and its resource refusals. When the compiler refuses code it prints a
short rule code; `hirsel explain RULE` prints the rule and a refused example, and
`hirsel explain --list` lists them all.

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

`main` receives its authority as parameters ([entry grants](spec/capabilities.md#cap-4-entry-grants)).
`--dir PATH` grants a confined directory, one per Dir parameter in order, and
`--grant KIND` grants `Clock`, `Random`, `Stdout`, `Stderr`, `Stdin`, `Process`, or
`Tasks`, as well as parameterized rules for `Net` (TCP/UDP/TLS/Resolve and Unix-domain
sockets, [Net sockets](spec/net.md)), `Env` ([Env](../core/std/api/env.md)), `Secret`,
`Cred`, `Foreign`, and `Exec` (run `hirsel help run` for exact syntax). A run missing grants does not start (status 103) and names every
ungranted parameter at once, one ``not started: grant `NAME` ...`` line each in parameter order;
an ungranted `Foreign` interface is refused before native code is emitted, one
`not granted: Foreign[D]` line per interface. Everything after `--` is passed to the program as `Args`.
A package whose `hirsel.toml` declares `limit`, `dirs` or `grants` in `[program]`
([packages](spec/packages.md)) builds and runs under them, and `--limit`, `--dir` and
`--grant` are refused; `Foreign` and `Exec` approvals still come from the command line.
A dependency's `[program]` is never applied. `hirsel build` warns once for each `main`
parameter the build leaves ungranted, in parameter order:
``hirsel: warning: grant `NAME` is not in the build's configuration; the executable will not start``.
`run` exits with the program's status. A `main` that returns `Error(e)` writes
`e` as Hirsel source after `error: `, with every `Secret` shown as `[secret]`; an
`Exit` writes nothing ([program entry](spec/programs.md#prog-3-entry)). For every nonzero
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
([recording is outside the program](spec/capabilities.md#cap-9-recording-is-outside-the-program)).

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

`hirsel test` discovers each `@test` function ([test functions](spec/programs.md#prog-4-tests))
in the package and runs each execution in its own process. A package whose
`hirsel.toml` declares its tests' run configuration runs under it, and
`--limit`, `--dir` and `--grant` are refused. Use the `limit`, `dirs` and
`grants` keys in `[tests]`. Otherwise `--limit BYTES` is
mandatory and is each test run's root limit; `--dir` and `--grant` configure
capabilities as for `run`. A test receives only the grants its parameters take.
`--filter TEXT` selects tests whose full definition path contains TEXT.
`--exact DEFINITION` matches a full path such as `app@1/main/smoke`. Repeat
`--exact` to select multiple paths. The two selectors are mutually exclusive.
`--test-timeout SECONDS` bounds each test's execution. An earlier failure does
not stop later tests, and no matching test exits 2.
A failing row names the outcome (`returned an error, exit status N`, `failed`,
`stopped`, `did not start` or the terminating signal), then the test's stderr, and
the lines below it repeat the test's stdout.
Hirsel reuses passing results for capability-free native tests with matching code
and configuration, and each row says `cached` or `executed`.
`--test-cache DIR` keeps them under DIR instead of the user's cache.
Use `--rerun` to execute every selected test and refresh passing results, `-j N` to
run tests in parallel, and `--sim` / `--seed` / `--seeds` / `--property-corpus` for
deterministic simulation and property campaigns.

`hirsel test app --laws` adds a strength column (held, truncated, vacuous, …) beside pass
and fail ([law table](laws.md)). `--write-laws` rewrites the rows
of the tests it ran in `hirsel.laws`; `--laws-baseline FILE`
compares a full run with a lock from main and exits 1 on a weaker
row. `hirsel laws check app` checks lock currency without running tests, and
`hirsel laws write app` refreshes its static columns.
`--locked` keeps its package meaning.

Additional CLI commands cover package workflows (`lock`, `fetch`, `pack`, `publish`,
`--locked`, `--frozen`; [Packages](spec/packages.md)), inspection and profiling
(`inspect`, `prof`, `primer`; [Profiling](spec/profiling.md)), resident builds
(`serve`, `--resident`), TypeScript SDK generation and RPC calls (`sdk ts`, `call`),
and platform hosting (`host`, `launcher`, `register`, `deploy`, `manifest show`;
[Launch](spec/launch.md), [CLI](spec/cli.md)).

## Compiler resource limits

When a program exceeds an implementation ceiling — parser or resolver nesting depth,
64-bit native layout range, or the per-build monomorphized instance limit (65,536 by
default) — `hirsel check` and `hirsel build` report a resource refusal ([program limits](spec/programs.md))
and exit **2** (rule refusals exit **1**). In `--json` mode these appear under the
`resources` array as `{primary, message}`. See
[internal compiler limits](internal/compiler-limits.md) for parser guard and stack-budget
details.


