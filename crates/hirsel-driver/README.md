# hirsel-driver

Compiler request graphs, source sessions, interpreted execution and host launch entry points.
`hirsel-front-phases` owns `FrontPhases`, `COMPILER`, `compiler_adapters()` and
source loading. `hirsel-native-queries` owns native source and instance producers;
`hirsel-native` owns validated stages, emission and linking; `hirsel-native-testing`
owns heap execution and differential matrices. `hirsel-host-io` owns interpreted
host effects. Candidate capture, observation views and package documentation live
in `hirsel-candidate`, `hirsel-observation-view` and `hirsel-project-info`.

Native builds and runs reserve a private scratch directory per build and remove
it on drop, including errors and unwinding. Set `HIRSEL_KEEP_NATIVE_SCRATCH=1`
to retain these directories for debugging; the driver prints each retained path.
A build or run with it set never uses the program record, so every retained
directory holds work that ran.

Source launch boundaries flush stdout before writing the interpreter's PROG-3 return-payload report. Ready programs retain a checked, layout-derived diagnostic view for the native entry wrapper. Host reports are separate from semantic observations and Io tapes. Native CLI launches inherit the OS output streams, preserving the host report and finalizer order; custom-writer launches retain per-stream forwarding.

`COMPILER` carries the compiler identity `hirsel-compiler-identity/build.rs` computes: a digest of the
sources and manifests of this crate and of every workspace crate it depends on,
with Cargo.lock and the toolchain pin. Std images are keyed by it, so any edit to
the compiler retires every std image written before it.

Fresh, no-daemon and resident checks, execution and TIR exports select immutable
`CompilerRequests`.
The source loader installs the admitted front, Lower, Check, Run and Object
producers, then drops its mutable engine before clients receive the graph.
`Session` retains input observations and host policy. Query products own their
admission and validity; the Session has no semantic program or invalidation cache.
Native build and run use the same Object producers and host launch binding.
Explicit engine APIs remain available to compiler fixtures and source bootstrap.

`hirsel_native_testing::backend_features` inventories LIR opcodes with operand representations,
arithmetic and conversion modes, projections, layout shapes, validation stage
outcomes and runtime calls in emitted O0 LLVM IR across both targets and all
lowering choices. It never links or executes the source. A stage failure is
returned separately; it is not evidence that the failed stage was covered.
Individual LIR-validator and heap-checker check kinds are not instrumented.

`hirsel_native_testing::pairwise_matrix` covers every feasible pair of dimension values in
its input matrix, including the matrix's constraints on runtime archives and
pass choices. A per-program rotating anchor visits every input configuration
within one matrix-length cycle of consecutive seeds. Pair coverage is not a
proof of equivalent bug-finding power: higher-order interactions are covered
only by rotation over time and the explicit exhaustive campaign.

`hirsel_native::Proofs` (`hirsel-native/src/proofs.rs`) is the `--proofs` consumer. It builds
this compiler's own export of the entry's closure, assesses the sidecar with
`hirsel_proof_export::sidecar`, and maps each accepted obligation's TIR sites to
a removal form in a `Discharge` plan carried by `Options::discharge`. Before it
plans, it requires the native snapshot's TIR of each planned definition to be
the exported TIR. `build_program` lowers with the plan and runs
`validate_discharge` on every discharged unit before the Value validator. Any
difference from the plan is an import validator error. `ReadyProgram.discharged`
lists the removed sites, which the proof report reads. Integer operators import
as unchecked `IntArith` (Neg as subtraction from zero); projection addresses
retain their path and mark only the licensed bounds/tag guards unchecked.
Append fusion carries source-site and canonical-place mappings through caller
rewrites and derived builders, so reports and evidence keep their original ids.

Extern calls (PROG-4): `ReadyProgram.foreign_links` are the root's `unsafe ffi` link
inputs, passed after the runtime archive (`-l<name>` or the file); the link accepts
`Foreign` imports beside runtime ones. `foreign_unreplayable` sets hirsel-rt's
`UNREPLAYABLE` bit beside the root mask, so the executable refuses `--hirsel-record` and
`--hirsel-replay`; H refuses an extern call as `Unsupported`.
`ReadyProgram.escaping` names the launch entries whose closure reaches an extern or
`Unsafe` operation (CAP-6, from `ValidatedNativeInput::unsafe_reach`); each sets
hirsel-rt's `ESCAPES` bit, so the executable refuses a record whose
`admission.code` is `admit.safe@1`.
`ReadyProgram.callbacks` names the definitions referenced as C callbacks; their
thunks live in the always-regenerated glue object.

Every native unit, including each library template and known callable body, owns
one retained object. Cranelift and LLVM O0 import their callees. Optimized LLVM
callers may include reached templates and small source callees
(`SMALL_UNIT_INSTRUCTIONS` or fewer) as `available_externally` optimizer bodies.
Copied bodies enter the caller's content key, and retained owners keep entries,
root adapters and callable thunks available independently of the caller set.
Workers lower each unit completely, with plan-derived finalizers and results in
plan order. `ReleaseLto` explicitly selects the whole-module path.

Allowed dependencies: hirsel-engine, the shared data crates, check-protocol, signature, check, modes, lower, run-config, core-contract, the native backends, and `object` (reading runner, image and stub ELF files, writing the runner note). Only cli, conformance and pipeline-tests (and the registry-backed harnesses of check and modes, as dev-dependencies) depend on this crate.

Native tool paths are explicit absolute inputs: `LLVM_SYS_231_PREFIX`,
`HIRSEL_LINK_KIT`, and for the targets and variants that use them
`HIRSEL_AARCH64_CC`, `HIRSEL_AARCH64_SYSROOT`, `HIRSEL_QEMU_AARCH64`,
`HIRSEL_VALGRIND`, `VALGRIND_LIB`, `HIRSEL_VALGRIND_RUNTIME`. The Buck2 test
launcher supplies them
from the pinned toolchains. Missing or relative paths refuse construction;
there is no PATH search or system LLVM fallback. Memcheck launches retain their
input library path when the child's environment is cleared.



Default links read the debug-stripped runtime archive. `hirsel build --debug`
links the debug-bearing one (`debug/libhirsel_rt.a` beside it) and moves the
finished executable's debug information into `<build-id>.debug` beside it,
named by its `.gnu_debuglink`. `native::test_path`'s dispatching `main` is LLVM
IR compiled in process, and its adapters are ordinary link inputs.

The kit also captures a separate static executable closure. It contains pinned glibc
2.36 archives and `crt1.o` on x86-64, and the pinned Arm GNU 14.3.rel1 sysroot's
glibc 2.40 archives and CRT on AArch64. Their minimum kernels are 3.2 and 3.7.
These static libcs include files/DNS resolution without loading host NSS modules;
the dynamic ABI floor remains glibc 2.28 on both targets. Static links group
the runtime, LLVM unwinder, foreign archives and libc archives with the pinned
compiler-rt `gcc_personality_v0.c` implementation. They never read host libgcc.
Frozen kits retain both closures within the existing file and byte bounds.
Ordinary native executables select this fixed-address static closure by default. Foreign
archive and relocatable object inputs stay static; explicit shared libraries and
linker scripts retain the closed dynamic runtime, as do executables launched with the explicit
memcheck loader. Launch and validation
inspect ELF program headers so static executable startup never runs through a
dynamic loader.

Production native import uses `hirsel-layout` for type-to-layout and ABI derivation.

`run_path_on_host` is the production interpreter launch. `bind_host_run` either validates
a record or template, checks the semantic revision and required roots with `admit_run` and,
opens the journal at the level the run record's `recording` member selects
(`hirsel_rt::host_launch::journal`: a streamed tape, or a durable journal, at the
`HostLaunch::Live { record: Some(tape) }` destination); or binds a `HostLaunch::Replay` tape's own run to the
checked revision, entry and roots and runs on `hirsel_rt::host_launch::replay`'s SimIo. It
then executes TIR with `network::HostLibrary`. This adapter translates every Io
projection, stdout and stderr writes, stdin and Env reads, and filesystem, clock and
network calls to the runtime's typed Request/Completion protocol, which owns grant
enforcement, in the order native code issues them, so one tape replays in either engine.
The interpreter has no host effects: live stdout and stderr are written by the runtime
adapter, and a replay copies its recorded output to the process's streams. HostLibrary
drives cooperative waits only through the adapter (`Adapter::enter`, `finish`,
`cancellable`, `parked`, `ready` and `wait`): at each scheduling point it asks `ready` once
for the parked requests to re-issue; live, RealIo asks its Readiness and records each park
and ready wait; in replay, SimIo parks and readies tasks exactly where the tape says, so a
run whose tasks interleave host requests by readiness replays its own schedule. Pure calls and
recorded Args/Env snapshots use the ordinary Library. `run_path`, `run_path_with_simulation` and `run_file` are the fake-provider
entry points for semantic fixtures. Production native runs pass the same validated record
to linking and launch; explicit host runs have streaming stdout and no harness wall-time limit.

`native::build_path_native` checks a source or manifest and writes a standalone
executable. Its entry adapter forwards nominal roots in source parameter order,
derived from the engine's shared `entry_roots` validation. The executable embeds
semantic program revision and entry identity; fresh launch grants and argv are
validated and admitted by the runtime, independently of compiler profile.

A sole Test entry selects test admission through the engine's shared
`entry_launch_kind`, for exported records, templates, live runs and replay in both
engines. Other valid entries select main admission.

`LaunchConfig` selects either a bound record or a grants template from an explicit
OS argument: a leading `{` selects inline JSON, otherwise it selects a path. Both source
engines resolve it against the checked semantic revision before host admission;
templates never pass through a temporary exported record. Native host launches pass
`--hirsel-record <tape>` or `--hirsel-replay <tape>` to the executable after the driver
has bound the run, so a recording of other code refuses before linking; a replay inherits
no stdin.

Native live launches inherit stdin only under the admitted stdin projection.
Their two output pipes drain concurrently into separate caller-owned writers,
while retaining the same bytes for native observations and launch diagnostics.

`run_path_with_step_budget` selects an optional simulation execution budget;
`run_path_on_host` receives the selected optional step bound for live and replayed
host runs. `None` means unbounded. CLI callers supply one billion for simulation
and replay, and no bound for admitted live host runs. `run_path`,
`run_path_with_simulation`, and interpreter
`Limits::default` retain their existing test/conformance budgets.

HostLibrary implements OperationsAdapter, whose forwarding implementation is generated
from the same inventory as Operations. Scheduler and cleanup hooks therefore cannot
silently inherit no-op defaults. Host sockets use CooperativeNet: parked operations
retain typed snapshots while the evaluator retains owners and mutable loans. A
`stdin.read` with no available byte parks its task under its token and resumes
through `Library::read_stdin`. A `clock.sleep_until` not yet due parks on a deadline-only
Readiness timer and is reissued when due; cancelling it finishes the token, removing the
timer, and returns `Canceled` without a host request. `random.bytes` draws from the
runtime's admitted provider and supplies the bytes to the library's semantic event. Socket
and stdin readiness, timers and relative timeouts are
polled at scheduling points; the scheduler waits only when no task is runnable. Effect-free waits observe level-triggered cancellation.

`LaunchConfig::Grants` can supply an explicit Args snapshot override, validated
and included in the completed record before admission. An absent override retains
the template's snapshot; an empty override grants empty Args.

`api_index` documents public declarations from an explicit package manifest,
using the declaration-only loader and compiler resolver for dependency links.
It retains source signatures, constructor payloads and attached doc comments,
without checking bodies or exposing opaque representations or internal modules.
Markdown and JSON share stable versioned definition-path anchors. std's manifest

Host timer waits expose their original absolute deadline to the interpreter's poll
ordering. Several overdue timers resume by deadline, with suspension order breaking
ties, identically in live recording and replay. A `task.scope_until` scope's deadline is
armed after the library enters the scope, as a `clock.sleep_until` request under a token
of its own that no cancellation reaches: a reached deadline completes at once and expires
the scope, otherwise the token parks on its Readiness timer. Every poll looks at the armed
tokens first, by deadline with ties in entry order, and a ready one's reissued wait
expires its scope; a scope that exited first has its token finished, recording nothing.
The compiled runtime (`hirsel-rt/src/host_waits.rs`) arms and polls the same requests, so
tapes replay in either engine. `HostLibrary` and H's `ScriptIo` use the runtime's
`io_checks` preflight before completing cancellation, so program errors win while
provider outcomes and elapsed deadlines remain after cancellation.

`testing` selects checked root-package `@test` functions, generates ordinary in-memory Test/Result launch adapters in each defining module, and runs independent interpreter contexts. `native::test_path` emits one executable with all selected adapters and starts one process per test; LLVM and Cranelift share selection, seeds and results. `testing::Schedules` selects each test's scheduler runs (the default, one `--scheduler`, or `--explore` seeds of a policy) and `testing::explore` stops a test at its first failing schedule, reporting its scheduler and run count; the interpreter takes the scheduler through `SimulationConfig::scheduler` and a native launch through `HIRSEL_RT_SCHEDULER`. Interpreted host runs (`HostLibrary::choose_task`) pass a seeded choice through the runtime adapter's `schedule`, so it is recorded and replayed like the native one.

Failure observations retain and compare the original operation site independently
of the primary attribution. Tooling reports render both complete locations.

Conformance `hirsel_native::Fixture` supplies a virtual monotonic start and validated scripted
network/process grants to `differential_ready` and `real_io_replay_ready`. H receives its
model world; LLVM and Cranelift use the same harness adapter. Production launch
records and capability seeds remain independent of semantic fixtures.

Simulation and host interpretation use the same `project_outcome` projection as
the engine; native launches return that same hirsel-run-config outcome, retaining
all failure causes and source-linked parked waits. Native observation admission
checks the actual process status against `RunOutcome::exit_code()` before an
observation may enter differential/agreement comparison.



`tests/it-stdlib/stdlib_table.rs` runs the typed-table storage fixtures through
the real store and checksum implementations: CRUD/reopen, limit atomicity,
version/reorder/rename history and shared representation renames. These exceed
the corpus step bound and run as bounded driver integration tests.

Native placement, RC insertion and elision, reuse and cleanup use one per-unit
lowering function. Object workers, heap inspection and dumps all use it;
production validates Ready once, and harnesses also check intermediate stages.
Finalizer facts come from the plan, so no stage retains every unit's RC output.
Native semantic binding checks every planned unit against the run identity's
closure. Derived append forms retain explicit source owners; planned library
bodies are covered by its shared runtime revision. Linking checks the sealed
unit selection and every exported function's definition again.

`Session::open_configured(entry, CompileConfig)` carries an explicit result-store
provider, worker count, retention, lowering and total hold budget through reloads.
`Session::open(entry, Policy)` keeps the ordinary Resident defaults under the
command's V and result-store choice (`Results`); the CLI selects `Results::Host`
for check, build, run and serve. Path entry points take the same `Policy`, and
the thread's session registry keeps sessions by working directory, entry and
policy, so no session is reused under another store choice. A configured
Compiler is Clone: clones share the selected provider and create separate engines.
Every thread keeps at least one session: a one-shot path entry point leaves its
compiled session on the thread after it returns, and a repeated request for the
same program reuses it. `keep_sessions(limit)` raises the per-thread limit
(`hirsel serve` keeps several for its clients) and drops the oldest sessions over
it. `opened()` counts the sessions the path entry points opened on this thread.

`hirsel_native::receipt_source::factory::NamedGraph::declarations` publishes receipt
source declarations only after reconstructing the graph against the caller's
current original roots and V provider. It retains complete profiles under the
aware request identity and preserves ordinary operation, source, descriptor and
invocation queries through its supplied declaration provider. Create a fresh
view for each current source selection, including native cache replay.
`queries::InstanceOperations` forwards these inputs without substituting a
candidate ABI or its source ordinals. This supplies independent C data; entry,
call, capture and task custody still require their executable proofs.
