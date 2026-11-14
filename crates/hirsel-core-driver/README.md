# Core driver

`hirsel-core check <package-dir> --libraries core --store <private-store>` reads
new-language `.hirsel` sources. Relative file paths name modules in `app@1`;
`core/core` and `core/std` name `core@1` and `std@1`. `--inputs <json>` instead accepts
an explicit resolver `CorpusManifest`. Directory checks select `app@1/main/main` as
the entry; `--entry <full-definition-path>` selects another. Library manifests have
no implicit entry. Entry selection is a cache input because main is always waits.

The serial request loads and hashes sources once, parses, builds the complete name
index, resolves once, publishes declarations/layout recipes and checks/lowers each
body once. Each body owns its CoreIR module and signature-only callee rows. Syntax,
name rows, successful bodies and diagnostics move into shared in-memory products.
One buffered binary archive per request writes their borrowed tables once, including
native CoreIR without text reconstruction. The stream is hashed once and committed
by atomic rename before its root selection. Disk reads check version, length and digest.
Resolver scratch is not retained or encoded; definition keys retain its observed
lookup fingerprint and transitive signature reads. The root commits last.
No old std products, old requests, TIR or independent checker enter this path.

Warm memory or disk snapshots perform no source checking. Edited requests reuse
unchanged definition products, including refusals. Products are trusted in memory.
External disk records check format version, exact length and content digest; damage
is a cache miss. Immutable contents publish before the root. No old requests,
old std products, TIR or independent checker enter this crate. The workspace
layering audit enforces its new-core-only dependency boundary.

`hirsel-core image --libraries core --store <image-dir>` checks library sources
through that same request path and publishes an immutable image. Rejected or
incomplete source never produces an image. `//:hirsel-core-image` is the Buck target;
it invokes this new producer, independently of old precompute. Its success depends
on the canonical signatures being supported by the checker. Check an application
with `--image <image-dir>` to bind shared declaration/type rows and a request overlay.
Image open reads a fixed compiler/source/directory root. Name, signature, type,
summary, anchor and body demands follow content-addressed hash indexes; their
bounded leaves decode only the requested rows. Missing names are represented by
those immutable indexes, so a damaged index is an IO cache miss. Signature
imports neither read library sources nor decode callee bodies. Image byte damage
is reported as a cache miss requiring an image rebuild.

Native `build` and `run` use published core and std packages by default. The
compiler build embeds the root library sources and freezes their digests. The
shared package cache is keyed by source selection and compiler identity. A miss
produces the package once; later builds parse, resolve and check only application
sources. Neither core/tests nor std/tests is a dependency module directory. Directory
checks, source-library checks, and image production exclude those entire test
subtrees, regardless of fixture headers. Contract runners select their files
through explicit manifests. Application test directories keep ordinary discovery.
Std contract-test files are not installed library sources. Explicit
canonical manifests preserve exactly their supplied std module subset, even when
the installed toolchain contains newer modules.

The cache is `$XDG_CACHE_HOME/hirsel/core-packages`, or
`$HOME/.cache/hirsel/core-packages`. `core-product --store <cache>` and
`std-product --store <cache>` explicitly produce/reopen the corresponding
package. `--core-product <cache>` selects that cache and enables package
consumption for `check`; ordinary package `check` still checks everything given.
`--source-std` checks std sources while keeping core published; `--source-core`
checks the supplied library sources. In a checkout these controls use its `core`
directory; `--libraries` supplies another source root. Installed product builds
need no extracted library source files.

The same producer and format contain declarations, summaries/layout recipes,
checked generic templates and nongeneric CoreIR for both libraries. Stale source
stamps and foreign compiler products miss and rebuild. Custom source manifests
remain source inputs rather than gaining bundled declarations. Compiler/source
changes require rebuilding the toolchain's embedded digests. Non-generic reached
bodies retain their published CoreIR by reference; only generic templates are
mechanically specialized. Incomplete reached bodies still refuse assembly.

`hirsel-core-request-cost` measures the native request through check/lower and
publication, before instance collection, engine emission and linking. Its optional
instance-details output measures collection separately, including body loads,
generic specialization and dependency discovery. It does not publish instances.
The cost guard keeps its two source corpora and adds published-library requests
with an absolute empty-program fixed cost and a separate marginal cost per own
source line. The 60,000 instruction budget remains independent of observed
regression ceilings.

Package opening allocates no slot per library row. Immutable prefixes use sparse
fixed-height caches and request-owned suffixes; request snapshots store prefix
references rather than copying their rows. Each indexed key lookup walks at most
two fixed eight-page arrays and a bucket of at most 32 keys, with no package-wide scan. Body and
source-file records remain separately addressed. Diagnostics use indexed item
byte offsets, so naming a library site does not decode its whole source file.
`core-product --name core@1/string/byte_len --store <cache> --metrics <file>`
measures opening, one name, its signature and teardown separately; `--image`
selects an already-produced image for the same command. Metadata/index rows,
files and bodies have independent demand counters in the JSON result.

The root also records the package source stamp. A valid image from a different
source revision is a miss even if copied into the expected cache directory.
External record failures cross the infallible table APIs as a private typed
unwind and become ordinary IO errors at driver/image boundaries; compiler
panics are not caught as cache misses. These compiler APIs require unwinding;
the native program runtime is unaffected. All disk records retain the existing
version, exact-length and content-digest boundary.

`collect_instances` collects reachable distinct instances without source rechecking,
with an explicit compiler-resource stop for growing instance families. The checked
product adapter follows direct calls/function references in the CoreIR the checker
currently emits; it explicitly stops when generic materialization or indirect target
information is still unavailable. Native code production belongs to the emitters.

`--json` emits PROG-7 rule IDs, module paths with absolute file byte spans, causal
sites, and declared/required facts supplied by the checker. Refusals exit 1; compiler
gaps and tool/resource errors exit 2; an accepted check exits 0.
`--metrics <file>` records exclusive user-instruction stages or a counter refusal.
Their sum is the independently read counter interval. External process-tree perf
also counts startup and reporting outside that interval; report that residual.

`hirsel-core build <package-dir> --runtime <libhirsel_core_rt.a> -o program`
uses the same checked request, collects reachable instances, emits one object per
checked body and links the independently built core runtime. `run` builds and
executes it, forwarding arguments after `--` and returning its process status.
`--backend cranelift|llvm` selects an engine. Development builds and runs default
to Cranelift; `--release` defaults to LLVM with optimization. `--cc`, `--target`
and `--limit` name the C linker driver, target and root logical limit. The current
native target is x86_64-unknown-linux-gnu. `--json` includes the executable,
compiled-object count, link count and explicit stage stop.

Object keys include compiler, checked-body/read identity, emitted module, target,
backend, optimization and runtime bytes. Reached installed-package objects live
in the package store and are shared across application stores. They compile on
first demand, including nongeneric bodies; no eager monolithic core object is
built. Executables additionally key the entry
thunk, headers and linker version. A warm build reads integrity-checked products
and compiles/links nothing; an implementation edit rebuilds its object and relinks.
The real `hirsel_core_run` boundary maps PROG-3 outcomes after cleanup. Entry
binding reads the emitted main's `Signature.dest` and derives its instantiated
layout from the emitted type table. Exit's status uses the result payload offset
plus the inline Exit field offset. Cranelift receives those computed module
layouts; LLVM derives the same layouts in its emitter. A generic declaration's
summary recipe is not the layout of an instantiated result.

`//:core-driver-native` checks and builds real source main functions on both
engines, including `Error(Exit(status: 7u8))`, success, checked overflow, reachable
calls, cache reuse and callee edits. There is no test-only C Result bridge.

`check` and `image` require every supplied body and declaration to check.
`build` and `run` require the entry's reachable native instances and the types
and signatures they name. Unreached body/declaration diagnostics remain visible
to package check but do not block native assembly. Reachable-image checking loads
cached library outcomes per demanded body; package checking explicitly enumerates
all supplied outcomes. Resolved signature/body reads
are cached separately from invalidation reads, which also include shadowed and
absent names. The driver reports reached diagnostics and a first-stop census in
build/run JSON. Behind incomplete bodies the census follows resolved source
names; completed bodies follow emitted calls/function references.

FORM decision 44 gives each main capability/Args parameter one plain `u64`
operand, filled from `Config.grants[i]` in declaration order. The standalone
bootstrap calls the runtime's OS process constructor with ordered kind/ordinal
requirements. `--dir <path>` supplies a directory, in Dir parameter order;
relative paths are resolved at executable startup. Repeat the option for more
directories. `--grant <kind>` explicitly supplies Net, Clock, Random, Stdout,
Stderr, Stdin, Env, Process or Tasks. Args is always present, including empty argv.
These options configure the built executable and are part of its link cache key.
For example, `hirsel-core run ... --dir . --grant Stdout -- input.txt normal`
grants the current directory and stdout and passes two program arguments.
Missing grants are diagnosed by build/run before native emission, naming the main
parameter and the required --grant or --dir option. Declaring a capability
parameter does not configure its grant. For example, a main with Stdout needs
`--grant Stdout`; omitting it is a CAP-4 configuration refusal. The runtime closes grants
after main cleanup and diagnostics, then the bootstrap frees process metadata.
All three runtime headers are embedded and included in link keys. Owned error
results still need the checker's cleanup support.

## Native compilation units

Both dev and release currently emit one object for each reached definition's
checked CoreIR module. A unit contains that definition's body, signature-only
callee/function-reference rows, and its complete local type, static, signature,
instance and source-anchor tables. `emit_unit` passes the module intact.
Cranelift receives the recipes computed from `module.types`, including their
drop recipes; LLVM's CoreIR entry computes the same recipes internally and uses
them for layout and drop glue. Nonempty type tables are supported.

LLVM's failure analysis can see the unit's one body, including self-recursion,
but another source definition is an import even when it lives in the same
package. Its failure status therefore remains unknown across that boundary.
Function references and exported symbols retain the external status ABI.

A package-sized or whole-reachable release unit would need a mechanical merger
of the checked modules: deduplicate nominal types and instances, remap module
function/type/signature/static/item references and finalizer references, then
compute recipes for the merged table. Body-local values and CFG IDs stay local.
The object key would include every member body. A body edit would recompile its
package group or the whole reachable group, and LLVM would hold/optimize the
larger IR at once. Package groups retain unknown cross-package calls; a reachable
group exposes more direct bodies. Both may enable status elision/inlining and
amortize emitter setup, but reduce codegen parallelism and incremental reuse.
No compile-time, runtime or memory benefit is measured yet. Grouping must preserve
reachable-only acceptance. Dev keeps its per-body granularity; no larger release
unit policy is implemented here.

A unit's type finalizers are native dependencies alongside calls and function
references. The same dependency walker serves instance collection and diagnostic
reachability, including finalizers imported during generic materialization. The
producer's complete type table makes finalizers behind record/variant, Option,
List and Box recipes visible without source calls to the drop block.

The package format and producer also accept core+std together, including existing
module extensions and checked constants/templates. A combined-image regression
adds an application extension and reaches the core, std and application bodies.
Automatic std-package selection is the remaining driver integration step; std's
constant/declaration acceptance and all body completion are still required for
the standing canonical-image rows to turn green.

`build` and `run --logical-charge-report <file.json>` create a diagnostic build.
The version-2 JSON keeps `observation_mode:true` and `peak_root_reserved_bytes`.
It adds these independent measurements at each native phase and at run end:

| Field | Meaning |
|---|---|
| `charged_bytes` | MEM-19 root reservation, including unused bounds/tasks/grants and startup. |
| `retained_bytes` | Live backing allocation requests, including native prefixes, region chunks/handles, task/channel storage, cause cells and resource state. |
| `slack_bytes` | Unassigned region chunk tails, including tails stranded by refill. |
| `peak_charged_bytes`, `peak_retained_bytes`, `peak_slack_bytes` | Independent lifetime maxima; the old peak field aliases peak charged. |
| `phases` | `run_start`, `startup`, `main`, `run_end`, each with current fields and interval peaks. |

Program stdout, stderr, exit status, Capacity boundaries and refund delivery stay
the same. These are separate units: `retained - charged` is not slack, because
unused limit reservations need no physical backing. Retained is allocator-requested
storage, not RSS, malloc headers/size classes, or arenas kept after free. Limit
records, observer bookkeeping, host/provider storage and call stacks are excluded.
Slack does not guess spare collection capacity, consumed alignment holes or exact
compact-builder trim waste; those lie inside already allocated extents and the
runtime has no live lengths/used extents to infer them from portable charges.

An observed-only native harness can call
`hirsel_core_memory_checkpoint(name,len)` between operations to name application
phase endpoints. This admits nothing, delivers no refunds and creates no event;
it is not a source operation. No ABI region mutation may be in progress.
`hirsel_core_memory_report(&len)` borrows the last completed UTF-8 JSON bytes until
a run next starts or completes on that worker. The driver writes them only after
the run; a report write failure remains a driver/report error. Arbitrary internal
source phases are not inferred from the automatic native lifecycle checkpoints.

Use the optional `//:hirsel-core-rt-observed-archives` Buck artifact with
`--runtime <artifact>/x86_64-unknown-linux-gnu/libhirsel_core_rt.a`. Its Cargo
equivalent is feature `charge-observation`. Shipping runtime archives contain
no sampling hooks, branches or TLS accesses. Observed objects and the build JSON
are labelled `observation_mode: true`, have distinct cache keys, and must never
be used as performance samples. The compiler prints the same timing warning.
Mixing observed emission with a shipping runtime fails at link time. Enabled
mode samples successful runtime and inline admissions; it also retains actual
empty-bound reservations that ordinary emission may cancel. Sampling changes
execution cost, not language-visible event order. This mode currently follows
the single-worker cooperative scheduler; a future multi-worker scheduler must
provide a synchronized run observer before using it.

`hirsel-core test <package-dir> --limit <bytes> --runtime <archive>` discovers
`@test` functions in the request's source modules, including private functions.
Installed dependency tests are not rediscovered. Tests take only unmarked
capabilities, no `Args` or type parameters, and return `Result[(), E]`; only
other tests may refer to them. A test retains normal access to its module's
private helpers. `--filter <substring>` matches the full definition path; the
driver visits matching paths in lexical order. No matching test exits 2.

Each selected test builds through the ordinary reachable closure and runs in
its own process. The mandatory `--limit` supplies each run's root limit; `--dir`
and `--grant` configure capabilities exactly as for `run`. The typed API exposes
`CheckProduct::tests` and `run_test(test, options)`, allowing each invocation its
own explicit configuration. A run stop ends only that test's process. All later
tests still run after an error, failure, stop, setup refusal or build refusal.
Returned owned errors are dropped by emitted FORM glue before their outcome is
reported; a failure from that cleanup is a failed test too. An `Exit` never makes
an `Error` pass. The driver does not share a test's run or root bookkeeping.

Human output is one `pass <definition>` or `fail <definition>: <reason>` line per
selected test. Error outcomes say `error`; failures, stops and setup refusals
include the run's stderr, with lines joined by ` | `. `--json` instead retains
each test's status and captured stdout/stderr or build-stop reason. Exit 0 means
at least one test ran and every selected test passed; 1 means a selected test
failed; 2 means no tests matched or configuration could not be read. `--backend`
and `--release` select the same engines and options as `build`; `--output` is a
test-executable directory, defaulting to the private store's `native/tests`.

`test --test-timeout <positive seconds>` optionally bounds each independent
native execution. It starts after build/link and records `timed_out` in JSON;
a timed-out run fails and later tests still run. With no option there is no
execution deadline. The core/std law walker uses this path with its existing
30-second execution bound; its build work remains unbounded.

`help`, `--help`, and `-h` show the command catalogue. `help <command>` and
`<command> --help` or `-h` show that command's options. The same catalogue
selects legal options and produces help; unsupported or retired flags fail
with exit 2. Arguments after run's `--` belong to the program.

`new <package-dir>` creates a new-source application. `new <package-dir> --lib`
creates a public `lib.hirsel` and an example `main.hirsel` that calls it. Both
check, build, and run with the ordinary directory interface. Existing directories
are refused without changing their contents. Native build/run still require an
explicit runtime archive until installed-root selection is configured.
