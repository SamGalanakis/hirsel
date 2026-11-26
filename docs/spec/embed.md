# Embedding and Apple mobile targets

*Specified 2026-11-26 (lane ios-spec-3137, from the PR #11 review and the iOS prospect).
Amended the same day after soundness review 3154 (lane ios-spec-fix-3158) and re-review 3167
(lane ios-spec-fix-2-3169). Rules EMB-1 to EMB-9. Not built: every rule here is a slice in
the iOS track, and iOS stays in [reserved](reserved.md) until its device gate passes
(EMB-9).*

This specification adds three general things and one Apple-specific thing:

- a **static-library output kind**, which any native target can build (EMB-1);
- an **embedded launch** and a versioned **C embed ABI**, through which a host process starts
  a Hirsel program, calls its exported entries, cancels, and stops it (EMB-3, EMB-4);
- **generated Swift bindings** and a static **XCFramework**, built by the toolchain from the
  same manifest the TypeScript SDK uses (EMB-5);
- the **Apple target records**, one per platform variant, from which every Apple floor,
  triple and linker flag is derived (EMB-2), and the two iOS targets
  `aarch64-apple-ios` and `aarch64-apple-ios-sim` built on them (EMB-6, EMB-7).

Nothing here is iOS-only machinery. The static library, the embed ABI and the Swift bindings
work for every native target. iOS is the first target that can only be built this way.

**The host survives Hirsel.** An embedded runtime runs inside an application that Hirsel
does not own. A Hirsel fault, run stop or refusal ends the runtime, reports to the host
and leaves the host process running (EMB-3). EMB-3 lists the few cases that still end the
process, with the reason each cannot be contained.

## EMB-1: Output kinds

A native build has an **output kind**: `Executable` (today's only kind) or `StaticLibrary`.
The CLI spells it `--emit exe` <!-- doc-ref: planned --> or
`--emit staticlib` <!-- doc-ref: planned -->; the default is `exe`.

```text
OutputKind = Executable | StaticLibrary
```

**Which commands take which kind.** Request validation checks the command against the
kind first, and then the target against the kind, before installation lookup, front work
or any output is created. It refuses with the diagnostic in EMB-8.

| Command | Kinds it admits |
|---|---|
| `hirsel build` | both, as the target's row below allows |
| `hirsel run`, `hirsel test`, `hirsel sdk ts`, `hirsel sdk json`, and every other command that runs the image it builds (Describe included) | `Executable` only: these commands execute their output, so `--emit staticlib` <!-- doc-ref: planned --> is refused before any lookup |
| `hirsel sdk swift` <!-- doc-ref: planned --> | builds static libraries itself (EMB-5) and refuses an explicit `--emit` <!-- doc-ref: planned --> |

**What a static library holds.** A static library contains:

- the program's objects;
- the runtime archive of the target's **embed slot** (EMB-3);
- its manifest (EMB-5) as read-only data behind `hirsel_embed_v1_manifest`;
- the members of the foreign archives the program reaches.

They are packed into one archive, with one member per object, in the order the link would
take them. The archive has no `main` and no process entry. Every global symbol it defines
is an embed ABI export (EMB-4), the library's anchor (below), or a name carrying the build
prefix below. The archive is not linked: there is no final link, and the link kit's only
role for an archive is the import audit below.

**Symbol hygiene.** A host process may link other Rust or C code. So every global symbol
that a member defines, other than an EMB-4 export or the anchor, is renamed in every member
that defines or references it. The new name is `_hirsel_<b>_<name>` (Mach-O) or
`hirsel_<b>_<name>` (ELF), where `<b>` is the first 16 hexadecimal digits of the build key.
A symbol that no member defines keeps its name: it is an import from the C library or a
framework. The renaming is one `llvm-objcopy --redefine-syms` <!-- doc-ref: external -->
pass per member, over one map computed from all members' symbol tables, so a reference and
its definition always get the same new name. This was checked with the pinned llvm-objcopy
23.1.5 on an `arm64-apple-ios26.0` object: the definition and its references are renamed,
and `LC_BUILD_VERSION` is kept.

**The anchor.** Each library defines one product symbol, `hirsel_embed_v1_anchor_<b>`, a
one-byte constant. It is defined in the same archive member as every `hirsel_embed_v1_*`
export, and nothing else is defined in that member. `hirsel_product.h`
<!-- doc-ref: planned --> declares `extern const uint8_t hirsel_embed_v1_anchor_<b>;` and
an inline helper `hirsel_embed_v1_require_anchor_<b>()` that reads it through a `volatile`
pointer (`*(const volatile uint8_t *)&hirsel_embed_v1_anchor_<b>`). Every generated
consumer calls that helper on its start path: the Swift `Runtime` in EMB-5, or a C host
using `hirsel_product.h` <!-- doc-ref: planned -->. Because the read is `volatile`,
compiler optimization (`-O`, whole-module optimization and LTO) cannot dead-strip the
reference.

The reference forces a lazy archive link to extract that member. Two Hirsel libraries
linked into one host therefore both extract a member defining the same `hirsel_embed_v1_*`
names. The host's link then fails with a duplicate symbol, in either archive order, even
under optimization and even when the two programs have equal binding projections. Without
the observable anchor reference, the linker could resolve the common names from the first
archive alone, extract nothing from the second, and let the second program's bindings start
the first program. Internal symbols never collide, because they carry the build prefix.

**The archiver.** An archive is written by the installation's own `bin/llvm-ar` with
`rcsD`: deterministic, with zero timestamps, owners and modes. It uses the target's format:
`--format=darwin` <!-- doc-ref: external --> for Mach-O targets and
`--format=gnu` <!-- doc-ref: external --> for ELF targets, as the foreign recipe already
does ([foreign](foreign.md)). No host archiver is consulted, and `/usr/bin/libtool` is
never used.

The installation ships `bin/llvm-ar` and `bin/llvm-objcopy` beside `bin/ld.lld`. The
toolchain manifest records the SHA-256 of each, as it records `ld.lld`'s. So a changed
archiver changes the toolchain identity, and with it every key below.

**The import audit.** A static library is not linked, so a reference to a name that the
target's link kit does not export would only fail later, in the host's link. The driver
fails it first. Every symbol left undefined across the archive's members, after the
renaming, must be exported by one of the target's link-kit stubs (EMB-6). A name that no
stub exports refuses the build with `UnlistedImport(name, member)`. This is the list an
executable's link enforces, applied without a link.

**Cache keys.** The output kind is a field of every key that names a native product, so an
archive and an executable of the same program never answer for each other:

| Key (domain string) | Where | Change |
|---|---|---|
| `hirsel.core-build.v1` | the link build key (`crates/hirsel-core-driver/src/native_link.rs`, `build_key`) | becomes `.v2`, with a field `output: OutputKind` after `link_mode`; `hirsel.core-executable.v10`, keyed over it, becomes `.v11` |
| `hirsel.recipe-final-link.v4` | the recipe final link (`crates/hirsel-core-driver/src/native.rs`, `link_objects`) | becomes `.v5`, keyed over `(assembly, output)` instead of `assembly` |
| `hirsel.incremental-link-frame.v2` | the resident compiler's link frame, from which it patches its last link (`crates/hirsel-core-driver/src/native.rs`, `crate::incremental_link`) | becomes `.v3`, with a field `output` after `target`. A static-library build never patches: it rewrites the archive from its members. The field also guarantees that a frame of one kind is never read for the other |

Some keys do not change:

- The resident parse and symbol memos (`hirsel.resident-parse.v1`,
  `hirsel.resident-symbols.v1`) are front-end products that do not depend on the output
  kind, and keep their keys.
- The launcher's `hirsel.assembly-record.v1` frame is not a product key. The launcher runs
  only executables, and an archive never enters it.

Old keys are not migrated: a build with the new keys misses and rebuilds. The kind is a full
field, never a suffix on a path or a flag folded into `link_mode`. PR #11's recipe cache
returned an executable for an archive request because the kind was not in the key.

**The target and kind table.** Which kinds a target builds is one exhaustive table in the
layout crate (`Target::admits(self, OutputKind) -> Result<(), Refusal>`), consulted at
request validation (EMB-8):

| Target | `Executable` | `StaticLibrary` |
|---|---|---|
| `x86_64-unknown-linux-gnu` | yes | yes |
| `aarch64-unknown-linux-gnu` | yes | yes |
| `aarch64-apple-darwin` | yes | yes |
| `aarch64-apple-ios` | refused: an iOS app is an Xcode-signed bundle, which Hirsel does not produce | yes |
| `aarch64-apple-ios-sim` | refused, as above | yes |

## EMB-2: Apple target records

Every Apple fact that depends on the platform variant or its floor comes from one record per
variant, in one file: `tools/native-toolchain/apple-targets.json` <!-- doc-ref: planned -->.
Nothing else in the repository spells an Apple floor, a platform number or an Apple LLVM
triple.

| Field | `macos` | `ios` | `ios-simulator` |
|---|---|---|---|
| `triple` (Hirsel, the only accepted spelling) | `aarch64-apple-darwin` | `aarch64-apple-ios` | `aarch64-apple-ios-sim` |
| `floor` | `26.0` | `26.0` | `26.0` |
| `macho_platform` (`LC_BUILD_VERSION`) | 1 (`PLATFORM_MACOS`) | 2 (`PLATFORM_IOS`) | 7 (`PLATFORM_IOSSIMULATOR`) |
| `platform_version_name` (linker) | `macos` | `ios` | `ios-simulator` |
| `llvm_os` | `macos` | `ios` | `ios` with environment `simulator` |
| `deployment_env` | `MACOSX_DEPLOYMENT_TARGET` | `IPHONEOS_DEPLOYMENT_TARGET` | `IPHONEOS_DEPLOYMENT_TARGET` |
| `swiftpm_platform` | `macOS` | `iOS` | `iOS` |
| `xcframework_slice` | `macos-arm64` | `ios-arm64` | `ios-arm64-simulator` |

The file also holds one table, `cpu_by_floor`. For each platform family (`macos`, `ios`)
and floor, it gives the oldest CPU that the floor admits:

| Family | Floor | CPU | Why |
|---|---|---|---|
| `macos` | `26.0` | `apple-m1` | macOS 26 runs on Apple silicon from M1 |
| `ios` | `26.0` | `apple-a12` | the iOS triples cover iPadOS, and iPadOS 26 is believed to run on A12 iPads. This is not confirmed against Apple's device list. A12 is the safe subset, since it lacks only DotProd relative to A13 |

**Derived values.** Each of these is computed from the record and nowhere written by hand:

- the LLVM and Cranelift triple: `arm64-apple-<llvm_os><floor>`, plus `-simulator` for the
  simulator (`arm64-apple-ios26.0-simulator`);
- the linker flag `-platform_version <platform_version_name> <floor> <floor>`
  <!-- doc-ref: external --> and the C compiler's `--target`;
- the `LC_BUILD_VERSION` `minos` that Hirsel's own objects carry, and the FFI-3 admission
  floor for foreign archive members (EMB-8);
- `Target::cpu()` and the Cranelift and LLVM feature sets, from `cpu_by_floor`;
- the SwiftPM `platforms:` entry and the XCFramework `Info.plist` rows (EMB-5);
- the deployment environment variable that the foreign C recipe sets.

**How the copies are generated.** `python3 tools/buck2/sync.py` reads the record and writes
two generated copies:

- the Rust table that the layout crate includes,
  `crates/hirsel-core-layout/src/apple_records.rs` <!-- doc-ref: planned -->;
- the Starlark constants that the toolchain rules load,
  `tools/buck2/toolchains/apple.bzl` <!-- doc-ref: planned -->.

Shell recipes take the values as arguments from those rules. The `--check`
<!-- doc-ref: external --> mode rejects a stale generated file, as it does for `pins.bzl`.
This replaces today's `DARWIN_FLOOR` copies and the `tools/check_target.py` rule that ties
them together. `sync.py` refuses a record whose `(family, floor)` has no `cpu_by_floor`
row, so a floor change can never leave the CPU baseline behind.

**Changing a floor.** For a floor that already has a `cpu_by_floor` row, the change is one
edit of `floor` and a regeneration. A new floor needs its row as well. The oldest admitted
CPU is a fact about Apple's device support, not about the version number, and a lower floor
can admit an older CPU with fewer features.

**Why 26.0.** The macOS floor is 26.0 because macOS 26 is the first release whose kernel
enforces `O_RESOLVE_BENEATH` (macOS decision D2). The iOS floor is 26.0 for the same reason,
for device and simulator alike. iOS runs the same XNU kernel, which has
`O_RESOLVE_BENEATH` and `ENOTCAPABLE`. So `Dir` confinement on iOS uses the macOS code path
and is enforced by the kernel; there is no userspace path walk. The device gate's first law
(EMB-9) checks that this holds on hardware. Its signed run qualifies this fixed floor; it
does not choose a floor.

## EMB-3: Embedded launch

An **embedded launch** runs a runner program inside a host process that the program does not
own. It makes concrete the [launch](launch.md) rule "an embedded runtime receives its launch
identity from its embedder before entry".

**The embed slot.** Each target's toolchain carries a second runtime archive, the **embed
slot**. It is built from the same runtime crates without the `standalone` feature. Without
`standalone`, the runtime does not own the process:

- It uses the host's allocator (the C library's `malloc`), never its own global allocator.
- It installs one process-wide signal action, for `SIGSEGV` (and `SIGBUS` on Darwin), and
  nothing else process-wide ("Faults and signal stacks" below).
- It never calls `exit` or `_exit`. Every startup refusal (including baseline-safe x86-64
  ISA detection, which returns `UnsupportedCpu` instead of calling `std::process::exit`),
  every run stop (including refused stack growth), a place's `Fail`, a stop by the host and
  spontaneous teardown end the runtime and are reported to the host (EMB-4). The process
  goes on. No embed-slot code path calls `exit` or `_exit`. The archive may still import
  them through Rust std members that Hirsel never calls, so the law is behavioural
  (slice 2s, covering startup refusals, run stops and spontaneous teardown), not a symbol
  listing.
- It never changes the process-wide `SIGPIPE` disposition. Socket sends suppress `SIGPIPE`
  per socket (`SO_NOSIGPIPE` on Darwin, `MSG_NOSIGNAL` on Linux). Pipe writes for `Exec`
  stdin (admitted on Linux and macOS) suppress `SIGPIPE` without altering the host's
  `SIGPIPE` disposition or any host thread's signal mask: on macOS the pipe write end sets
  `F_SETNOSIGPIPE`; on Linux the calling runtime worker blocks `SIGPIPE` around `writev`,
  drains a thread-pending `SIGPIPE` with zero-timeout `sigtimedwait` when `writev` fails
  with `EPIPE`, and restores its thread mask. Writing to a closed child pipe therefore
  delivers `Reset` ([exec](exec.md)) while the host process continues even under `SIG_DFL`.
- It reads no environment variable, argument vector or inherited descriptor — neither for
  grants (`Dir`, `Secret` or any other capability), nor for ISA detection, nor during
  startup.
- Its Rust code is built with `panic = "abort"`, so no unwind ever crosses the C boundary.

**What still ends the host process.** Four cases end the process. Each is stated here with
the reason it cannot be contained, and no other Hirsel outcome may end the host:

1. A Rust panic in the runtime. This is a runtime defect. With `panic = "abort"` and no
   unwinding allowed across the C boundary, there is no frame to recover in.
2. A refused allocation of the runtime's own Rust bookkeeping memory. Rust's
   allocation-failure handler cannot return, and nothing may unwind. Program payload memory
   and stack mappings are allocated fallibly instead, and their refusal is `OutOfBytes` or
   the contained run stop `HostMemory` (below).
3. A crash, abort, stack overflow or refused stack growth in audited native foreign code.
   This is termination by the host
   ([FFI-1](foreign.md#ffi-1-hirsel-trusts-audited-native-code-and-does-not-contain-it)).
   Synchronous foreign thunks may run on a guarded stack (`Stack::call`,
   [FFI-7](foreign.md#ffi-7-host-stack-and-waits)), which the runtime marks as
   **non-recoverable native execution** at the fault boundary. A guard hit or refused
   growth on that stack is never abandoned or recovered; it is forwarded to the host's
   previous fault action. Containing native code would need a process boundary, which iOS
   does not offer.
4. A fault on a host thread, or anywhere outside a guarded Hirsel stack. It is passed to the
   host's previous action, and that action decides.

**Faults and signal stacks.** Fiber stacks grow by a fault handler: a fault below a stack's
writable part makes the next pages writable. So the handler is required, not optional, in
every slot. The embed slot sets it up as follows.

- **One installation per process.** The first `start` in the process installs the handler,
  on its calling thread, while the registry is in `Starting` (EMB-4), so no two starts
  install concurrently.

  For each signal, the runtime first reads the current action and publishes it, with a
  release store, into a static **previous-action slot**. Then it installs its own action
  with `SA_SIGINFO | SA_ONSTACK`. If the action that `sigaction` reports as replaced
  differs from the one published, the runtime publishes the replaced action instead.

  The handler reads the slot with an acquire load, so it never reads an unpublished slot.
  The implementation today installs before it records (`PREVIOUS` in
  `crates/hirsel-core-rt/src/fiber.rs`), so a fault in that window ends the process as
  `Signal`. Slice 2s removes that window.

  If a host changes these actions concurrently with the first `start`, the chain order is
  undefined, as with any two libraries installing at once.
- **Never removed, never reinstalled.** The handler stays installed after `stop`, because
  another library may have chained after it. After `stop` no Hirsel stack is guarded, so
  every fault passes through.

  A later `start` checks that the installed action is still Hirsel's. If it is not, that
  `start` refuses with `FaultHandlerReplaced` and installs nothing. Reinstalling would make
  Hirsel chain to a handler that may itself chain back to Hirsel. A host that installs its
  own handler after Hirsel must chain to the action it replaced, as every chaining library
  does.
- **Unrelated faults keep the host's disposition and flags.** A signal that is not on a
  recoverable guarded Hirsel stack (including any fault on a host thread, any fault on a
  non-recoverable native foreign stack, and any software-raised signal) is forwarded to the
  previous action according to whether it was a synchronous hardware fault (such as
  `SEGV_MAPERR`, `SEGV_ACCERR`, `BUS_ADRERR` or `BUS_OBJERR`) or a software-raised signal
  (`kill`, `raise`, `pthread_kill`, `sigqueue`), preserving the previous action's flags:
  - **Previous action is `SIG_IGN`.** For a software-raised signal, the handler returns at
    once and leaves Hirsel's handler installed, so the process continues and fiber stack
    growth keeps working. Ignoring a synchronous hardware fault is undefined in POSIX and
    is treated as `SIG_DFL` below.
  - **Previous action is `SIG_DFL`.** For a synchronous hardware fault, the handler
    restores `SIG_DFL` and returns so the faulting instruction repeats under `SIG_DFL`. For
    a software-raised signal (which has no faulting instruction to repeat), the handler
    restores `SIG_DFL`, unblocks the signal, and re-raises it on the current thread so the
    process terminates with that signal instead of dropping it and disarming Hirsel.
  - **Previous action is a custom handler.** Before calling it, the handler blocks
    `previous.sa_mask` (plus the delivered signal itself unless `previous.sa_flags` sets
    `SA_NODEFER`) with `pthread_sigmask`, and restores the prior thread mask on return. If
    `previous.sa_flags` sets `SA_RESETHAND`, the handler atomically replaces the saved
    previous-action slot with `SIG_DFL` before invoking it (one-shot semantics while
    keeping Hirsel's handler installed). It then invokes the previous handler with that
    handler's own signature (`SA_SIGINFO` three-argument or single-argument). Crash
    reporters installed by the host therefore see host faults with their expected mask and
    flags.
- **Signal stacks are per runtime worker.** Each runtime worker thread installs its own
  alternate signal stack when it starts, and restores the previous one before it exits.
  The handler needs an alternate stack because a guard fault leaves the faulting stack
  unusable.

  No ABI function installs, changes or reads a host thread's signal stack. On a host
  thread, the handler runs only for a fault that is not on a guarded Hirsel stack, and it
  chains at once.

  Today `fiber::arm` installs an alternate stack on whichever thread calls it (`ALTERNATE`
  in `fiber.rs`). In the embed slot, only worker start calls it.
- **No Mach exception port** is installed. Whether the chained handler is sound under a
  debugger that takes Mach exceptions first is a device-gate question (EMB-9).

**Run stops end the runtime, not the process.** In the embed slot, every
[FAIL-15](failure.md#fail-15-run-stops) run stop ends the **runtime generation** (EMB-4).
Generated code, the runtime and the providers raise a stop the same way as standalone. What
differs is how it ends:

1. **The first stop wins.** The worker that raises a stop records the stop's name as the
   generation's end, unless an end is already recorded.
2. **Its task is abandoned.** It abandons the task it was running, without unwinding,
   cleanup, finalizers or `defer`, as FAIL-15 requires. It does so at a point where that
   task holds no lock (next item).
3. **Workers leave.** It returns to its worker's scheduler, which runs no further task of
   this generation. Every other worker observes the recorded end at its next scheduling
   point (a task switch, a park or a provider completion) — and in pure Hirsel code with no
   `bound steps`, through the embed slot's cooperative step check below — and leaves the
   same way. In the embed slot, every task without an explicit step bound initializes its
   `steps` countdown (`Inst::Step`, [MEM-30](budgets.md#mem-30-step-bounds)) to 65,536
   instead of `2^62`; when the cold step handler fires with no step bound current, it loads
   the generation's atomic end flag. If no end is recorded, it reloads 65,536 and returns
   without emitting a scheduling event or ending a slice (preserving CONC-10 and MEM-30);
   if an end is recorded, it returns a stop to the worker's scheduler immediately. A
   provider call blocked in the host finishes, and its result is discarded. Every provider
   wait is bounded by its deadline or by the runtime's cancellation of host waits at a
   generation end, as at a standalone run's end (and `SecTrust` runs on an independently
   owned job, EMB-7, so teardown never blocks on it). Audited native foreign code is the
   one liveness boundary: an issued foreign `waits` call or an unbounded synchronous
   foreign call runs audited native code (FFI-1, FFI-7), Hirsel cannot preempt it, and
   teardown, `ended` and `stop` wait until it returns.
4. **Teardown.** Once every worker has left Hirsel code **and** every already-admitted host
   ABI call of this generation has finished its last access (the admission barrier in
   EMB-4), the runtime releases every host resource the generation holds. These are its
   stacks (every stack of the ended generation is unmapped whole with `munmap`, never
   retained in a process-static stock), descriptors, handle tables, store and memory, as
   for a stopped run ([foreign](foreign.md#ffi-6-handles): "freed when the run ends, an
   embedded run's included"). This teardown is the runtime's, not Hirsel cleanup.
5. **The host is told.** The host gets each session's `session_ended`, then `ended` with
   code `HIRSEL_END_RUN_STOP` and the stop's name as the reason (EMB-4). Nothing is written
   to stderr.

How a stop leaves depends on where it is raised:

- **In ordinary code.** A stop raised in ordinary runtime or provider code is a value
  returned to the worker's scheduling point. Examples are `HostMemory` when the C allocator
  or a stack pre-commit refuses memory, a counter overflow, and `Deadlock`. Every lock
  guard on the way is released as its owner returns. In the embed slot, no stop site
  diverges through `crate::process::stop`. Slice 2s turns each of those sites into a
  returned stop.
- **On the guard page (recoverable Hirsel stack).** The per-worker fault descriptor
  distinguishes **recoverable** Hirsel fiber stacks from **non-recoverable native** foreign
  stacks (`Stack::call`). A fault on a recoverable stack's hard guard page is stack
  exhaustion, the stop `Stack`. The handler cannot return into the faulting frame. It
  records `Stack` through an async-signal-safe atomic handoff (first stop wins, no lock, no
  allocation, no stdio) and rewrites the interrupted context so that the thread resumes at
  the worker's stop entry, on the worker's own stack. This abandons the fiber stack, as a
  fiber switch would. A fault on a non-recoverable native foreign stack's guard page never
  rewrites the context: it chains to the host's previous action (termination by the host,
  item 3 above). Standalone, a recoverable guard fault exits through `process::stop`
  (`fiber.rs`, the `fault` handler).
- **On refused stack growth.** A fault below a recoverable stack's committed low-water mark
  (`guard_end <= addr < low_word`) grows the stack with `mprotect`. If `mprotect` fails
  inside the signal handler, the runtime fails closed:
  - It never returns to the faulting instruction (which would fault again) and never
    advances `low_word`.
  - Because the guard rule below forbids any locked or libc/allocator frame from faulting
    on growth, abandoning the faulting Hirsel frame is sound. The handler records
    `HostMemory` through the same async-signal-safe atomic handoff, marks the faulting
    stack **retired** via an async-signal-safe flag, and rewrites the interrupted context
    to resume at the worker's stop entry on the worker's own stack.
  - The unit and its generation abort with `HIRSEL_END_RUN_STOP` and reason `"HostMemory"`
    (named in EMB-8), leaving the host app alive.
  - **Region retirement.** A stack whose `mprotect` failed (in the handler or in
    `fiber::room` below) is retired: it is never trimmed, never re-`mmap`ped, and never
    returned to `Stock` or `units::Memory`. At generation teardown, once all workers have
    left Hirsel code, its entire `[base, base + len)` reservation is unmapped whole with
    `munmap`. No half-mapped region is ever reused by another fiber or a later generation.
  - If `mprotect` fails on a **non-recoverable native** foreign stack, the handler never
    abandons native frames: it forwards to the host's previous fault action (item 3 above).

**The guard rule.** Abandoning a frame at a guard fault or refused-growth fault is sound
only if that frame holds no lock and is not inside the C library or the allocator. So only
two kinds of code may run into the guard page or an uncommitted growth page:

- generated Hirsel code;
- runtime code that holds no lock and calls no C library or allocator function.

Every runtime path on a fiber stack that calls the C library or the allocator, or takes a
lock, does one of two things *before* taking the lock or entering libc or the allocator:

- ensures `bound` bytes of **already-writable** stack below the current stack pointer via
  `fiber::room(bound)` (checking distance to the committed low-water mark `*low_word(top)`,
  not merely distance to the hard guard page). If `bound` does not fit above the hard guard
  page, `fiber::room` raises `Stack` as an ordinary returned stop. If `bound` fits above
  the guard page but crosses `low_word`, `fiber::room` pre-commits the needed pages with
  `mprotect` in ordinary code *before* any lock is taken; if that `mprotect` fails, it
  marks the stack retired and raises `HostMemory` as an ordinary returned stop. Once the
  check succeeds, the protected path cannot fault on either the hard guard or growth.
- switches to the worker's host stack (which is already writable for the protected path and
  never takes a recoverable growth fault inside a lock or libc).

This rule is proven by tests (slice 2s), not guarded at run time: no product mechanism
guards against the runtime breaking its own rule.

**What runs.** The embedded program is a runner program: it declares
`runner = { major = 1 }`. It is launched as one place with
`PlaceConfig.listen = InProcess(dials)` (RUNNER-SPEC §14.11), so it binds no socket. The
runtime keeps the place's `Dialer` and dials one connection per EMB-4 session. The place's
reservation counts each session's dial room, as `local`'s does (§14.11 "Transport").

**Launch inputs.** The host supplies the inputs below in `hirsel_embed_config` (EMB-4). The
target is not among them.

| Input | Meaning |
|---|---|
| `digest` | 32 bytes, or empty. The generated bindings pass the SHA-256 of the static library that the toolchain wrote (EMB-5). It is diagnostic only, as for every launch: the release identity is `revision`, `n_place` and the exports, never the digest. An empty digest is `Unavailable("embedded, not supplied")`. The runtime never hashes `current_exe` or the host's image. |
| `n_place` | The place size, as for every launch. Registration runs under it at `start`: a place that does not fit refuses `start` with `PlaceCapacity(Room)`, with [launch](launch.md#describe-launch)'s registration semantics. |
| `grants` | The grant text, in the grammar `hirsel run` takes, subject to the embedded grant-source restrictions below (no environment, working directory or inherited descriptor for any grant). A grant the target cannot honour was refused at build (EMB-7, EMB-8). A grant outside the program's declared wants is refused at `start`. |
| `dir_bases` | The bases of the launcher-resolved directory forms (below). |
| `store_dir` | An absolute path to the place's store directory. The host creates it, and it must persist across launches (on iOS, under Application Support, never under Caches). The runtime opens it once, as the launcher does. |
| `client_bytes` | The bound on the runtime's host-client storage (EMB-4 "Host-client storage"). Must be at least the read/control progress floor, or `start` refuses with `ClientBytesTooSmall`. |
| `start_timeout_ms` | Positive millisecond bound on `start` reaching Ready (`0` is `HIRSEL_ABI_MISMATCH`). If registration does not reach Ready before expiry, `start` signals generation end (interrupting provider waits and pure Hirsel loops via the embed step check), joins workers, releases resources and refuses with `PlaceTimedOut` (subject only to the native foreign-code liveness boundary above). |

The target is the immutable target record of the slice the library was compiled for. The
runtime passes it to the launch internally, and `Welcome.target` reports it. It is never an
embedder value, so a binding cannot claim a target its library was not built for
([launch](launch.md) is amended to match).

The runtime copies every input before `start` returns. Later changes to the host's memory
cannot change the launch.

**Directory grants.** An embedded launch resolves every `Dir` grant before entry. Each
resolved root is a directory opened with `O_DIRECTORY` and pinned as a descriptor before the
place serves its first session. Every program access is relative to that descriptor, as for
any `Dir` grant. The grant sources resolve as follows:

- **An absolute path.** It is accepted, and resolved with the symlink and creation rules of
  [launch](launch.md#launcher-resolved-directories).
- **A launcher-resolved form** (`xdg-config:<app>`, `xdg-cache:<app>`, `xdg-data:<app>`,
  `xdg-state:<app>`). It resolves against `dir_bases`, never an environment variable.
  `dir_bases` is UTF-8 text of lines `config=<abs>`, `cache=<abs>`, `data=<abs>` and
  `state=<abs>`, each at most once, each path absolute. A form's directory is
  `<base>/<app>`, with launch.md's name, symlink and creation rules. A form whose base is
  not given refuses `start` with `DirBase(form)`.
- **A relative path, an `env:` source or a descriptor source.** Each refuses `start` with
  `DirSource(grant)`. An embedded runtime has no working directory, environment or
  inherited descriptor to resolve them against.

Two more checks apply:

- **Store overlap.** `store_dir` must not equal, contain or lie inside a granted root.
  Overlap is derived from the directories actually opened and pinned (comparing the opened
  `store_dir` descriptor and each opened `Dir` root descriptor after symlink resolution by
  canonical `(st_dev, st_ino)` parent-chain walk to the filesystem root), never from raw
  input path spellings. If `store_dir` and a granted root resolve to the same directory or
  either lies on the ancestor chain of the other, `start` refuses with
  `StoreOverlapsDir(grant)`, so that a program cannot write its own store's files even
  through a symlink alias.
- **Unavailable directory.** A root that cannot be opened refuses `start` with
  `DirUnavailable(path, errno)`. On iOS this includes a data-protection class that is
  locked while the device is locked.

**Secret and other grant sources.** The no-environment, no-cwd and no-inherited-descriptor
rule applies to every grant form, not only `Dir`:

- A `Secret` grant ([TLS-5](tls.md#tls-5-secrets)) admits `file:<abs>` with an absolute
  path. An `env:<VAR>` source or a relative `file:` path refuses `start` with
  `SecretSource(grant)` before entry, without reading any environment variable.
- Any other grant form that names an environment variable, relative path or inherited
  descriptor refuses `start` before entry the same way.

The generated Swift `Runtime.start` resolves the container's directories again at every
start, because an app's container path can change across launches and updates. It passes:

- `data`, `config` and `state` under Application Support;
- `cache` under Caches;
- the store under Application Support, in its own directory.

A program written for the CLI's `xdg-data:<app>` grant therefore runs unchanged, with its
data persistent and its cache disposable.

## EMB-4: The C embed ABI

**The export surface already exists.** Hirsel's export surface is the runner's entries
(RUNNER-SPEC §0 rule 2): compute, transact, query, granted and room entries. Their
arguments, results and errors are `codec` values. The Describe launch and the SDK
[schema manifest](sdk.md#schema-manifest) list them with their codec ASTs.

The embed ABI adds no export declaration, no new type rule and no second way to cross a
boundary. It is a host binding for the entries that a place already exports, as the
TypeScript SDK is a WebSocket binding for them.

**Shape.** The runtime contains the client side of the runner protocol: the work that
`client.ts` does over WebSocket, which is streams, positions, call queueing, subscription
images and rooms. So the C ABI is entry-level, not frame-level. A host sends codec bytes for
an entry, and receives outcome bytes, subscription changes and room events through
callbacks.

A generated binding (Swift in EMB-5, any later language the same way) is a codec and a set
of typed stubs over these calls, with no protocol logic of its own.

The runtime's client side reuses the TypeScript client kernel delivered by lane 3145
(`sdk/client/state.ts`, `sdk/client/lanes.ts`); this ABI specifies no second multiplexer.
Because a store stream passes a position only after its reply is stored (STORE-SPEC §1.8
S7), the kernel multiplexes concurrent calls through one host session as **lanes**, each
in-flight call holding its own leased store stream, up to the session's `1 + q` owed units
from `Welcome`. Calls beyond `1 + q` wait in memory in submission order. Each call keeps
its own deadline (`deadline_ms`) and cancellation (`abandon`); in the shared kernel, a
reconnect replays every lane's pending call once under its unchanged idempotency key and
drops old-connection answers. Both `client.ts` and the runtime's client run one set of
executable transition vectors (input frame or host action in; effects and state out), and
the runtime's in-process effect adapter replaces the WebSocket one.

The toolchain writes the header, `hirsel_embed.h`. It is the same for every program and
target at one ABI version:

```text
#define HIRSEL_EMBED_ABI 1

typedef struct { uint64_t generation; } hirsel_runtime;                    /* 0: none */
typedef struct { uint64_t generation; uint64_t session; } hirsel_session; /* 0: none */
typedef struct { const uint8_t *ptr; size_t len; } hirsel_bytes;           /* ptr may be NULL only if len is 0 */

typedef enum {
  HIRSEL_OK = 0,
  HIRSEL_ABI_MISMATCH = 1,    /* the arguments break ABI v1: abi, fingerprint, size, tail or a NULL required field */
  HIRSEL_REFUSED = 2,         /* start refused; config.ended has run once with the reason */
  HIRSEL_STOPPED = 3,         /* the handle's runtime or session is not live */
  HIRSEL_UNKNOWN_ENTRY = 4,   /* want out of range, or the wrong kind for this function */
  HIRSEL_BUSY = 5,            /* stop called on the delivery thread */
  HIRSEL_ALREADY_RUNNING = 6, /* the runtime is starting, running, ended or stopping */
  HIRSEL_ID_IN_USE = 7,       /* the call, sub or room id is live in this session */
  HIRSEL_DEFERRED = 8,        /* close called on the delivery thread; session_ended follows */
  HIRSEL_NOT_LIVE = 9,        /* the id names no live call, sub or room */
  HIRSEL_FULL = 10,           /* host-client storage cannot take this request; nothing was sent */
  HIRSEL_HOST_MEMORY = 11,    /* the C allocator refused the copy; nothing was sent */
  HIRSEL_OVER_BOUND = 12      /* a room state or message exceeds the room's registered bound */
} hirsel_status;

typedef enum {
  HIRSEL_END_STOPPED = 0,     /* the host called stop */
  HIRSEL_END_REFUSED = 1,     /* start refused; reason is the refusal */
  HIRSEL_END_PLACE_FAILED = 2,/* the place ended with Fail; reason is its failure text */
  HIRSEL_END_RUN_STOP = 3     /* a FAIL-15 run stop; reason is the stop's name */
} hirsel_end_code;

typedef struct {
  uint32_t size;              /* sizeof(hirsel_embed_callbacks) the caller was built with */
  void *context;              /* passed back unread; retained until session_ended returns */
  void (*outcome)(void *context, uint64_t call, hirsel_bytes outcome);
  void (*change)(void *context, uint64_t sub, hirsel_bytes change);
  void (*sub_ended)(void *context, uint64_t sub, hirsel_bytes end);
  void (*room_event)(void *context, uint64_t room, hirsel_bytes event);
  void (*session_ended)(void *context, hirsel_bytes end);
} hirsel_embed_callbacks;

typedef struct {
  uint32_t size;              /* sizeof(hirsel_embed_config) the caller was built with */
  uint32_t abi;               /* HIRSEL_EMBED_ABI the caller was generated for */
  hirsel_bytes manifest_fp;   /* SHA-256 of the binding projection (EMB-5) */
  hirsel_bytes digest;        /* 32 bytes or empty (EMB-3) */
  uint64_t n_place;
  uint64_t client_bytes;      /* host-client storage bound */
  uint32_t start_timeout_ms;  /* positive ms bound on start reaching Ready; 0 is HIRSEL_ABI_MISMATCH */
  hirsel_bytes grants;        /* UTF-8 grant text */
  hirsel_bytes dir_bases;     /* UTF-8 base lines (EMB-3) */
  hirsel_bytes store_dir;     /* UTF-8 absolute path */
  void *context;              /* passed back unread; retained until ended returns */
  void (*ended)(void *context, uint32_t code, hirsel_bytes reason);
} hirsel_embed_config;

uint32_t hirsel_embed_v1_abi(void);
hirsel_bytes hirsel_embed_v1_manifest(void);
hirsel_status hirsel_embed_v1_start(const hirsel_embed_config *, hirsel_runtime *out);
hirsel_status hirsel_embed_v1_open(hirsel_runtime, const hirsel_embed_callbacks *,
                                   hirsel_bytes credential, hirsel_session *out);
hirsel_status hirsel_embed_v1_call(hirsel_session, uint64_t call, uint32_t want,
                                   hirsel_bytes args, uint32_t deadline_ms);
hirsel_status hirsel_embed_v1_abandon(hirsel_session, uint64_t call);
hirsel_status hirsel_embed_v1_subscribe(hirsel_session, uint64_t sub, uint32_t want,
                                        hirsel_bytes args);
hirsel_status hirsel_embed_v1_unsubscribe(hirsel_session, uint64_t sub);
hirsel_status hirsel_embed_v1_room_join(hirsel_session, uint64_t room, uint32_t want,
                                        hirsel_bytes args);
hirsel_status hirsel_embed_v1_room_set(hirsel_session, uint64_t room, uint32_t present,
                                       hirsel_bytes state);
hirsel_status hirsel_embed_v1_room_send(hirsel_session, uint64_t room, uint64_t to,
                                        hirsel_bytes message);   /* to = 0: whole room */
hirsel_status hirsel_embed_v1_room_leave(hirsel_session, uint64_t room);
hirsel_status hirsel_embed_v1_close(hirsel_session);
hirsel_status hirsel_embed_v1_stop(hirsel_runtime);
```

**Handles are values.** A `hirsel_runtime` or `hirsel_session` is a value, never a pointer
the runtime dereferences. The runtime keeps a process-static **registry** that is never
freed. It holds:

- the current runtime generation;
- that generation's state;
- its session table.

Generations start at 1, and each `start` that reserves the registry takes the next one,
whether or not it succeeds. Session numbers start at 1 within a generation. Neither is ever
reused in the process.

A function given a handle validates its ABI v1 arguments first (below), then looks the
handle up in the registry. A handle whose generation is not the current one, whose runtime
is not `Running`, or whose session is closed or ended, returns `HIRSEL_STOPPED`. It never
touches freed memory. A handle the host keeps across `stop` and a later `start` is
therefore stale, and the new runtime never answers for it.

**Admission barrier.** Under the registry lock, every accepted handle lookup acquires a
per-generation **admission guard** (and, for session functions, a per-session admission
guard) before releasing the lock, and drops the guard before returning. No function holds
an admission guard across a blocking wait or a callback. Every transition that ends a
session or generation (`close`, `stop` and spontaneous end) closes admission first and
waits for all already-admitted producers to drop their guards — ensuring each admitted
producer has either observed the closing/ended state, discarded its work and returned
`HIRSEL_STOPPED`, or finished publishing its work before the session's `session_ended`
drain — before releasing any session record, handle table or generation memory. The
registry lock is held only for the lookup and admission step, never across a wait or a
callback.

**Runtime states.** The registry moves through these states. Each transition is atomic
under the registry lock:

```text
Stopped --start reserves--> Starting --ready--> Running --stop--> Stopping --joined--> Stopped
                            Starting --refused or failed-------------------> Stopped
                                                Running --ends by itself--> Ended --stop--> Stopping
```

- **`start`.** `start` runs in strict phase order so malformed calls never fault on a NULL
  `out`, never call an unvalidated callback and never perturb a concurrent valid `start`:

  1. **Output check and zeroing.** If `out` is NULL, return `HIRSEL_ABI_MISMATCH` at once,
     with no write, no reservation and no callback. Otherwise set `*out` to the zero handle
     immediately, so it is zero on every return other than `HIRSEL_OK` and before any
     callback runs.
  2. **ABI v1 validation.** Validate `config` before taking the registry lock (below:
     non-NULL `config`, `size` at least v1 minimum, zero tail, matching `abi` and
     `manifest_fp`, `start_timeout_ms > 0`, non-NULL `ended`, valid `hirsel_bytes`). A
     failure returns `HIRSEL_ABI_MISMATCH` with no registry reservation, no generation
     increment and no callback.
  3. **Singleton reservation.** Under the registry lock, if the state is `Stopped`, reserve
     the registry (`Starting`, next generation) before creating any worker. In any other
     state, return `HIRSEL_ALREADY_RUNNING` with no callback.
  4. **Semantic validation and launch.** Validate CPU/ISA support (baseline-safe x86-64
     check, refusing `UnsupportedCpu` without reading the environment or calling `exit`),
     `client_bytes` against the read/control progress floor (`ClientBytesTooSmall`), grants
     (including EMB-7 target refusals, `Dir*` and `SecretSource`), directory overlap on
     opened descriptors (`StoreOverlapsDir`), and install or verify the fault handler
     (`FaultHandlerReplaced`, EMB-3). Open the store, create the workers and launch the
     place under `n_place` and `start_timeout_ms`. Wait until the place is Ready and its
     `Dialer` is published.

  Success is that publication. The state becomes `Running`, `*out` is written, and `start`
  returns `HIRSEL_OK`. The thread that called `start` may then return and exit: the
  runtime's own threads hold the launch metadata, the store and the place.
- **A failed start.** If step 4 fails (including `PlaceCapacity(Room)` or `PlaceTimedOut`
  when `start_timeout_ms` expires before Ready), `start` signals generation end (stopping
  provider waits and pure Hirsel registration loops via EMB-3's step check), joins every
  thread it created and releases everything it opened. It then calls `config.ended` once,
  synchronously, with `HIRSEL_END_REFUSED` and the reason, while the state is still
  `Starting`. Finally it moves to `Stopped` and returns `HIRSEL_REFUSED`.

  A failed start is followed by a normal `start`. A reentrant call from inside that
  `ended` behaves as follows:

  - `start` returns `HIRSEL_ALREADY_RUNNING`;
  - any function given a handle returns `HIRSEL_STOPPED`;
  - `abi` and `manifest` work.
- **Output on every return.** In both `start` and `open`, `out == NULL` returns
  `HIRSEL_ABI_MISMATCH` without writing. For every non-NULL `out`, `*out` is zeroed before
  ABI validation or any callback, so it is zero on every return other than `HIRSEL_OK`.
- **Ending by itself.** A place's `Fail` or a run stop ends a `Running` runtime by itself
  (state `Ended`). Admission closes at once under the registry lock: `open` and every
  session function return `HIRSEL_STOPPED`. Spontaneous teardown then observes the same
  ownership barrier as `stop`: it waits for every already-admitted host ABI call of the
  generation to drop its admission guard (publishing or discarding its work) and for every
  worker to leave Hirsel code (EMB-3), delivers each open session's `session_ended` in the
  order the sessions were opened, then delivers `ended` with the code and reason, and only
  after all admitted producers and workers have left releases session records, handle
  tables and generation memory.
- **`stop`.** On a host thread, `stop` of the current generation works as follows:

  1. In `Running`, it closes admission (`Stopping`). An `open` or session call that loses
     this race returns `HIRSEL_STOPPED`.
  2. It waits for ABI calls already admitted to drop their admission guards. Each holds its
     guard only for its own short non-blocking duration, never across a callback.
  3. It ends every session with `session_ended`, then runs `ended` with
     `HIRSEL_END_STOPPED`.
  4. It joins the delivery thread and every worker, unmaps every stack of the generation,
     and closes the store ("Durability" below).
  5. It moves to `Stopped` and returns `HIRSEL_OK`.

  In `Ended`, stop does steps 4 and 5 only, with no further callback. Every successful
  `start` is matched by exactly one `stop` that returns `HIRSEL_OK`, as a thread by its
  join. A second `stop` racing the first waits until `Stopped` and returns
  `HIRSEL_STOPPED`, as does a `stop` of a stale generation. On the delivery thread, `stop`
  returns `HIRSEL_BUSY`, because stop waits for the delivery thread.

**Validating arguments.** Every function validates its ABI v1 arguments before touching
the registry, reading past `size`, calling any callback or doing any work. A failure
returns `HIRSEL_ABI_MISMATCH` with no callback and no effect.

- **Output pointer first.** In `start` and `open`, `out == NULL` returns
  `HIRSEL_ABI_MISMATCH` before any write.
- **The size prefix.** A struct's `size` is read first. The **v1 minimum** of each struct
  is every field listed above. A `size` below it fails, and no field past `size` is read.
- **A larger size.** A host built against a later additive revision may pass a larger
  `size`. Bytes past the fields this library knows must be zero, or the call fails. A field
  the library cannot honour is therefore never ignored.
- **The ABI version, fingerprint and timeout.** In the config, `abi` must equal the
  library's version, `start_timeout_ms` must be positive (`> 0`), and `manifest_fp` must be
  32 bytes equal to the SHA-256 of the binding projection (EMB-5) of the manifest that
  `hirsel_embed_v1_manifest` returns. The fingerprint check catches bindings generated from
  another build of the program, whose Want list would index other entries.
- **Required pointers and lengths.** Every function pointer is required and non-NULL. A
  `hirsel_bytes` with a NULL `ptr` and a nonzero `len` fails, as does an `out` pointer that
  is NULL.
- **Context.** `context` may be NULL and is never dereferenced.
- **Copying.** `start` copies the config and `open` copies the callback table before they
  return. A table on the host's stack is valid.
- **Context retention.** The runtime retains `config.context` until `ended` has returned,
  and a session's `context` until its `session_ended` has returned. The host may release
  either then, and not before.

| Refusal | Status | Callbacks |
|---|---|---|
| a struct shorter than its v1 minimum, a nonzero tail, a NULL required pointer or `out`, a bad `hirsel_bytes`, a wrong `abi` or `manifest_fp`, or `start_timeout_ms == 0` | `HIRSEL_ABI_MISMATCH` | none |
| `start` while the registry is not `Stopped` | `HIRSEL_ALREADY_RUNNING` | none |
| `start` refused for `UnsupportedCpu`, grants, `Dir*`, `SecretSource`, `StoreOverlapsDir`, unrecoverable store, `PlaceCapacity(Room)`, `PlaceTimedOut`, `ClientBytesTooSmall`, target capability (EMB-7) or `FaultHandlerReplaced`, or a start that fails after reservation | `HIRSEL_REFUSED` | `config.ended` once, synchronously |
| `open` on a stale or not-`Running` handle | `HIRSEL_STOPPED` | none |
| `open` whose session record or credential copy exceeds host-client storage, or is refused by the C allocator | `HIRSEL_FULL` or `HIRSEL_HOST_MEMORY` | none |
| `open` whose dial is refused by the place (HTTP 503: full or draining) or whose Hello or credential fails | `HIRSEL_OK`, then the session ends | `session_ended` once (below) |

**Ids.** The host chooses each call, subscription and room id, unique among that session's
live ones (`HIRSEL_ID_IN_USE` otherwise). So an id is known before any callback can name
it: an outcome may be delivered on the delivery thread before `call` returns.

An id becomes free at one of three points:

- when its terminal callback has returned (`outcome`, `sub_ended`, or the room's
  `Ended` event);
- when `abandon`, `unsubscribe` or `room_leave` has returned `HIRSEL_OK`;
- when its session's `session_ended` has returned.

**Host ids and wire identities are separate.** Each in-flight call the client sends runs
on a leased stream lane at its wire identity `(stream, p)` (STORE-SPEC §1.8, RUNNER §2.1,
lane 3145). The client keeps a link from that lane's in-flight call to the host id's
observer. `abandon` cuts the link, but the lane stays occupied until its protocol result
arrives. The client consumes that result to settle the lane and release its credit, and
discards it.

A host id reused after `abandon` gets a new observer and a new call on an available lane.
So a late result for the abandoned call is never delivered to the reused id, and never
suppresses the new call's outcome. Subscriptions and rooms keep the same separation
between the host id and the protocol sub id.

**Exactly one end per accepted request.** Each `call` that returned `HIRSEL_OK` ends in
exactly one of three ways:

- its `outcome` callback runs once;
- `abandon(call)` returns `HIRSEL_OK`, and then no `outcome` runs for it;
- its session's `session_ended` runs while the call is live, and then no `outcome` runs.

The rule for a race between `abandon` and delivery is fixed. If the outcome callback has
started, or has run, when `abandon` takes the session's lock, `abandon` returns
`HIRSEL_NOT_LIVE` and the outcome is (or was) delivered. Otherwise `abandon` detaches the
observer and returns `HIRSEL_OK`. `abandon` never waits for a callback.

Subscriptions and rooms follow the same rule. A subscription ends with `sub_ended`, a
returned `unsubscribe`, or `session_ended`. A room ends with its `Ended` event, a returned
`room_leave`, or `session_ended`.

**Entries and bytes.** `want` is the entry's index in the binding projection's `entries`
array, which is the Want list of the generated bindings. `args` are the entry's argument
codec bytes.

The runtime decodes nothing the host sends beyond checking the codec bytes against the
entry's schema. A malformed argument is a `Refused` outcome, never a fault.

Every byte string a callback receives is the ordinary value codec's encoding of the type
below. These types are part of ABI v1 and are written into the generated codec (EMB-5).
Their test vectors are fixed bytes in the spec harness, not generator output (slice 7).

```text
// outcome: the runner's Outcome for the call, exactly the bytes a WebSocket client reads.

// change: one applied Batch of a subscription's image, or one clear (sdk.md "Subscriptions").
type Change: codec {
  invalidated: bool,            // the image was cleared by a Reset; rows is empty, replaced false
  replaced: bool,               // a completing group (sdk.md change.replaced)
  value: Option[ValueChange],   // present when the query value's encoded bytes changed
  rows: List[RowChange],        // the difference from the current image, in Batch order
}
type ValueChange: codec { old: Option[Bytes], new: Bytes }
type RowChange: codec {
  slot: u32,                    // the slot's index in the entry's slots array
  key: Bytes,                   // the ordered key encoding, without the store's collection id
  old: Option[Bytes],           // row value codec bytes; None for an insertion
  new: Option[Bytes],           // None for a deletion
}

// sub_ended: why a subscription ended. Its id is free after the callback.
type SubEnd: codec =
    Server(Bytes)               // the Ended frame's Outcome bytes (Refused(Overloaded), Failed(Gone), ...)
  | ClientCapacity              // host-client storage could not hold the image (below)

// room_event: one event of a joined room, in the order the client applied them.
type RoomEvent: codec =
    Admission(Change)           // the admission view: value is the Grant, rows its slots
  | Peers(PeersChange)          // sdk.md onPeers, after the Part is applied
  | Message(MessageIn)          // sdk.md onMessage
  | Gap(u64)                    // sdk.md onGap: messages to this client the server dropped
  | Lost(u64)                   // sdk.md onLost: messages from this client that were dropped
  | Full                        // sdk.md onFull
  | Ended(SubEnd)               // the room's final event; its id is free after it
type PeersChange: codec {
  incarnation: Option[Bytes],   // None before any non-full Part; Some(16 bytes) once established
  revision: Option[u64],        // None until the Part with snapshot set
  renewed: bool,
  me: Option[u64],              // this membership's peer id (sdk.md self)
  joined: List[Peer],
  states: List[PeerState],      // peers whose state was set, in Part order
  left: List[u64],
}
type Peer: codec { peer: u64, caller: Bytes, state: Option[Bytes] }
type PeerState: codec { peer: u64, state: Option[Bytes] }
type MessageIn: codec { from: u64, message: Bytes, direct: bool }

// session_ended: why a session ended. It is the session's last callback.
type SessionEnd: codec =
    Closed                      // the host called close
  | RuntimeStopped              // the host called stop
  | RuntimeEnded(u32, Bytes)    // the runtime ended by itself: hirsel_end_code and reason
  | DialRefused(u16)            // the place answered the dial with this HTTP code (503)
  | ClosedByPlace(u16)          // the place closed the session with this close code
```

Some semantics are pinned beyond the types:

- A `Change` with `replaced` and empty `rows` and `value` is the first completion of an
  empty image. It is always delivered, as in sdk.md.
- A `Change` with `invalidated` is followed by the next instance's first completion.
- In one room, the client matches [sdk](sdk.md#rooms) (`sdk/client/room.ts`): each Part
  applies `joined`, then `states`, then delivers `msgs`, then applies `left` and snapshot
  completion, then delivers `Gap`, `Lost` and `Full`. So a message's sender is always in
  `peers` with its updated state when `Message` runs, and a Part with no roster, `me` or
  `revision` change (such as a first `Full` Part, whose zero head establishes no
  incarnation) emits no `Peers` event:
  - When a Part has no `msgs`, the client emits at most one `Peers` event (combining
    `renewed`, `me`, `joined`, `states`, `left` and `revision`), and only if at least one
    of those changed.
  - When a Part has `msgs`, the client first emits a pre-message `Peers` event if any of
    `renewed` (with prior-incarnation removals in `left`), `me`, `joined` or `states`
    changed; then emits each `Message` in Part order; then emits a post-message `Peers`
    event if `part.left`, snapshot-unconfirmed removals or `revision` changed.
  - `Gap(n)` (when `gap > 0`), `Lost(n)` (when `lost > 0`) and `Full` (when `part.full`)
    follow in that order.
  - `Admission` and `Peers` events have no order between them, as `Live` and `Batch`
    frames have none.

**Rooms.** The ABI exposes the TypeScript room surface:

- **Join and leave.** `room_join` joins, and `room_leave` sends the protocol's leave.
- **State.** `room_set(room, present, state)` sets the newest state (`present` = 0 clears
  it).
- **Messages.** `room_send(room, to, message)` sends to the whole room (`to` = 0) or to peer
  `to`.
- **Never blocking.** `room_set` and `room_send` never block. A value set before membership
  is stored, and becomes the member's first state. A message sent before membership, or
  with more than one Live frame (64 KiB) unsent, is dropped and reported as `Lost(1)`, as
  in sdk.md.
- **Bounds.** A state or message over the room's registered bound returns
  `HIRSEL_OVER_BOUND`, and nothing is sent. The server would close the session with 1002
  for such a frame.

**Ownership.** Every `hirsel_bytes` the host passes is borrowed for the duration of the call
and copied by the runtime before the call returns. Every `hirsel_bytes` the runtime passes to
a callback is borrowed for the duration of the callback; a host that keeps it copies it.
`hirsel_embed_v1_manifest` returns bytes that live as long as the process. Nothing crosses
that the other side must free, so the ABI has no free function.

**Threads and callbacks.** The runtime runs its own worker threads. Every callback of one
generation, except the refusal `ended` described above, runs on one runtime-owned
**delivery thread**. Callbacks run one at a time, in the order the runtime produced them.
The host must not block that thread for long. The generated bindings only resume a
continuation or yield to a stream there.

Every ABI function may be called from any host thread, including from inside a callback.
Two exceptions apply on the delivery thread:

- `stop` returns `HIRSEL_BUSY`;
- `close` defers (below).

Hirsel never calls into host code synchronously, except for the refusal `ended` inside
`start`. It never waits for a host answer: there are no host-implemented entries and no
reverse calls. Push to the host is a subscription change or a room event.

**Close.** `close(session)` ends a session and its calls, subscriptions and rooms. It
removes every undelivered callback of that session from the queue, and runs
`session_ended(Closed)` as the session's last callback. The order is fixed:

- **On a host thread.** `close` first closes the session's admission, so every later call
  on it returns `HIRSEL_STOPPED`. It then waits for every already-admitted producer call on
  that session to drop its admission guard (publishing or discarding its work; producers
  never block on delivery), waits for any callback of that session already running on the
  delivery thread to return, lets no further callback of the session start other than
  `session_ended`, waits for `session_ended` to return, releases the session's state, and
  then returns `HIRSEL_OK`. While it waits, it holds no lock that a callback needs: not the
  registry lock, not the session's lock.
- **On the delivery thread.** This covers a call from inside any callback, including one of
  the same session. `close` closes the session's admission and returns `HIRSEL_DEFERRED` at
  once. After the current callback returns, the delivery thread waits for any
  already-admitted producer calls on that session to drop their admission guards (bounded
  and non-blocking), drains queued callbacks of that session, runs `session_ended(Closed)`,
  and releases the session's state. A callback that closes its own session never waits for
  itself.
- **On a closed or ending session.** `close` returns `HIRSEL_STOPPED` immediately without
  waiting.

The host may release a session's context only once `session_ended` has returned. On a host
thread, `close` returning `HIRSEL_OK` (or `stop` returning `HIRSEL_OK`) guarantees that
`session_ended` has already returned. A `close` that returns `HIRSEL_DEFERRED` or
`HIRSEL_STOPPED` is **not** a context-release barrier — another `close`, a deferred
self-close, `stop` or a spontaneous end may still owe or be running `session_ended`. In
those cases the host may release `context` only after observing `session_ended` return (or
after `stop` returns `HIRSEL_OK`).

`stop` orders its final callbacks the same way: every session's `session_ended`, then
`ended`, and nothing after `ended`. A Swift binding that resolves its waiters in
`session_ended` therefore resolves each one exactly once.

**Cancellation.** The runner protocol has no per-call cancel frame, and this ABI adds none.
`abandon(call)` withdraws the host's interest. Once it returns `HIRSEL_OK`, that call's
`outcome` callback never runs, and the bindings resolve the caller's task as cancelled.

The place still runs the call to its outcome or its `deadline_ms`. A transact entry that
commits stays committed, exactly as for a TypeScript caller that stops awaiting.
`unsubscribe` and `room_leave` send the protocol's own Unsubscribe and leave.

**Host-client storage.** The embed creates two kinds of storage, and charges them
separately:

- **Program storage** is the place's, under MEM-19, MEM-21, MEM-24 and MEM-28 and the
  runner's session ledger, exactly as for a session over `local`'s dial. The embed creates
  nothing on the place's side that a dialled session does not.

  So for matched inputs, limits and schedule, a call through the embed never raises a
  program charge above what the same call over the in-process dial charges. It never
  changes a refusal kind either: a program that fits keeps fitting, and `Failed(OutOfBytes)`
  stays `Failed(OutOfBytes)`.
- **Host-client storage** is everything on the client side of the dial. It is charged to
  one per-generation account bounded by `config.client_bytes`. Charges go to that account
  and are never charged to the place.

Host-client storage holds:

- each open session's record, its copied `credential` until `Hello` is sent, and its
  terminal-notification reserve (`session_ended`);
- the copied `args` of a call until the client has framed and sent it;
- each outstanding call's record;
- each subscription image and room roster;
- each frame read from the dial and not yet applied;
- each callback's bytes waiting for the delivery thread.

**Read/control progress reserve.** At `start`, the runtime partitions `config.client_bytes`
into a fixed **progress reserve** (at least `max_frame + 14` bytes for one inbound frame
plus fixed control/terminal notification room for `SubEnd.ClientCapacity` and
`session_ended`) and the **admissible pool** (`client_bytes - progress_reserve`). If
`config.client_bytes` is smaller than the progress reserve, `start` refuses with
`ClientBytesTooSmall`. Producer admissions draw only from the admissible pool, never from
the progress reserve, so a full admissible pool can never starve the inbound read that
consumes a reply and refunds its call record, nor block a terminal notification.

The account is charged in four ways:

- **Admission.** `open`, `call`, `subscribe`, `room_join`, `room_set` and `room_send`
  reserve their copy and record (and, for `open`, the session's record, copied `credential`
  and per-session `session_ended` reserve) from the admissible pool before anything is
  registered or sent. If the admissible pool cannot hold them, they return `HIRSEL_FULL`
  and register or send nothing (for `open`, no session is opened and no `session_ended`
  runs). The host may retry once callbacks or calls have drained. `HIRSEL_HOST_MEMORY`
  means the C allocator refused memory the account had room for. Neither status is a runner
  refusal: a runner refusal arrives as a `Refused` outcome or a `SessionEnd`, and program
  capacity arrives as `Failed(OutOfBytes)` in an outcome.
- **Reading.** The client reads an inbound frame from a session's dial into the reserved
  progress slot (or additional `max_frame + 14` bytes from the admissible pool). Control
  and call-completion frames (`Done`, `Ended`, `Welcome`, `Placed`) can therefore always be
  read, applied and refunded even when the admissible pool is full. If applying a frame
  would queue callback bytes that the admissible pool cannot hold (such as a stalled
  delivery callback), the client pauses further reads after that frame until callbacks
  drain; the dial's 16 frame slots per direction then fill (RUNNER §14.11), and the place's
  existing backpressure and slow-reader rules apply, exactly as for a TCP client that stops
  reading. The client never drops a frame.
- **Images.** Applying a Batch charges its growth to the admissible pool. A Batch that the
  admissible pool cannot hold ends that subscription or room with `SubEnd.ClientCapacity`
  (queued using the terminal reserve so it delivers even at capacity), frees its image, and
  sends the protocol's Unsubscribe. It is an explicit end, never a silent drop.
- **Refund.** A charge is refunded when its owner goes:
  - a session's `credential` copy when `Hello` is sent, and its session record and
    `session_ended` reserve when `session_ended` returns;
  - a sent call's copy when it is sent;
  - a call's record when its result is consumed, delivered or not;
  - an image when its subscription ends;
  - a callback's bytes when the callback returns.

  After `abandon`, `close` or `stop`, the account returns to what is still live.

No step of this waits while holding a lock that delivery needs. A producer inside a callback
gets `HIRSEL_FULL` rather than blocking, so a reentrant producer cannot deadlock delivery.

**Sessions.** A session is one dialled connection whose client kernel (lane 3145)
multiplexes concurrent calls across leased stream lanes (up to `1 + q` in flight, from
`Welcome`). `open` registers the session once its admission succeeds and returns
`HIRSEL_OK`. The dial, Hello and credential then proceed on the runtime's threads. Calls
made before the session is ready wait in the client, as in `client.ts`.

A refused dial or failed Hello ends the session with `session_ended`, carrying
`DialRefused(503)` or `ClosedByPlace(code)`, and so ends every call queued on it.

The client state is in memory. An embedded place and its client share one process, so an
in-process session does not reconnect across `stop`/`start`: a session ends with its
runtime, and the shared 3145 kernel's reconnect replay is exercised by the kernel's
transition vectors and transport-loss laws.

**Durability.** The store's rules decide what a host may rely on:

- **An observed commit is durable.** An `outcome` that reports a commit (a transact's
  `Returned`, including `Returned(Error(e))`, or a `Failed` backed by a reply) is delivered
  only after that commit is durable at the store's strongest level. This follows the
  runner's output gate and RUNNER §2.1 V4, and it holds because the embed delivers the
  outcome frame the place released. The host can rely on such a commit surviving any later
  crash, kill or reopen of the same `store_dir`. Every commit before it survives too: the
  durable state is a prefix of the commit order (STORE-SPEC §1.6).
- **An unobserved commit is all or nothing.** A transact whose outcome the host did not
  observe may be wholly present or wholly absent after reopen, never in part. This covers a
  call with no outcome yet, one abandoned, one ended by `close`, `stop` or a session end, and
  one in flight when the process was killed. A query after reopen tells which.
- **No automatic replay.** The runtime never replays a call. A new session starts with no
  client state, so a fresh stream never resends an old request. Whether a retry is safe is
  the program's design (an idempotent entry), decided by the host after a query.
- **Suspension.** When the process is suspended and resumed (iOS background), the runtime
  continues with its sessions, calls and subscriptions intact. Deadlines and timers follow
  the existing clock rules. Whether a suspension counts toward a deadline is the clock's
  rule, qualified on device (EMB-9).
- **A kill or transient poison.** A killed process gets no `stop` and no callback, and
  loses its client correlation. A transient write or sync failure while running poisons the
  open `Db` (STORE-SPEC §1.6 `Poisoned`) and ends the place with `Fail` (`ended` with
  `HIRSEL_END_PLACE_FAILED`); poison is never persistent on disk. Once the failed owner
  stops and closes, the next `start` on the same `store_dir` opens and **recovers the
  durable prefix** with all earlier acknowledged commits intact. Only a store whose
  on-disk state cannot be recovered (such as a corrupt manifest or journal) refuses `start`
  with `HIRSEL_REFUSED` carrying the store's recovery error.
- **Graceful stop discards and then closes.** `stop` does not wait for calls in flight.
  Their outcomes are not delivered, and `session_ended(RuntimeStopped)` ends them. The
  store's writer finishes the group commit it has started and that commit's durability. So
  a commit that was issued is durable or was never issued. The store then closes and
  releases its writer lock before `stop` returns, so the next `start` opens the store
  without waiting.

**One runtime per process.** At most one generation exists at a time (the registry above).
A host links one Hirsel library. Two libraries define the same `hirsel_embed_v1_*` exports,
and each library's anchor forces its export member into the link (EMB-1). So linking two
fails the host's link with a duplicate symbol, in either order. That is the intended
diagnostic. The archive digest stays diagnostic only.

**Versioning.** `HIRSEL_EMBED_ABI` is the ABI version, and every export's name carries it
(`hirsel_embed_v1_`).

- **Additive changes keep the version.** These are a new function, a new status or end code,
  a new `SubEnd`, `RoomEvent` or `SessionEnd` variant appended at the end, and a field
  appended to a struct whose size is passed. A host must treat an unknown status, code or
  variant as a terminal error of its kind. The validation rules above make an appended
  field safe in both directions.
- **Any other change is a new version** with new names, so a host generated for one version
  never calls another version's function by mistake.

## EMB-5: Swift bindings and the XCFramework

`hirsel sdk swift <program> --output DIR` <!-- doc-ref: planned --> builds the program as a
static library for every Apple target in the records (EMB-2). It generates Swift bindings
and assembles a Swift package. It takes the build options `hirsel sdk ts` takes.

**The manifest.** Every static-library build computes its program's manifest and embeds it
(returned by `hirsel_embed_v1_manifest`); `sdk swift` reads it from the libraries it built.
A static library cannot run a Describe launch on the build host, and an iOS one cannot run
there at all. So the build runs the Describe launch on the evaluator, with the library's
target's toolchain constants (PROG-6).

The embedded manifest is the ordinary sdk.md manifest, with two changes: `launch.target`
is the library's target, and `launch.digest` is empty. A library cannot contain its own
digest; the digest is the generated bindings' constant (EMB-3). Each entry keeps its
`revision`.

This spec decides that the evaluator's Describe gives the same entries, codecs and
fingerprints as the native Describe; that is not checked here. Both run the same
registration code. Slice 3 carries the law that decides it: on Linux, the evaluator
manifest's binding projection must equal the native Describe manifest's. If it fails, the
static-library kind waits until it holds.

**The binding projection** is its own format. It is the part of a manifest that a binding
depends on: everything except `launch` and each entry's `revision`. It is a compact UTF-8
JSON object with lexicographically sorted object keys and a final newline, as sdk.md writes
a manifest. It has three fields:

- `version`: the integer `1`;
- `tables`: as in the manifest;
- `entries`: as in the manifest, each without `revision`.

Its bytes are hashed with SHA-256 for `manifest_fp`. The runtime computes the projection
from its embedded manifest with the same function, so the projection never changes with
`n_place`, digest or revisions.

`sdk swift` requires the binding projections of all its libraries to be byte-identical. If
a program registers different entries or codecs on different Apple targets, it refuses with
`TargetDependentEntries(target_a, target_b)` and writes nothing.

**What it writes.**

```text
DIR/
  Package.swift                      swift-tools-version 6.0; platforms from EMB-2
  projection.json                    the binding projection; its SHA-256 is manifest_fp
  manifests/<slice>.json             each slice's ordinary sdk.md manifest, as sdk json writes it
  Sources/<Name>/Codec.swift         codec for every type in the projection and the EMB-4 callback types
  Sources/<Name>/Entries.swift       typed stubs, Want list, manifest_fp, per-slice digests
  Sources/<Name>/Runtime.swift       Runtime and Session over the C ABI
  <Name>Core.xcframework/
    Info.plist
    macos-arm64/lib<name>.a
    macos-arm64/Headers/hirsel_embed.h, hirsel_product.h, module.modulemap
    ios-arm64/lib<name>.a
    ios-arm64/Headers/...
    ios-arm64-simulator/lib<name>.a
    ios-arm64-simulator/Headers/...
```

`<slice>` is the record's `xcframework_slice`. `<Name>` is the program's package name in
UpperCamelCase. `hirsel_product.h` declares the library's anchor and its inline
`volatile`-read helper `hirsel_embed_v1_require_anchor_<b>()` (EMB-1), which `Runtime`
calls on start.

The driver assembles the XCFramework, not `xcodebuild`. It is a directory of the three
archives, their headers and module maps, and an XML property list. For each library the
list names:

- `LibraryIdentifier`, `LibraryPath` and `HeadersPath`;
- `SupportedArchitectures` (`arm64`);
- `SupportedPlatform`;
- for the simulator, `SupportedPlatformVariant` = `simulator`.

Keys are sorted and there are no timestamps, so the XCFramework is a pure function of its
inputs. The simulator slice is arm64 only; an x86_64 simulator is not a target.
`Package.swift` declares a binary target for the XCFramework and a source target for the
Swift files, with `platforms` from the records (`.iOS("26.0")`, `.macOS("26.0")`).

**The Swift surface.** It follows the TypeScript stubs (sdk.md section 4), with Swift types.

- **Types.**
  - Integers are Swift integers of the same width; `Bytes` is `Data`; `String` is `String`.
  - Records are structs, and sums are enums with associated values.
  - `Option` is `Optional` only when unnested. A nested Option is a generated enum, so
    `Some(None)` survives.
  - `Result` is a generated enum, not Swift's `Result`, because the error type need not be
    an `Error`.
  - Every generated type is `Sendable` and `Equatable`.
- **Starting.** `Runtime.start(grants:nPlace:clientBytes:startTimeoutMs:)` resolves the
  container directories (EMB-3) and calls `hirsel_embed_v1_require_anchor_<b>()` so the
  anchor reference survives `-O` and whole-module optimization. It passes the generated
  `abi`, the projection fingerprint and the running slice's library digest. There is one
  generated constant per slice, chosen by `#if targetEnvironment(simulator)` and
  `#if os(...)`. `clientBytes` defaults to 64 MiB and `startTimeoutMs` defaults to 10,000
  ms (matching `launch.md`'s `--place-timeout`), both written in the generated signature,
  never hidden.

  A refused start throws an error carrying the status, and for `HIRSEL_REFUSED`, the
  reason. `runtime.open(credential:)` returns a `Session`. `runtime.stop()` is `async`.
  The runtime value holds the `hirsel_runtime` value, not a pointer.
- **Calls.** `api.call.<module>.<name>(args, deadline:) async throws -> Outcome<R, E>`.
  Each `Session` assigns call, subscription and room ids from a monotonic 64-bit counter
  (starting at 1, never reused within the session), so a late cancellation handler can
  never alias another call's id. Each call has a lock-protected state box
  (`PendingIssue` → `Issuing(cancelRequested)` → `Issued` / `CancelledBeforeIssue` /
  `Abandoning` → `Resolved`) inserted into the session's table *before* calling C, so the
  continuation is resumed exactly once and cancellation never races C registration:
  - If cancellation arrives in `PendingIssue`, the box moves to `CancelledBeforeIssue` and
    resumes with `CancellationError`; the issuer observes `CancelledBeforeIssue` and never
    calls `hirsel_embed_v1_call` or `abandon`.
  - If cancellation arrives while the issuer is in `Issuing` (inside
    `hirsel_embed_v1_call`, before C registration finishes), the handler sets
    `cancelRequested` and does not call `abandon` yet. Once `hirsel_embed_v1_call` returns
    `HIRSEL_OK`, if the box is not already `Resolved` (an early outcome arrived before C
    returned), the issuer moves to `Abandoning` and calls `abandon`: on `HIRSEL_OK` it
    resolves with `CancellationError`; on `HIRSEL_NOT_LIVE` delivery already won the C race
    and resolves the box.
  - If cancellation arrives in `Issued`, the handler moves to `Abandoning` and calls
    `abandon` with the same `HIRSEL_OK` / `HIRSEL_NOT_LIVE` rule.
  - `outcome` and `session_ended` lock the box; if not `Resolved`, they move to `Resolved`
    and resume the continuation (even if the issuer is still in `Issuing`).
  - `HIRSEL_FULL`, `HIRSEL_HOST_MEMORY` and `HIRSEL_STOPPED` from `hirsel_embed_v1_call`
    move the box to `Resolved` and throw at once.
- **Subscriptions.** `api.subscribe.<module>.<name>(args, buffer:) ->
  AsyncThrowingStream<Change, Error>` yields one decoded `Change` per callback. Its buffer
  is `.bufferingOldest(buffer)`, with a default of 1,024 changes written in the signature.

  If a yield finds the buffer full, the stream finishes with `ClientCapacity`, and the
  binding unsubscribes. A slow consumer thus gets an explicit end, never a dropped change.
  Ending the stream's iteration calls `unsubscribe`.
- **Rooms.** `api.room.<module>.<name>(args, buffer:)` returns a room value. It has an
  `AsyncThrowingStream` of `RoomEvent`s with the same buffer rule, plus `set(_:)`,
  `send(_:to:)` and `leave()`, matching EMB-4's room calls.

**Where it lives.** The header, the generator, the XCFramework assembly and the
`Package.swift` generation live in the Hirsel repository, in the driver beside
`hirsel sdk ts`. Each is a function of facts Hirsel owns: the manifest, the ABI version and
the target records. The application, its Xcode project, signing, entitlements and the device
run live in the host repository (`gillesmag/hirsel-ios`).

This settles DECISIONS 2026-11-26 20:3xZ ("target + runtime + output mode live in the Hirsel
repo; Swift host/Xcode glue stays in gillesmag/hirsel-ios"). The Swift glue generated from a
manifest is Hirsel's; the Swift an application author writes is the host repository's.

## EMB-6: Link-kit rows for iOS

The link kit (`tools/native-toolchain/README.md`, "Link kit") gains one TBD v4 stub set per
iOS variant, generated from Hirsel-authored symbol lists, as the macOS rows are. Nothing is
copied from an Apple SDK, and no Apple file is shipped.

- **Lists.** `darwin/<library>.symbols` stays the macOS list. The iOS lists are
  `ios/<library>.symbols` <!-- doc-ref: planned -->, shared by both iOS variants.

  Each iOS list starts as the macOS list minus what EMB-7 refuses or replaces on iOS:
  process creation, `proc_pidinfo`, `pthread_fchdir_np`, the routing socket and the macOS
  Keychain root read. It adds the Security and CoreFoundation names that `Trust.System`
  uses on iOS (EMB-7). Adding a name is a reviewed one-line change, as for macOS.
- **Stubs.** The stubs are `libSystem.tbd`, with install name `/usr/lib/libSystem.B.dylib`,
  and `CoreFoundation.tbd` and `Security.tbd`, with their framework install names. Each has
  `targets` `arm64-ios` (device) or `arm64-ios-simulator` (simulator).

  As on macOS, every stub is linked. The generated `module.modulemap` links the
  `CoreFoundation` and `Security` frameworks for every program, so the host app needs no
  per-program framework list. Both frameworks ship on every iOS device. (Lane 3137's report
  decision 19 said to link only the frameworks used. This rule replaces it.)
- The kit's manifest row for each variant records its floor from EMB-2. The kit digest
  enters every iOS build key, as it does for other targets.
- The import audit (EMB-1) checks a static library's undefined symbols against these stubs.

Zig's `libSystem.tbd`, against which the macOS list is checked, lists no iOS targets. So the
iOS names cannot be checked on Linux; they are checked on the Mac.

An ordinary link extracts only the members it needs, so a fixture link cannot certify every
listed name. Instead the simulator gate (EMB-9) builds a **symbol reference fixture**: one
object generated from the iOS lists that references every listed name. It links that object
with Apple's linker against the iOS SDK, once for the device and once for the simulator. A
name the SDK does not export fails that link. The same gate links the fixture's archive
into a host and runs it. Signed device execution is separate (EMB-9 item 2).

## EMB-7: Capabilities on iOS

Every capability either has an iOS provider or is refused at the earliest point that can
tell. Two points exist:

- **At build, by reach** (PROG-6, `target.unsupported(why)`). This applies where the
  capability's operations are their own provider functions, so only a program that calls
  them reaches them: `Exec` and the Unix-domain operations. Their iOS arms call
  `target.unsupported`.
- **At embedded start, by grant**, for every refused capability. `start` refuses
  (`HIRSEL_REFUSED`) a grant text with an `Exec` grant or a `Unix` rule. A program that
  never reaches the operation can still be handed such a grant. The refusal says so before
  any work, rather than leaving an unusable grant in place.

A refusal by reach fits only an operation that is its own provider function. A capability
chosen by a run-time value, such as `Trust` (`System | Roots(b)`), is retained by every
program that uses the shared operation, so it cannot be refused by reach. On iOS every such
capability is provided.

Inherited listeners are not a refusal. The standalone entry takes them
(`crates/hirsel-core-rt/src/entry.rs`), and the embedded launch never runs that entry. So an
embedded program sees exactly what a standalone program started without activation sees.

| Capability | iOS | Rule amended |
|---|---|---|
| `Exec` (spawn, wait, signal) | refused at build by reach and at start by grant: "iOS apps cannot create processes". An iOS app has no `posix_spawn` or `fork` that App Review admits. | [exec](exec.md) |
| Inherited listeners (`LISTEN_FDS`, socket activation, raw TCP adoption) | none inherited: the embedded launch takes no descriptor (EMB-3), as a standalone run without activation | [launch](launch.md) |
| Unix-domain sockets (NET-12 to NET-14) | refused at build by reach and at start by grant: "Unix socket naming needs `pthread_fchdir_np`, which iOS does not offer". The macOS provider's directory-relative naming has no public iOS equivalent. | [net](net.md#unix-domain-sockets) |
| NET-18 public-only connect | provided. Before every public-only connect, the provider takes a fresh `getifaddrs` snapshot (after the connect is requested and before it is issued) and classifies the destination against it. There is no change source on iOS, so there is no cached table. | [net](net.md#net-18-public-only-connects) |
| `Dir` grants | provided by the macOS kernel path (EMB-2), with the embedded grant sources of EMB-3 | [launch](launch.md#launcher-resolved-directories) |
| `Trust.System` | provided by Apple's platform verifier (below). `Trust.Roots(pem)` is provided as on every target. | [tls](tls.md) |
| Launch digest | the embedder's value (EMB-3), else `Unavailable`. Never `_NSGetExecutablePath` or `proc_pidinfo`. | [launch](launch.md) |
| TCP connect and listen, UDP, DNS, clocks, files under granted directories, the store | provided, by the Darwin providers. They use only public BSD and Mach interfaces. The device gate qualifies each. | none |

**`Trust.System` on iOS.** iOS gives an app no readable system root store; its public route
to the system roots is trust evaluation. So the iOS `System` verifier is Apple's platform
verifier, as `rustls-platform-verifier` uses it on Apple targets. `Roots(b)` keeps the
existing verifier, which trusts exactly `b` and never consults SecTrust.

The TLS-1 checks before issue keep their order: socket origin, name, ALPN, `Roots` bundle,
then the TLS-3 rule cover. The cover check includes the rule's trust kind and, for `Roots`,
the exact digest. A `System` handshake under a `Roots=` rule is `Denied` before any provider
call, as on every target.

The handshake's signature checks (CertificateVerify) stay the TLS provider's. So does
everything else in the provider. For a `System` handshake, the provider decides the chain in
two steps.

1. **Its own leaf checks, in this order.**
   - A leaf it cannot parse is `Tls(Certificate(Invalid))`.
   - A leaf not valid for the TLS-1 verified `name` is `Tls(Certificate(NameMismatch))`.
     For an IP literal, the name is matched against IP subject alternative names.
   - A leaf outside its validity at the provider's wall clock is
     `Tls(Certificate(Expired))` or `Tls(Certificate(NotYetValid))`.

   These run before SecTrust, so the three variants that TLS-1 distinguishes never depend
   on which single error Apple's evaluator chooses to report.
2. **Apple's chain decision.** The provider evaluates the chain with SecTrust:
   - It builds a `SecCertificate` for each certificate the peer sent, leaf first, and a
     `SecTrust` over them with `SecPolicyCreateSSL(true, name)`.
   - It sets the verification date to the provider's wall clock
     (`SecTrustSetVerifyDate`).
   - It forbids network fetches (`SecTrustSetNetworkFetchAllowed(false)`).
   - It calls `SecTrustEvaluateWithError`. The chain is accepted if and only if that
     returns true.

   **Fail closed.** If creating the policy, the trust object or a certificate fails, or
   setting the date or the fetch policy returns an error, the handshake fails with
   `Tls(Certificate(Invalid))`, with Apple's status in the diagnostic. It is never
   evaluated with a default date or fetch policy.

   A false result maps by the error's code. The names are the Security framework's
   `errSec*` constants:

   | Code | Result |
   |---|---|
   | `errSecCreateChainFailed`, `errSecNotTrusted` | `Tls(Certificate(UnknownIssuer))` |
   | `errSecHostNameMismatch` | `Tls(Certificate(NameMismatch))` (unreachable after step 1; kept for totality) |
   | `errSecCertificateExpired` | `Tls(Certificate(Expired))` (an intermediate's validity) |
   | `errSecCertificateNotValidYet` | `Tls(Certificate(NotYetValid))` |
   | every other code, revocation and policy codes included | `Tls(Certificate(Invalid))` |

   Every failure carries Apple's code in its diagnostic.

**Blocking, deadlines and cancellation.** `SecTrustEvaluateWithError` blocks. It runs on a
helper thread as an independently heap-owned evaluation job that copies the peer
certificate DER chain, the verified hostname and the verification timestamp before launch,
borrowing zero generation memory, zero `TlsRequest` buffers, zero socket descriptor and
zero rustls session state. It never runs on the delivery thread or on a host thread inside
an ABI call. The handshake's `until` and cancellation apply while it runs:

- A passed deadline gives `TimedOut`, and a cancellation gives `Aborted`. Both close the
  socket and free the TLS session immediately inside the abort callback, matching
  [TLS-4](tls.md#tls-4-provider-calls) with no double close.
- Generation teardown and `stop` do not block waiting for an outstanding
  `SecTrustEvaluateWithError`, keeping teardown bounded (EMB-3).
- When an aborted or orphaned evaluation eventually returns (including after `stop` and a
  subsequent `start`), it sees its cancellation token is set and drops its own `CF*`
  objects and result without touching generation memory, invoking any callback or closing
  any socket.

A recorded handshake replays its recorded result, whichever verifier produced it.

**What `System` means on iOS.** The evaluator consults the device's current trust settings
at each handshake. There is no root set loaded once per run. User-installed and
MDM-installed roots that the device trusts are trusted, which is what `System` means: the
host's root store.

Hirsel itself checks no revocation, and network fetch is off. But the iOS evaluator still
applies its own local trust policy. That policy may include revocation and distrust data
the system already holds, and its certificate-transparency requirements. A chain that
policy rejects gets the mapped code above, usually `Invalid`. [tls](tls.md) is amended to
say so.

macOS keeps its current `System` provider in this track: the Keychain root read through
`rustls-native-certs`, at `crates/hirsel-core-rt/src/os_tls.rs` (`system_roots`; the
variant mapping is `protocol_error`). The platform verifier is the likely end state for
every Darwin target, so that both use one decision procedure and the macOS root read goes.
That unification is a separate follow-up outside the iOS track.

## EMB-8: Refusals

**Invariant.** Main never accepts a build request that it cannot fulfil hermetically. Every
request that cannot be fulfilled is refused at one of these points, before any work it would
waste:

| Refusal | Where | Kind |
|---|---|---|
| a command that runs its image given `--emit staticlib` <!-- doc-ref: planned -->, or `sdk swift` given `--emit` <!-- doc-ref: planned --> | request validation, first | `Unsupported(command, kind, "this command runs what it builds")` |
| target named in the wrong spelling (`arm64-apple-ios`, `aarch64-apple-ios18.0`) | request validation, where `--target` is resolved | unknown target, listing the accepted triples |
| (target, output kind) not in the EMB-1 table, such as `--emit exe` <!-- doc-ref: planned --> for an iOS target | request validation, before installation lookup | `Unsupported(target, kind, why)` |
| no embed slot for the target in the installation | installation lookup | `MissingRuntime(target, "embed")`, as a missing runtime slot is today |
| reached `Exec` or Unix operation on iOS (EMB-7) | front, by reach | PROG-6 `target.unsupported(why)` with the reaching path |
| foreign archive member for the wrong platform or a later floor | FFI-3 admission, before the link or archive | the FFI-3 member diagnostic (EMB-2's `macho_platform` and `floor`) |
| import that no link-kit stub exports | the import audit (EMB-1), before the archive is written | `UnlistedImport(name, member)` |
| bindings whose manifests differ between Apple targets | `hirsel sdk swift` <!-- doc-ref: planned -->, before output | `TargetDependentEntries(a, b)` |
| arguments that break ABI v1 (EMB-4 "Validating arguments", including `start_timeout_ms == 0`) | the embed ABI, before any read past `size` or registry reservation | `HIRSEL_ABI_MISMATCH` |
| `UnsupportedCpu`, grant outside the program's wants, a grant for a capability EMB-7 refuses on the target, `DirBase`, `DirSource`, `DirUnavailable`, `SecretSource`, `StoreOverlapsDir`, an unopenable or unrecoverable store, `ClientBytesTooSmall`, `PlaceCapacity(Room)`, `PlaceTimedOut`, `FaultHandlerReplaced` | the embedded launch's `start` | `HIRSEL_REFUSED`, with the reason in `ended` |
| refused stack-growth `mprotect` (in `fiber::room` pre-commit or the fault handler) or refused stack/payload mapping (EMB-3) | the embedded runtime | `HIRSEL_END_RUN_STOP` (`"HostMemory"`); the unit and generation abort fail-closed, the host survives, and any failed-growth stack is retired and unmapped whole (`munmap`) |

The last three rows are the only ones after the build. They concern the host's inputs (its
arguments, grants, directories and store) and host-memory availability during execution,
not the build's.

**FFI-3 on iOS.** The Mach-O member rows of [FFI-3](foreign.md) apply to both iOS targets
with the platform and floor from the record:

- **Platform.** A member's `LC_BUILD_VERSION` must name the target's `macho_platform`: 2
  for the device, 7 for the simulator. A device member is refused for the simulator, and
  the other way round.
- **Minimum version.** The member's minimum version must be no later than the target's
  `floor`. A member built for a later minimum is refused, because it could use an
  interface the floor lacks. A member with an earlier minimum is admitted: it runs on the
  floor. PR #11 admitted `minos` up to 28.0, which inverted this rule.

Hirsel's own objects carry exactly the floor. Foreign and prebuilt runtime members (`ring`'s
C and assembly objects among them) carry their own minimum, no later than the floor.

## EMB-9: Gates and std provisioning

**The Apple check.** iOS has no host on Linux, so a slice's gate is `cargo check` of the
shipped feature set for both iOS triples, as `aarch64-apple-darwin` is checked today.
`tools/apple_check.py` runs it, over one table of `{triple: crates}`.

It checks with the toolchain's own `rustc` and a sysroot that the Rust bootstrap provisions.
`tools/buck2/toolchain-lock.json` has `rust-std-ios` and `rust-std-ios-sim` rows
(hermeticbuild rust-redist, at the toolchain's Rust version).
`tools/buck2/bootstrap_rust_toolchain.py` installs their `lib/rustlib/<triple>/lib`, as it
does `rust-std-darwin`.

It sets `CC_<triple>`, `CXX_<triple>` and `AR_<triple>` to `true` and
`CRATE_CC_NO_DEFAULTS=1`, so no build script runs `xcrun` or a host compiler. It is
therefore a Rust type check only. It never produces `ring`'s native objects. Those are
produced by the embed slot build in the real target slot (slice 6), whose archive members'
platforms the FFI-3 rows check.

The iOS crate set grows with the slices: the layout crate first, then the runtime closure
(the embed slot's crates), then the driver's embed and SDK modules.

**The embed slot.** `tools/toolchain-dist/product.toml` gains embed runtime rows for every
target and both iOS triples. Each is a Buck platform like `runtime-darwin`, built without
`standalone`. An iOS embed slot is built on Linux like the Darwin runtime archive, from the
locked Rust std, with the pinned clang for its C and assembly members.

**Qualification gates (Mac).** Two gates are separate from slice gates and need Apple
hardware.

### Simulator gate

The simulator gate is a GitHub-hosted `macos-26` job in `.github/workflows/mac-ci.yml`,
beside its existing macOS jobs.

**Triggers.** Pushes land directly on `main`, so the workflow gains a `push` trigger on
`main`, and the simulator job runs on it as well as on `pull_request`, `schedule` and
`workflow_dispatch`. The job's `if:` names those events, so a push to main is never skipped.

**Inputs.** A Linux job of the same workflow builds the installation (with the iOS and
macOS embed slots), runs `hirsel sdk swift` <!-- doc-ref: planned --> on the fixture
program (and builds the two-library equal-projection Swift packages and the EMB-6 symbol
reference fixture object), and uploads the generated packages, archives, headers and
fixture object as artifacts to the `macos-26` job. (The `macos-26` runner never executes an
ELF Linux driver; alternatively, it may build a native macOS driver and its Mach-O tools
in the same job and run `hirsel sdk swift` <!-- doc-ref: planned --> locally.) Either way,
the job never depends on artifacts from another workflow run.

**What it runs.**

1. **Receive or generate the package.** Unpack the Linux-generated Swift package, archives,
   headers and symbol reference fixture object (or generate them with the native macOS
   driver built in this job).
2. **Check the XCFramework against Apple's tool.** Run `xcodebuild -create-xcframework`
   over the same three archives and headers. Compare its `Info.plist` with the driver's,
   field by field: for each library, `LibraryIdentifier`, `LibraryPath`, `HeadersPath`,
   `SupportedArchitectures`, `SupportedPlatform` and `SupportedPlatformVariant`. Apple's
   tool is the independent oracle for the driver's assembly.
3. **Check the symbol lists and two-library Swift link.** Link the EMB-6 symbol reference
   fixture against the iOS SDK for the device and the simulator. Also compile two generated
   Swift packages with equal binding projections in release mode (`-O`) and link both
   against both lazy archives in both link orders (`A, B` and `B, A`), asserting a
   duplicate `hirsel_embed_v1_*` link error in both orders (EMB-1, EMB-5).
4. **Prepare a simulator.** Run `xcrun simctl list runtimes`, pin the simulator runtime it
   found (never a version written blind), and create a disposable simulator. Add the TLS
   test CA to that simulator's trust store (`xcrun simctl keychain <udid> add-root-cert`).
5. **Run the test.** Build a minimal SwiftUI host with an XCTest target that depends on the
   **generated package as SwiftPM resolves it**. Run it with `xcodebuild test` on the
   simulator. The test checks:
   - the start path, including the anchor and the projection fingerprint: the SHA-256 of
     the shipped `projection.json` equals the generated `manifest_fp` and the projection
     the linked library's `hirsel_embed_v1_manifest` gives;
   - a Hirsel call's decoded result, a call of each entry kind, and a subscription with
     two changes;
   - a room with a state, a mixed-Part message-then-leave, a first-`Full` Part, and a
     leave, matching `sdk/client/room.ts` / `sdk.md`;
   - abandon, Swift cancellation before issue, held at the C registration boundary, after
     issue, early outcome before C returns, and late cancellation handler safety;
   - close from a host thread, from inside a callback, and racing external/deferred close;
   - the EMB-7 TLS rows:
     - **on every executing host (simulator, macOS and signed device):** a `Trust.System`
       handshake to a public HTTPS host succeeds and an unanchored test CA gives
       `UnknownIssuer`; under `Trust.Roots` matching the test CA, the leaf succeeds and a
       wrong name, expired leaf and not-yet-valid leaf give their named variants; a `Roots`
       grant with another digest is `Denied`;
     - **on hosts deliberately provisioned with the private test CA** (the disposable
       simulator above, and macOS only when the CI job explicitly provisions trust for that
       test CA): the test CA's leaf also succeeds under `Trust.System` and gives
       `NameMismatch`, `Expired` and `NotYetValid` against it;
   - a stop and a second start, after which the old session handle returns
     `HIRSEL_STOPPED` and a new call succeeds;
   - the `Welcome.target` each slice reports.

   The same XCTest runs on the macOS slice without a simulator (with the private-CA-under-
   `Trust.System` assertion scoped to whether the macOS host provisioned that CA).
6. **Delete the simulator.**

Daytona macOS machines are for developing the Mac slices by hand. The standing gate is this
job, because it runs on every change and does not expire.

**Measurements.** The job also records measurements, not pass or fail, in its summary:

- the Swift-to-Hirsel call round trip: p50 and p99 over 1,000 calls of a trivial compute
  entry;
- the time from `start` to the first result;
- resident memory after `start`, physical and virtual separately;
- the time of a stop then a start;
- the worker and fiber counts.

It runs a **PR #11 comparison arm** too, recording start time and resident memory only.
PR #11's app is not a runner fixture: its entry is a renamed `main` (`hirsel_app_main`) run
on a pthread, and its host drives a `Foreign[ios.Ios]` event loop through `hios_host_*`
exports. So the comparison is defined by one shared program and one marker:

- **The program.** It is runnable by both pinned toolchains (this repository's, and
  PR #11 at `65a6f54` in a `/tmp` checkout, never pushed or commented on). It stores one
  counter durably and returns it.
- **The host adapter on each arm.** On the embed arm, the host calls the counter's
  transact entry. On the PR arm, it posts the same input as a lifecycle event, and reads
  the counter from the main-queue UI command that the program emits. PR #11's host-only
  bridge symbols stay in the PR checkout; no new general-purpose export is added for them.
- **The marker.** Both arms measure from process start to the first counter value seen on
  the main queue. They take resident memory at the same point. Each sample is a fresh
  process on the same simulator and configuration.
- **Durability check (outside the timing interval).** Before accepting either arm's
  fixture as the decided workload, each arm independently relaunches against its own
  same-container store after observing the acknowledged counter value and confirms that the
  persisted counter survives relaunch. This check runs outside the timed samples so neither
  adapter can pass by serving a counter from process memory alone.

PR #11 reserves 1 MiB per fiber stack and commits 256 KiB at first on iOS. The job records
those values next to Hirsel's. Slice 9 carries that sizing difference into its device check.
They are not adopted as a semantic limit.

### Device gate

The device gate is **not done**. It needs a physical iPhone and an A12 iPad at the 26.0
floor, and a signed build. gillesmag runs it on their own devices (Sam's ruling G3 = C), not
on Hirsel infrastructure. The supervisor asks for it in the PR #11 comment it posts after
the build. The run is reported with the device models, iOS versions and the build ID.

Until a signed device run passes it, iOS is labelled **simulator only** in reserved.md and
in release notes. That holds after the simulator gate passes, too. The run qualifies the
fixed 26.0 floor; it does not choose one.

**First law: confinement.** The test runs inside the signed app's own writable container,
so the OS sandbox cannot be what refuses:

1. The test creates `root/` and a sibling `outside/` in the same container, with a file in
   each. It first proves that the host itself can read and write `outside/` directly.
2. It grants `root/` as a `Dir`. Through the grant, a file inside `root/` opens.
3. A relative symlink inside `root/` that points inside `root/` is followed, where the
   operation follows links.
4. Through the grant, three legal relative names (passing Hirsel's public path check) whose
   symlink targets lead outside `root/` are refused with `Denied`, with the confined open
   reaching the kernel (`O_RESOLVE_BENEATH`) each time:
   - a legal relative name whose symlink inside `root/` has a relative `..` target pointing
     to `outside/`'s file;
   - a legal relative name traversing a symlink directory inside `root/` whose relative
     `..` target points to `outside/`;
   - a legal relative name whose symlink inside `root/` has an absolute target pointing to
     `outside/`'s file.

   On the kernel path, `ENOTCAPABLE` maps to `EXDEV` and then to `Denied`
   (`crates/hirsel-core-rt/src/host/darwin/path.rs`).
5. `outside/`'s file is unchanged afterwards.

The three public-boundary `Denied` assertions of the macOS law (a literal `..` parent
component, an absolute path, and a symlink escape) are kept as well; the language boundary
continues to reject literal `..` and absolute paths before any syscall, while the three
symlink cases above prove kernel `O_RESOLVE_BENEATH` enforcement on legal relative names.
A blanket symlink refusal, a sandbox `EACCES` or a lexical refusal does not pass the
kernel law.

A device that behaves otherwise stops the iOS track. The floor's premise (kernel
enforcement of `O_RESOLVE_BENEATH`) is then false, and the confinement design is reopened.
No userspace walk is built.

**Then** the same simulator test runs on device (using the unprovisioned-host TLS suite
above: public HTTPS `Trust.System` success, unanchored test CA `UnknownIssuer`, and
`Trust.Roots` leaf-variant checks, without installing a private test CA on the user's
device), followed by the runtime qualification that the simulator cannot give:

- **Stack depth.** A recursion of a stated depth runs on the A12, and the guard page is
  observed one frame past the bound.
- **Faults.** The host survives a guard fault (EMB-3), and debugger attachment does not
  break stack growth.
- **The floor and interfaces.** The floor's `O_RESOLVE_BENEATH` holds, and NET-18 works
  against the device's interfaces.
- **Clocks.** A deadline spans a suspension: the test records whether the clock counts
  suspended time (continuous) or only uptime.
- **Memory.** The virtual reservation that `n_place` takes is recorded, with the resident
  memory.
- **Recovery.** An app suspends and resumes with a call in flight. A relaunch in the same
  container finds every acknowledged commit.

iOS leaves [reserved](reserved.md) when this gate passes.

## Prior art

| Project | Mechanism | Verdict |
|---|---|---|
| UniFFI | Logic stays in the Rust core; generated foreign code is a thin typed layer over a C ABI of byte buffers; generated Swift ships beside a static library | ADOPTED: the client protocol stays in the runtime, and Swift is codec plus stubs. NOT TAKEN: its interface definition language, because the runner entries and codec schemas already are the interface |
| gomobile (`bind`) | Builds a `c-archive` per Apple slice and assembles an XCFramework; one Go runtime per process; exports only bound packages | ADAPTED: per-slice static archives and an assembled XCFramework, without `xcodebuild` and with renamed internals. One runtime per process, made a link error by the per-library anchor |
| Kotlin/Native | Exports a class graph to Objective-C with reference-counted handles that cross the boundary | NOT TAKEN: no object, closure or reference crosses the embed ABI. Only codec values and generation-checked value handles cross, so there is no cross-runtime lifetime |
| rustls-platform-verifier | On Apple targets, decides a chain by `SecTrustEvaluateWithError` with an SSL policy for the name, instead of reading roots; maps only a few Apple codes and reports the rest as other errors | ADAPTED for `Trust.System` on iOS (EMB-7): the same SecTrust decision, with the provider's own verification date and no network fetch. REPLACED: its partial mapping, by Hirsel's own leaf checks first and a total code table. The likely Darwin end state |
| Goose | `export fn` gives a C wrapper per function; `goose_init`/`goose_exit` start and end the program; the compiler writes the header (`--header` <!-- doc-ref: external -->) | ADAPTED: start and stop, and a toolchain-written header. NOT TAKEN: per-function C-typed exports, because Hirsel exports entries with stable schemas, not C signatures |
| Go `c-archive` | Installs its synchronous-signal handlers with `SA_ONSTACK`, gives each thread it creates its own alternate signal stack, and passes faults outside Go code to the handler it replaced | ADOPTED: one chaining fault handler installed with `SA_ONSTACK`, alternate signal stacks only on the runtime's own worker threads, and host threads untouched (EMB-3) |
| PR #11 | `/usr/bin/libtool` archive; `hirsel_app_main`; iOS FFI-3 admitting `minos` up to 28.0; CPU and triples written per site | REPLACED by the pinned `llvm-ar`, the EMB-4 ABI, the EMB-8 FFI-3 rule and the EMB-2 records. Its target-enum widening and front array sizing are taken as they are |
