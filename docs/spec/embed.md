# Embedding and Apple mobile targets

*Specified 2026-11-26 (lane ios-spec-3137, from the PR #11 review and the iOS prospect).
Rules EMB-1 to EMB-9. Not built: every rule here is a slice in the iOS track, and iOS stays
in [reserved](reserved.md) until its device gate passes (EMB-9).*

This specification adds three things that are general and one that is Apple's:

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

## EMB-1: Output kinds

A native build has an **output kind**: `Executable` (today's only kind) or `StaticLibrary`.
The CLI spells it `--emit exe` <!-- doc-ref: planned --> or
`--emit staticlib` <!-- doc-ref: planned -->, on
`hirsel build` and on every command that builds a native image; the default is `exe`.

```text
OutputKind = Executable | StaticLibrary
```

**What a static library holds.** A static library is the program's objects, the runtime
archive of the target's **embed slot** (EMB-3), its manifest (EMB-5) as read-only data
behind `hirsel_embed_v1_manifest`, and the reached foreign archives' members,
in one archive with one member per object, in the order the link would take them. It has
no `main` and no process entry. Every global symbol it defines is either an embed ABI
export (EMB-4) or carries the build prefix below. It is not linked: there is no final link, and the link kit's role for an
archive is the import audit below.

**Symbol hygiene.** A host process may link other Rust or C code, so every global symbol
that a member defines and that is not an EMB-4 export is renamed, in every member that
defines or references it, to `_hirsel_<b>_<name>` (Mach-O) or `hirsel_<b>_<name>` (ELF),
where `<b>` is the first 16 hexadecimal digits of the build key. A symbol that no member
defines (an import from the C library or a framework) keeps its name. The renaming is
one `llvm-objcopy --redefine-syms` <!-- doc-ref: external --> pass per member over one map
computed from all members' symbol tables, so a reference and its definition always get the
same new name. (Checked with the pinned llvm-objcopy 23.1.5 on an `arm64-apple-ios26.0`
object: the definition and its references are renamed, and `LC_BUILD_VERSION` is kept.) Two Hirsel libraries in one process therefore never share an internal
symbol; they do share the EMB-4 export names, so a host links one Hirsel library (EMB-4,
"One runtime per process").

**The archiver.** An archive is written by the installation's own `bin/llvm-ar` with
`rcsD` (deterministic: zero timestamps, owners and modes) and the target's format:
`--format=darwin` <!-- doc-ref: external --> for Mach-O targets and
`--format=gnu` <!-- doc-ref: external --> for ELF targets, as the foreign recipe already does
([foreign](foreign.md)). No host archiver is consulted, and `/usr/bin/libtool` is never
used. The installation ships `bin/llvm-ar` and `bin/llvm-objcopy` beside `bin/ld.lld`, and
the toolchain manifest records the SHA-256 of each as it records `ld.lld`'s, so a changed
archiver changes the toolchain identity and with it every key below.

**The import audit.** A static library is not linked, so a reference to a name that the
target's link kit does not export would only fail later, in the host's link. The driver
fails it first: every symbol left undefined across the archive's members (after the
renaming) must be exported by one of the target's link-kit stubs (EMB-6). A name that no
stub exports refuses the build with `UnlistedImport(name, member)`. This is the same list
an executable's link enforces, applied without a link.

**Cache keys.** The output kind is a field of every key that names a native product, so an
archive and an executable of the same program never answer for each other:

| Key (domain string) | Where | Change |
|---|---|---|
| `hirsel.core-build.v1` | the link build key (`crates/hirsel-core-driver/src/native_link.rs`, `build_key`) | becomes `.v2` with a field `output: OutputKind` after `link_mode`; `hirsel.core-executable.v10`, keyed over it, becomes `.v11` |
| `hirsel.recipe-final-link.v4` | the recipe final link (`crates/hirsel-core-driver/src/native.rs`, `link_objects`) | becomes `.v5`, keyed over `(assembly, output)` instead of `assembly` |
| `hirsel.incremental-link-frame.v2` | the resident compiler's link frame, from which it patches its last link (`crates/hirsel-core-driver/src/native.rs`, `crate::incremental_link`) | becomes `.v3` with a field `output` after `target`. A static-library build never patches: it rewrites the archive from its members, so the field also guarantees that no frame of one kind is ever read for the other |

The resident parse and symbol memos (`hirsel.resident-parse.v1`,
`hirsel.resident-symbols.v1`) are front-end products that do not depend on the output kind
and keep their keys. The launcher's `hirsel.assembly-record.v1` frame is not a product key:
the launcher runs only executables, and an archive never enters it. Old keys are not migrated: a build with the new keys misses and
rebuilds. The kind is a full field, never a suffix on a path or a flag folded into
`link_mode`: PR #11's recipe cache returned an executable for an archive request because
the kind was not in the key.

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

The file also holds one table, `cpu_by_floor`, giving for each platform family (`macos`,
`ios`) and floor the oldest CPU the floor admits:

| Family | Floor | CPU | Why |
|---|---|---|---|
| `macos` | `26.0` | `apple-m1` | macOS 26 runs on Apple silicon from M1 |
| `ios` | `26.0` | `apple-a12` | the iOS triples cover iPadOS, and iPadOS 26 is believed to run on A12 iPads (not confirmed against Apple's device list; A12 is the safe subset, since it lacks only DotProd relative to A13) |

**Derived values.** Each of these is computed from the record and nowhere written by hand:

- the LLVM and Cranelift triple: `arm64-apple-<llvm_os><floor>`, plus `-simulator` for the
  simulator (`arm64-apple-ios26.0-simulator`);
- the linker flag `-platform_version <platform_version_name> <floor> <floor>`
  <!-- doc-ref: external --> and the C compiler's `--target`;
- the `LC_BUILD_VERSION` `minos` that Hirsel's own objects carry, and the FFI-3 admission
  floor for foreign archive members (EMB-7);
- `Target::cpu()` and the Cranelift and LLVM feature sets, from `cpu_by_floor`;
- the SwiftPM `platforms:` entry and the XCFramework `Info.plist` rows (EMB-5);
- the deployment environment variable that the foreign C recipe sets.

**How the copies are generated.** `python3 tools/buck2/sync.py` <!-- doc-ref: planned -->
reads the record and writes the Rust table the layout crate includes
(`crates/hirsel-core-layout/src/apple_records.rs` <!-- doc-ref: planned -->) and the Starlark
constants the toolchain rules load, in
`tools/buck2/toolchains/apple.bzl` <!-- doc-ref: planned -->; shell recipes take the values as arguments from those rules. Its
`--check` <!-- doc-ref: external --> mode rejects a stale generated file, as it does for
`pins.bzl`. This replaces today's `DARWIN_FLOOR` copies and the `tools/check_target.py`
rule that ties them together. `sync.py` refuses a record whose `(family, floor)` has no
`cpu_by_floor` row, so a floor change can never leave the CPU baseline behind.

**Changing a floor.** For a floor that already has a `cpu_by_floor` row, the change is one
edit of `floor` and a regeneration. A new floor needs its row as well, because the oldest
admitted CPU is a fact about Apple's device support, not about the version number, and a
lower floor can admit an older CPU with fewer features.

**Why 26.0.** The macOS floor is 26.0 because it is the first release whose kernel enforces
`O_RESOLVE_BENEATH` (macOS decision D2). The iOS floor, for device and simulator alike, is
26.0 for the same reason: iOS runs the same XNU (`O_RESOLVE_BENEATH` and `ENOTCAPABLE` in
the shared kernel). `Dir` confinement on iOS is therefore the macOS code path, enforced by
the kernel; there is no userspace path walk. The device gate's first law (EMB-9) checks
that this holds on hardware.

## EMB-3: Embedded launch

An **embedded launch** runs a runner program inside a host process that the program does not
own. It is the [launch](launch.md) rule "An embedded runtime receives the digest, target and
`n_place` explicitly from its embedder before entry", made concrete.

**The embed slot.** Each target's toolchain carries a second runtime archive, the **embed
slot**, built from the same runtime crates without the `standalone` feature. Without
`standalone` the runtime does not own the process:

- it uses the host's allocator (the C library's `malloc`), never its own global allocator;
- it installs exactly one kind of signal handler, the fiber stack's fault handler for
  `SIGSEGV` (and `SIGBUS` on Darwin), at `start`, and that handler already passes every
  fault that is not on a guarded Hirsel stack to the handler that was installed before it
  (`crates/hirsel-core-rt/src/fiber.rs`). It never removes the handler (another library may
  have chained after it); after `stop` no Hirsel stack is guarded, so every fault passes
  through. It installs no alternate signal stack or Mach exception port, and ignores
  `SIGPIPE` per socket (`SO_NOSIGPIPE` on Darwin, `MSG_NOSIGNAL` on Linux), not
  process-wide;
- it never calls `exit`, `_exit` or `abort` for a program outcome: a place's `Fail`, its
  stop, or a launch refusal is reported to the host (EMB-4);
- it reads no environment variable, argument vector or inherited descriptor;
- its Rust code is built with `panic = "abort"`, so no unwind ever crosses the C boundary.
  A Rust panic in the runtime is a defect that ends the process, as it does standalone.

Fiber stacks grow by that handler: a fault below a stack's writable part makes the next
pages writable. So the handler is required, not optional, in every slot. A fault on the
guard page itself (stack exhaustion) is reported as it is standalone, as a place fault, not
by exiting. Whether the chained handler is sound on iOS, including under a debugger that
takes Mach exceptions first, is a device-gate question (EMB-9).

**What runs.** The embedded program is a runner program (it declares
`runner = { major = 1 }`), launched as one place with
`PlaceConfig.listen = InProcess(dials)` (RUNNER-SPEC §14.11), so it binds no socket. The
runtime keeps the place's `Dialer` and dials one connection per EMB-4 session. The host
supplies:

| Input | Meaning |
|---|---|
| `digest` | 32 bytes, or empty. The generated bindings pass the SHA-256 of the static library that the toolchain wrote (EMB-5). It is diagnostic only, as for every launch: the release identity is `revision`, `n_place` and the exports, never the digest. An empty digest is `Unavailable("embedded, not supplied")`. The runtime never hashes `current_exe` or the host's image. |
| `n_place` | The place size, as for every launch. |
| `grants` | The grant text, in the grammar `hirsel run` takes. A grant the target cannot honour was refused at build (EMB-7, EMB-8); a grant outside the program's declared wants is refused at start. |
| `store_dir` | An absolute path to the place's store directory, which the host creates (on iOS, inside the app container). The runtime opens it once, as the launcher does. |

The runtime copies every input before `start` returns, so later changes to the host's memory
cannot change the launch.

## EMB-4: The C embed ABI

**The export surface already exists.** Hirsel's export surface is the runner's entries
(RUNNER-SPEC §0 rule 2): compute, transact, query, granted and room entries, whose
arguments, results and errors are `codec` values, listed with their codec ASTs by the
Describe launch and the SDK [schema manifest](sdk.md#schema-manifest). The embed ABI adds no
export declaration, no new type rule and no second way to cross a boundary. It is a host
binding for the entries that a place already exports, as the TypeScript SDK is a WebSocket
binding for them.

**Shape.** The runtime contains the client side of the runner protocol (the work that
`client.ts` does over WebSocket: streams, positions, call queueing, subscription images and
rooms), so the C ABI is entry-level, not frame-level. A host sends codec bytes for an entry
and receives outcome bytes, subscription changes and room events through callbacks. A
generated binding (Swift in EMB-5, any later language the same way) is a codec and a set of
typed stubs over these calls, with no protocol logic of its own.

The runtime's client side is the same client kernel as the TypeScript client's, with many
calls in flight on one stream; it carries no limit of one call in flight per stream, so
concurrent `call`s from the host are pipelined, not serialized.

The header is written by the toolchain (`hirsel_embed.h`), and is the same for every program
and target at one ABI version:

```text
#define HIRSEL_EMBED_ABI 1

typedef struct hirsel_embed_runtime hirsel_embed_runtime;   /* opaque */
typedef struct hirsel_embed_session hirsel_embed_session;   /* opaque */
typedef struct { const uint8_t *ptr; size_t len; } hirsel_bytes;

typedef enum {
  HIRSEL_OK = 0,
  HIRSEL_ABI_MISMATCH = 1,   /* abi or manifest fingerprint differs from the library's */
  HIRSEL_REFUSED = 2,        /* launch refused: grants, store, n_place; reason in ended */
  HIRSEL_STOPPED = 3,        /* the runtime or session has ended */
  HIRSEL_UNKNOWN_ENTRY = 4,  /* want index out of range or wrong kind for this call */
  HIRSEL_BUSY = 5,           /* stop called from inside a callback */
  HIRSEL_ALREADY_RUNNING = 6, /* a second runtime in this process */
  HIRSEL_ID_IN_USE = 7       /* a call, sub or room id already live in this session */
} hirsel_status;

typedef struct {
  uint32_t size;              /* sizeof(hirsel_embed_callbacks) the caller was built with */
  void *context;
  void (*outcome)(void *context, uint64_t call, hirsel_bytes outcome);
  void (*change)(void *context, uint64_t sub, hirsel_bytes batch);
  void (*sub_ended)(void *context, uint64_t sub, hirsel_bytes reason);
  void (*room_event)(void *context, uint64_t room, hirsel_bytes event);
  void (*session_ended)(void *context, hirsel_bytes reason);
} hirsel_embed_callbacks;

typedef struct {
  uint32_t size;              /* sizeof(hirsel_embed_config) the caller was built with */
  uint32_t abi;               /* HIRSEL_EMBED_ABI the caller was generated for */
  hirsel_bytes manifest_fp;   /* SHA-256 of the binding projection (EMB-5) */
  hirsel_bytes digest;        /* 32 bytes or empty (EMB-3) */
  uint64_t n_place;
  hirsel_bytes grants;        /* UTF-8 grant text */
  hirsel_bytes store_dir;     /* UTF-8 absolute path */
  void *context;
  void (*ended)(void *context, uint32_t code, hirsel_bytes reason);
} hirsel_embed_config;

uint32_t hirsel_embed_v1_abi(void);
hirsel_bytes hirsel_embed_v1_manifest(void);
hirsel_status hirsel_embed_v1_start(const hirsel_embed_config *, hirsel_embed_runtime **);
hirsel_status hirsel_embed_v1_open(hirsel_embed_runtime *, const hirsel_embed_callbacks *,
                                   hirsel_bytes credential, hirsel_embed_session **);
hirsel_status hirsel_embed_v1_call(hirsel_embed_session *, uint64_t call, uint32_t want,
                                   hirsel_bytes args, uint32_t deadline_ms);
hirsel_status hirsel_embed_v1_abandon(hirsel_embed_session *, uint64_t call);
hirsel_status hirsel_embed_v1_subscribe(hirsel_embed_session *, uint64_t sub, uint32_t want,
                                        hirsel_bytes args);
hirsel_status hirsel_embed_v1_unsubscribe(hirsel_embed_session *, uint64_t sub);
hirsel_status hirsel_embed_v1_room_join(hirsel_embed_session *, uint64_t room, uint32_t want,
                                        hirsel_bytes args);
hirsel_status hirsel_embed_v1_room_send(hirsel_embed_session *, uint64_t room,
                                        hirsel_bytes message);
hirsel_status hirsel_embed_v1_room_leave(hirsel_embed_session *, uint64_t room);
void hirsel_embed_v1_close(hirsel_embed_session *);
hirsel_status hirsel_embed_v1_stop(hirsel_embed_runtime *);
```

**Ids.** The host chooses each call, subscription and room id, unique among that session's
live ones (`HIRSEL_ID_IN_USE` otherwise), so an id is known before any callback can name it:
an outcome may be delivered on the delivery thread before `call` returns. An id becomes
free once its final callback has run (`outcome`, `sub_ended`, or the room's final event) or
once `abandon`, `unsubscribe` or `room_leave` returns.

**Refusal at start.** When `start` refuses, it calls `config.ended` with the code and reason
on the calling thread before it returns `HIRSEL_REFUSED`; this is the only callback that
runs on a host thread, and the reason bytes are borrowed for that call. After a successful
`start`, `ended` runs once on the delivery thread when the runtime ends for any reason.

**Entries and bytes.** `want` is the entry's index in the manifest's `entries` array, which
is the Want list of the generated bindings. `args` are the entry's argument codec bytes. An
`outcome` is the runner's `Outcome` codec bytes for that call (Returned, Failed, Refused and
the rest, as the TypeScript client reports them). A `change` is one applied Batch of the
subscription's image (its slots' rows, keyed by ordered-key bytes, as `subscribeDecoded`
sees them). A room event is the room's message or state codec bytes, tagged as the
TypeScript `room` handlers are. The runtime decodes nothing the host sends beyond checking
the codec bytes against the entry's schema; a malformed argument is a `Refused` outcome,
never a fault.

**Ownership.** Every `hirsel_bytes` the host passes is borrowed for the duration of the call
and copied by the runtime before the call returns. Every `hirsel_bytes` the runtime passes to
a callback is borrowed for the duration of the callback; a host that keeps it copies it.
`hirsel_embed_v1_manifest` returns bytes that live as long as the process. Nothing crosses
that the other side must free, so the ABI has no free function.

**Threads and callbacks.** The runtime runs its own worker threads. Every callback of one
runtime runs on one runtime-owned **delivery thread**, one at a time, in the order the
runtime produced them; the host must not block it for long, and the generated bindings only
resume a continuation or yield to a stream there. Every ABI function may be called from any
host thread, including from inside a callback, with one exception: `stop` from inside a
callback returns `HIRSEL_BUSY`, because stop waits for the delivery thread. No callback for a
session runs after `close` of that session returns, and no callback of a runtime runs after
`stop` returns. Hirsel never calls into host code synchronously and never waits for a host
answer: there are no host-implemented entries and no reverse calls. Push to the host is a
subscription change or a room event.

**Cancellation.** The runner protocol has no per-call cancel frame, and this ABI adds none.
`abandon(call)` withdraws the host's interest: once it returns, that call's `outcome`
callback never runs, and the bindings resolve the caller's task as cancelled. The place
still runs the call to its outcome or its `deadline_ms`, and a transact entry that commits
stays committed, exactly as for a TypeScript caller that stops awaiting. `unsubscribe` and
`room_leave` send the protocol's own Unsubscribe and leave. `close` ends a session (and its
calls, subscriptions and rooms); `stop` stops the place and ends every session.

**Sessions and recovery.** A session is one dialled connection with one stream. Its client
state is in memory: an embedded place and its client share one process, so there is no
reconnect, and the durable recovery that `client.ts` keeps in IndexedDB or files has nothing
to recover from. A process that dies with a transact call in flight leaves its outcome
unreported; the next launch reads the committed state through queries.

**One runtime per process.** `start` returns `HIRSEL_ALREADY_RUNNING` while a runtime of this
library is running in the process. After `stop` returns, `start` may run again. A host links
one Hirsel library: two would define the same `hirsel_embed_v1_*` exports and fail the host's
link with a duplicate symbol, which is the intended diagnostic.

**Versioning.** `HIRSEL_EMBED_ABI` is the ABI version, and every export's name carries it
(`hirsel_embed_v1_`). An additive change (a new function, a new status code, a new callback
appended to a struct whose size is passed) keeps the version. Any other change is a new
version with new names, so a host generated for one version never calls another version's
function by mistake. Each struct begins with its `size`; the runtime reads only the fields
that size covers and treats later ones as absent, so a field appended in a later revision of
the same version is additive. At `start` the runtime checks two things, and refuses with
`HIRSEL_ABI_MISMATCH` before any work if either differs: `config.abi` equals its own
version, and `config.manifest_fp` equals the SHA-256 of the binding projection (EMB-5) of
the manifest that `hirsel_embed_v1_manifest` returns. The second catches bindings generated
from another build of the program, whose Want list would index other entries.

## EMB-5: Swift bindings and the XCFramework

`hirsel sdk swift <program> --output DIR` <!-- doc-ref: planned --> builds the program as a
static library for every Apple target in the records (EMB-2), generates Swift bindings and
assembles a Swift package. It takes the build options `hirsel sdk ts` takes.

**The manifest.** Every static-library build computes its program's manifest and embeds it
(returned by `hirsel_embed_v1_manifest`); `sdk swift` reads it from the libraries it built.
A static library cannot run a Describe launch on the build host, and an iOS one cannot run
there at all, so the build runs the Describe launch on the evaluator with the library's
target's toolchain constants (PROG-6). The embedded manifest is the sdk.md format with
`launch.target` the library's target and `launch.digest` empty (a library cannot contain
its own digest; the digest is the generated bindings' constant, EMB-3).

That the evaluator's Describe gives the native Describe's entries, codecs and fingerprints
is a decision of this spec, not a fact checked here: both run the same registration code.
Slice 3 carries the law that decides it (on Linux, the evaluator manifest's binding
projection equals the native Describe manifest's). If it fails, the static-library kind
waits until it holds.

The **binding projection** of a manifest is the manifest without `launch` and without each
entry's `revision`: the part a binding depends on (entries, their kinds, identities, codecs,
fingerprints, slots and rooms, and the tables). Its canonical bytes (sdk.md's compact sorted
JSON) are hashed for `manifest_fp`. `sdk swift` requires the binding projections of all its
libraries to be byte-identical; if a program registers different entries or codecs on
different Apple targets, it refuses with `TargetDependentEntries(target_a, target_b)` and
writes nothing. It writes the common projection's manifest as `manifest.json`.

**What it writes.**

```text
DIR/
  Package.swift                      swift-tools-version 6.0; platforms from EMB-2
  manifest.json                      the SDK manifest (sdk.md), as sdk json writes it
  Sources/<Name>/Codec.swift         codec for every type in the manifest
  Sources/<Name>/Entries.swift       typed stubs, Want list, manifest fingerprint, digests
  Sources/<Name>/Runtime.swift       Runtime and Session over the C ABI
  <Name>Core.xcframework/
    Info.plist
    macos-arm64/lib<name>.a
    macos-arm64/Headers/hirsel_embed.h, module.modulemap
    ios-arm64/lib<name>.a
    ios-arm64/Headers/...
    ios-arm64-simulator/lib<name>.a
    ios-arm64-simulator/Headers/...
```

`<Name>` is the program's package name in UpperCamelCase. The XCFramework is assembled by the
driver, not by `xcodebuild`: it is a directory of the three archives, their headers and
module maps, and an XML property list naming each library's `LibraryIdentifier`,
`LibraryPath`, `HeadersPath`, `SupportedArchitectures` (`arm64`), `SupportedPlatform` and,
for the simulator, `SupportedPlatformVariant` = `simulator`, with keys sorted and no
timestamps, so it is a pure function of its inputs. The simulator slice is arm64 only; an
x86_64 simulator is not a target. `Package.swift` declares a binary target for the
XCFramework and a source target for the Swift files, with `platforms` from the records
(`.iOS("26.0")`, `.macOS("26.0")`).

**The Swift surface.** It follows the TypeScript stubs (sdk.md section 4) with Swift types:

- Integers are Swift integers of the same width; `Bytes` is `Data`; `String` is `String`;
  records are structs and sums are enums with associated values; `Option` is `Optional`
  only when unnested, and a nested Option is a generated enum, so `Some(None)` survives;
  `Result` is a generated enum, not Swift's `Result`, because the error type need not be an
  `Error`. Every generated type is `Sendable` and `Equatable`.
- `Runtime.start(grants:storeDirectory:nPlace:)` passes the generated `abi`, projection
  fingerprint and the running slice's library digest (one generated constant per slice,
  chosen by `#if targetEnvironment(simulator)` and `#if os(...)`); `runtime.open(credential:)` returns a
  `Session`; `runtime.stop()` is `async`.
- `api.call.<module>.<name>(args, deadline:) async throws -> Outcome<R, E>`. Task
  cancellation calls `abandon` and throws `CancellationError`.
- `api.subscribe.<module>.<name>(args) -> AsyncThrowingStream<Change, Error>`; ending the
  stream's iteration calls `unsubscribe`. Rooms are an `AsyncThrowingStream` of events plus
  `send`.

**Where it lives.** The header, the generator, the XCFramework assembly and the
`Package.swift` generation live in the Hirsel repository, in the driver beside
`hirsel sdk ts`, because each is a function of facts Hirsel owns: the manifest, the ABI
version and the target records. The application, its Xcode project, signing, entitlements
and the device run live in the host repository (`gillesmag/hirsel-ios`). This settles
DECISIONS 2026-11-26 20:3xZ ("target + runtime + output mode live in the Hirsel repo; Swift
host/Xcode glue stays in gillesmag/hirsel-ios"): the Swift glue that is generated from a
manifest is Hirsel's; the Swift an application author writes is the host repository's.

## EMB-6: Link-kit rows for iOS

The link kit (`tools/native-toolchain/README.md`, "Link kit") gains one TBD v4 stub set per
iOS variant, generated from Hirsel-authored symbol lists, as the macOS rows are. Nothing is
copied from an Apple SDK, and no Apple file is shipped.

- Lists: `darwin/<library>.symbols` stays the macOS list; the iOS lists are
  `ios/<library>.symbols` <!-- doc-ref: planned -->, shared by both iOS variants. Each starts as
  the macOS list minus what EMB-7 refuses or replaces on iOS (process creation,
  `proc_pidinfo`, `pthread_fchdir_np`, the routing socket and the macOS Keychain root
  read), plus the SecTrust names `Trust.System` uses on iOS (EMB-7). Adding a name is a
  reviewed one-line change, as for macOS.
- Stubs: `libSystem.tbd` with install name `/usr/lib/libSystem.B.dylib`, and
  `CoreFoundation.tbd` and `Security.tbd` with their framework install names, each with
  `targets` `arm64-ios` (device) or `arm64-ios-simulator` (simulator). As on macOS, every
  stub is linked: the generated `module.modulemap` links the `CoreFoundation` and `Security`
  frameworks for every program (both ship on every iOS device), so the host app needs no
  per-program framework list.
- The kit's manifest row for each variant records its floor from EMB-2, and the kit digest
  enters every iOS build key as it does for other targets.
- The import audit (EMB-1) checks a static library's undefined symbols against these stubs.

Zig's `libSystem.tbd`, against which the macOS list is checked, lists no iOS targets, so the
iOS names cannot be checked against it on Linux. They are checked on the Mac day: the
simulator gate (EMB-9) links a test host against the archive with Apple's linker and fails on
any name the iOS SDK does not export.

## EMB-7: Capabilities on iOS

Every capability either has an iOS provider or is refused at the earliest point that can
tell. Two points exist:

- **At build, by reach** (PROG-6, `target.unsupported(why)`), where the capability's
  operations are their own provider functions, so only a program that calls them reaches
  them: `Exec` and the Unix-domain operations. Their iOS arms call `target.unsupported`.
- **At embedded start, by grant**, for every refused capability: `start` refuses
  (`HIRSEL_REFUSED`) a grant with an `Exec` grant or a `Unix` rule. A program that never
  reaches the operation can still be handed such a grant, and the refusal says so before
  any work rather than leaving an unusable grant in place.

A refusal by reach fits only an operation that is its own provider function. A capability
chosen by a run-time value (such as `Trust`, `System | Roots(b)`) is retained by every
program that uses the shared operation, so it cannot be refused by reach; on iOS every such
capability is provided.

Inherited listeners are not a refusal: the standalone entry takes them
(`crates/hirsel-core-rt/src/entry.rs`) and the embedded launch never runs that entry, so an
embedded program sees exactly what a standalone program started without activation sees.

| Capability | iOS | Rule amended |
|---|---|---|
| `Exec` (spawn, wait, signal) | refused at build by reach and at start by grant: "iOS apps cannot create processes". An iOS app has no `posix_spawn` or `fork` that App Review admits. | [exec](exec.md) |
| Inherited listeners (`LISTEN_FDS`, socket activation, raw TCP adoption) | none inherited: the embedded launch takes no descriptor (EMB-3), as a standalone run without activation | [launch](launch.md) |
| Unix-domain sockets (NET-12 to NET-14) | refused at build by reach and at start by grant: "Unix socket naming needs `pthread_fchdir_np`, which iOS does not offer". The macOS provider's directory-relative naming has no public iOS equivalent. | [net](net.md#unix-domain-sockets) |
| NET-18 public-only connect | provided: before every public-only connect the provider takes a fresh `getifaddrs` snapshot, after the connect is requested and before it is issued, and classifies the destination against it. There is no change source on iOS, so there is no cached table. | [net](net.md#net-18-public-only-connects) |
| `Trust.System` | provided by Apple's platform verifier (below). `Trust.Roots(pem)` is provided as on every target. | [tls](tls.md) |
| Launch digest | the embedder's value (EMB-3), else `Unavailable`. Never `_NSGetExecutablePath` or `proc_pidinfo`. | [launch](launch.md) |
| TCP connect and listen, UDP, DNS, clocks, files under granted directories, the store | provided, by the Darwin providers. They use only public BSD and Mach interfaces. The device gate qualifies each. | none |

**`Trust.System` on iOS.** iOS gives an app no readable system root store; its public route
to the system roots is trust evaluation. So the iOS `System` verifier is Apple's platform
verifier, as `rustls-platform-verifier` uses it on Apple targets. For a `System`
handshake the provider:

1. builds a `SecCertificate` for each certificate the peer sent, leaf first, and a
   `SecTrust` over them with `SecPolicyCreateSSL(true, name)`, where `name` is the TLS-1
   verified name (for an IP literal, the address text, which SecTrust matches against IP
   subject alternative names);
2. sets the verification date to the provider's own wall clock (`SecTrustSetVerifyDate`),
   as tls.md requires of every verification, and forbids network fetches
   (`SecTrustSetNetworkFetchAllowed(false)`), so verification does no I/O of its own and,
   as tls.md states, revocation is not checked;
3. accepts the chain if and only if `SecTrustEvaluateWithError` returns true. That call
   blocks, so it runs where the provider's other blocking host calls run, never on the
   delivery thread or a host thread that called into the ABI. A false
   result is the TLS provider's existing certificate-verification failure, with Apple's
   error code in the diagnostic.

The handshake's signature checks, the TLS-1 name rule (the certificate must be valid for
`name`) and the rest of the TLS provider are unchanged; only the chain-to-root decision is
Apple's. User-installed and MDM-installed roots that the device trusts are therefore trusted,
which is what `System` means: the host's root store. `Roots(b)` never consults SecTrust.

macOS keeps its current `System` provider (the Keychain root read through
`rustls-native-certs`) in this track. The platform verifier is the likely end state for
every Darwin target, so that both use one decision procedure and the macOS root read goes;
that unification is a separate follow-up outside the iOS track.

## EMB-8: Refusals

**Invariant.** Main never accepts a build request that it cannot fulfil hermetically. Every
request that cannot be fulfilled is refused at one of these points, before any work it would
waste:

| Refusal | Where | Kind |
|---|---|---|
| target named in the wrong spelling (`arm64-apple-ios`, `aarch64-apple-ios18.0`) | request validation, where `--target` is resolved | unknown target, listing the accepted triples |
| (target, output kind) not in the EMB-1 table, such as `--emit exe` <!-- doc-ref: planned --> for an iOS target | request validation, before installation lookup | `Unsupported(target, kind, why)` |
| no embed slot for the target in the installation | installation lookup | `MissingRuntime(target, "embed")`, as a missing runtime slot is today |
| reached `Exec` or Unix operation on iOS (EMB-7) | front, by reach | PROG-6 `target.unsupported(why)` with the reaching path |
| foreign archive member for the wrong platform or a later floor | FFI-3 admission, before the link or archive | the FFI-3 member diagnostic (EMB-2's `macho_platform` and `floor`) |
| import that no link-kit stub exports | the import audit (EMB-1), before the archive is written | `UnlistedImport(name, member)` |
| bindings whose manifests differ between Apple targets | `hirsel sdk swift` <!-- doc-ref: planned -->, before output | `TargetDependentEntries(a, b)` |
| grant outside the program's wants, a grant for a capability EMB-7 refuses on the target, unopenable store, ABI or manifest mismatch | the embedded launch's `start` | `HIRSEL_REFUSED` or `HIRSEL_ABI_MISMATCH` with the reason |

The last row is the only one after the build, and it is about the host's inputs (its grants
and store), not the build's.

**FFI-3 on iOS.** The Mach-O member rows of [FFI-3](foreign.md) apply to both iOS targets
with the platform and floor from the record: a member's `LC_BUILD_VERSION` must name the
target's `macho_platform` (2 for the device, 7 for the simulator; a device member is refused
for the simulator and the other way round) and a minimum version no later than the target's
`floor`. A member built for a later minimum is refused: it could use an interface the floor
lacks. PR #11 admitted `minos` up to 28.0, which inverted this rule.

## EMB-9: Gates and std provisioning

**The Apple check.** iOS has no host on Linux, so a slice's gate is `cargo check` of the
shipped feature set for both iOS triples, as `aarch64-apple-darwin` is checked today.
`tools/apple_check.py` <!-- doc-ref: planned --> replaces the Darwin-only check script with
one table of `{triple: crates}`. It checks with the toolchain's own `rustc` and a sysroot
that the Rust bootstrap provisions: `tools/buck2/toolchain-lock.json` gains `rust-std-ios` and
`rust-std-ios-sim` rows (hermeticbuild rust-redist, at the toolchain's Rust version), and
`tools/buck2/bootstrap_rust_toolchain.py` installs their `lib/rustlib/<triple>/lib`, as it
does `rust-std-darwin`. It sets `CC_<triple>`, `CXX_<triple>` and `AR_<triple>` to `true`
and `CRATE_CC_NO_DEFAULTS=1`, so no build script runs `xcrun` or a host compiler. The iOS
crate set grows with the slices: the layout crate first, then the runtime closure (the
embed slot's crates), then the driver's embed and SDK modules.

**The embed slot.** `tools/toolchain-dist/product.toml` gains embed runtime rows for every
target and both iOS triples, each a Buck platform like `runtime-darwin`, built without
`standalone`. An iOS embed slot is built on Linux like the Darwin runtime archive, from the
locked Rust std.

**Qualification gates (Mac).** Two gates are separate from slice gates and need Apple
hardware:

1. **Simulator gate**: a GitHub-hosted `macos-26` job in `.github/workflows/mac-ci.yml`,
   beside its existing macOS jobs, on every change. It runs `hirsel sdk swift` <!-- doc-ref: planned -->
   on the fixture program; then runs `xcodebuild -create-xcframework`
   over the same three archives and headers and checks that the driver's XCFramework is
   equivalent (same libraries, identifiers, platforms and variants in `Info.plist`), so
   Apple's tool is an independent oracle for the driver's assembly; then builds a minimal
   SwiftUI host with an XCTest target that depends on the generated package and runs it
   with `xcodebuild test` (or `xcrun simctl`) on a disposable simulator. The test starts
   the runtime, asserts a Hirsel call's decoded result, exercises a call of each entry kind,
   a subscription with two changes, abandon and close, a `Trust.System` handshake to a
   public HTTPS host, then stops the runtime, starts it again and asserts a call again. The
   job runs `xcrun simctl list runtimes` and pins the simulator runtime it found, never a
   version written blind. Its links against the iOS SDK check the EMB-6 symbol lists. The
   same XCTest runs on the macOS slice without a simulator. Daytona macOS machines are for
   developing the Mac slices by hand; the standing gate is this job, because it runs on
   every change and does not expire.
   The job also records, as measurements and not pass or fail: the Swift-to-Hirsel call
   round trip (p50 and p99 over 1,000 calls of a trivial compute entry), the time from
   `start` to the first result, resident memory after `start`, and the time of a stop then
   a start.
2. **Device gate** (not done; needs a physical iPhone and iPad at the floor, and a signed
   build). It runs on a contributor's own devices, not on Hirsel infrastructure, and its run
   is reported with the device models, iOS versions and the build ID. Until a signed device
   run passes it, iOS is labelled **simulator only** in reserved.md and in release notes. Its first law:
   a granted `Dir` refuses an escape by `..`, by a symlink pointing outside it and by an
   absolute path, each with the error that maps to the shared confinement result, exactly as
   on macOS. A device that behaves otherwise stops the iOS track: the floor's premise (kernel
   enforcement of `O_RESOLVE_BENEATH`) is then false and the confinement design is reopened.
   Then the same simulator test on device, plus the runtime qualification the simulator cannot give: guard-page
   behaviour and stack depth on the oldest admitted CPU, the floor's `O_RESOLVE_BENEATH`,
   NET-18 against the device's interfaces, clocks across suspension and sleep, and the
   virtual-memory reservation `n_place` takes. iOS leaves [reserved](reserved.md) when this
   gate passes.

## Prior art

| Project | Mechanism | Verdict |
|---|---|---|
| UniFFI | Logic stays in the Rust core; generated foreign code is a thin typed layer over a C ABI of byte buffers; generated Swift ships beside a static library | ADOPTED: the client protocol stays in the runtime and Swift is codec plus stubs. NOT TAKEN: its interface definition language, because the runner entries and codec schemas already are the interface |
| gomobile (`bind`) | Builds a `c-archive` per Apple slice and assembles an XCFramework; one Go runtime per process; exports only bound packages | ADAPTED: per-slice static archives and an assembled XCFramework, without `xcodebuild` and with renamed internals; one runtime per process, enforced by duplicate export names |
| Kotlin/Native | Exports a class graph to Objective-C with reference-counted handles that cross the boundary | NOT TAKEN: no object handle, closure or reference crosses the embed ABI; only codec values, so there is no cross-runtime lifetime |
| rustls-platform-verifier | On Apple targets, decides a chain by `SecTrustEvaluateWithError` with an SSL policy for the name, instead of reading roots | ADAPTED for `Trust.System` on iOS (EMB-7): the same SecTrust decision, with the provider's own verification date and no network fetch; the likely Darwin end state |
| Goose | `export fn` gives a C wrapper per function; `goose_init`/`goose_exit` start and end the program; the compiler writes the header (`--header` <!-- doc-ref: external -->) | ADAPTED: start and stop, and a toolchain-written header. NOT TAKEN: per-function C-typed exports, because Hirsel exports entries with stable schemas, not C signatures |
| Go `c-archive` on iOS | Runtime skips its usual signal setup on iOS | REPLACED: the embed slot installs no handler at all, and the device gate decides whether one is sound |
| PR #11 | `/usr/bin/libtool` archive; `hirsel_app_main`; iOS FFI-3 admitting `minos` up to 28.0; CPU and triples written per site | REPLACED by the pinned `llvm-ar`, the EMB-4 ABI, the EMB-8 FFI-3 rule and the EMB-2 records. Its target-enum widening and front array sizing are taken as they are |
