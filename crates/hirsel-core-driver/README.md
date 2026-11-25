# Core driver

`hirsel check <package-dir> --libraries core --store <private-store>` reads
new-language `.hirsel` sources. Relative file paths name modules in `app@1`;
`core/core` and `core/std` name `core@1` and `std@1`. `--inputs <json>` instead accepts
an explicit resolver `CorpusManifest`. Directory checks select `app@1/main/main` as
the entry; `--entry <full-definition-path>` selects another. Library manifests have
no implicit entry. Entry selection is a cache input because main is always waits.

The serial request loads and hashes sources once, parses, builds the complete name
index, resolves once, publishes declarations/layout recipes and checks/lowers each
body once. Each body owns its CoreIR module and signature-only callee rows. Syntax,
name rows, successful bodies and diagnostics move into shared in-memory products.
Each checked body is one content-addressed record, and its address is the body
product's content hash. A persistent 16-way index over definition paths holds one
small row per live definition of the request (its definition-key hash and record
address); file records hold each module's namespace map and anchor map. One
rename of the `program-index-v2` selection publishes the root node, rows and maps
together, so the index holds the last request's rows alone. Each checked body
also writes a key pointer, addressed by its path and key hash, to its record: a
body whose row misses (another request, such as `run` without the test sources
or `test` without an entry, published last) is found by its key, and the record's
own path and key must match. A request reads the whole index, never a whole
product, and writes only the records that changed. Disk reads check version,
length and checksum.
Resolver scratch is not retained or encoded; definition keys retain its observed
lookup fingerprint and one hash of the transitive signature components. A body's
own part of its key is its item's source bytes: record sites and IR anchors are
offsets from the item start, which includes a leading comment, so a comment or
layout edit inside an item rechecks it, while a moved item keeps its bytes and its
record. Signatures are keyed by header tokens; a resolved body's record cites no
other item.
No old std products, old requests, TIR or independent checker enter this path.

A warm in-memory snapshot performs no source checking. Requests reuse unchanged
definition products from memory or from their records, including refusals. Products are trusted in memory.
External disk records check format version, exact length and checksum; damage
is a cache miss. Records publish before the selection. No old requests,
old std products, TIR or independent checker enter this crate. The workspace
layering audit enforces its new-core-only dependency boundary.

`hirsel image --libraries core --store <image-dir>` checks library sources
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

Native objects, executables and records live in the user's content store,
`$XDG_CACHE_HOME/hirsel/cas` (or `$HOME/.cache/hirsel/cas`), which every build
of the user's shares. A blob (an object, its debug rows, a release part, a std
part's bitcode, an image unit, a code pack, a linked executable and its
detached debug file) is the file `o/<sha256>` of its bytes; a record is `k/<key>`, a frame that names blobs by digest and carries an
origin, which a reader requires to be this machine's. Both are sharded by their
first byte (`o/xx/<62 hex>`). A record whose blob is missing or fails its check
is a miss and is rebuilt, and so is a record or blob stored under another's
name. A release std side is a store of the same shape under `s/<key>/`, which a
build pins with a shared lock while it reads it. An image ships its prebuilt
objects (`native`) and release std (`release-std`) as stores of the same shape.
An executable is the blob the record under its link key names, with its
debug file's: a build whose executable blob is damaged, or holds another's
bytes, links again and never runs it. A request's record (its answer, under its
inputs' key, which does not name the request's store) is a record of the same
store. Delivery reflinks the checked blob to the output
where the filesystem shares extents, and otherwise copies it; only the
delivered file is executable. A request's `--store` keeps its scratch.
One collector keeps the content store under 10 GiB: at most once a day, or
hourly while over, it removes the least recently used records, blobs and
unpinned sides (a hit touches what it read) down to 7/8 of that. A file a
running build has open stays readable after its removal; its next read misses.
Opening a store of an older layout deletes its records and sides, and the user
store also deletes, by name, the directories older formats kept beside it.

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
The cost guard measures two source corpora and published-library requests.
Fixed requests divide the empty-program process cost by library tokens.
Marginal requests subtract the fixed std process cost and divide by program
tokens. The finished budgets are independent of the observed regression ceilings.

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
version, exact-length and checksum boundary.

`collect_instances` collects reachable distinct instances without source rechecking,
with an explicit compiler-resource stop for growing instance families. The checked
product adapter follows direct calls/function references in the CoreIR the checker
currently emits; it explicitly stops when generic materialization or indirect target
information is still unavailable. Native code production belongs to the emitters.

`--json` emits PROG-7 rule IDs, module paths with absolute file byte spans, causal
sites, and declared/required facts supplied by the checker. Refusals exit 1; compiler
gaps and tool/resource errors exit 2; an accepted check exits 0.
`--metrics <file>` records exclusive user-instruction stages or a counter refusal,
and the request's counts: body keys built (calls and bytes), reach rows read,
objects and files opened, and image bytes read by kind (directory, declarations,
index pages, files, bodies, sources). `core/tools/request_profile.py ROW` samples
one request-cost row with `perf record --call-graph dwarf` and reports it by stage.
Their sum is the independently read counter interval. External process-tree perf
also counts startup and reporting outside that interval; report that residual.

`hirsel build <package-dir> --runtime <libhirsel_core_rt.a> -o program`
uses the same checked request and collects reachable instances. Development emits
one object per checked body; LLVM release lowers the program's bodies into one
unit over the separately lowered release std. Both link the independently built
core runtime. `run` builds and
executes it, forwarding arguments after `--` and returning its process status.

Every LLVM release build, `build`, `run` and `test` alike, is assembled from its
published recipe over the release std. When the build's library image holds
the std its std product prebuilt for the build's target and generator, the
release std is that: every plain std root the image checked whole, with what
it reaches, less the roots that take bindable code or make owned network waits
and what those roots reach. Its modules each run
the full optimizer and emit machine code when the toolchain installs the
target (`std-product --install`) or on `std-product --store|--image --target
TRIPLE`. A build over
it compiles none of the std. Otherwise the release std is the plain core and
std instances the program reaches, with what they reach, lowered apart from the
program and keyed by those instances' content, in the user's cache, where every
build reaching the same roots finds them. The std side exports its plain
roots, each with its step summary, and lends bodies of at most `LEND_LIMIT`
instructions `available_externally` for inlining. The release unit holds the
program's own bodies and every other library instance it reaches, so its
optimiser sees the closure and dictionary arguments it passes them, along with
the held roots and what they reach; it calls every export
by its symbol, and its widths stay dynamic. A program that reaches no std
root links the side like any other.
The unit's key covers every reached module and the sealed and place-blind
parameter facts its emitter consumed, with the fact schema.
After the whole-program pipeline, code generation runs per part: one per
out-of-line definition, with the declarations it references and the constants
it uses, its local names derived from content. A part's object is keyed by the
code generator (compiler, libLLVM, target, debug lines) and the part's bitcode,
so an unchanged definition is generated once whichever program reached it.
Every part goes to the user's content store; the unit's record there lists its
parts, and an unchanged rebuild loads them all.
Release builds cache the program-wide role facts by their complete content
inputs. An unchanged build restores the same consumed rows; an edit in the
fixpoint's input set derives them again. Native object keys retain the exact
facts read by their emitter.

`--backend cranelift|llvm` selects an engine. Development builds and runs default
to Cranelift; `--release` defaults to LLVM with optimization. `--cc`, `--target`
and `--limit` name the C linker driver, target and root logical limit. Without
`--cc` an installed compiler links its host target through the recipe and link
editor in its prefix and runs no C driver; another target refuses until `--cc`
names one. A compiler that is not installed uses the pinned LLVM's
`$LLVM_SYS_231_PREFIX/bin/clang`, and with that unset refuses. No request looks
for `cc` on PATH. The current
native targets are x86_64-unknown-linux-gnu and aarch64-unknown-linux-gnu;
cross execution requires explicit --emulator and --sysroot inputs. `--json` includes the executable,
compiled-object count, link count and explicit stage stop.

Object keys include compiler, checked-body/read identity, emitted module, target,
backend, optimization and runtime bytes. Reached installed-package objects live
in the package store and are shared across application stores. They compile on
first demand, including nongeneric bodies; no eager monolithic core object is
built. Executables additionally key the entry
thunk, headers, runtime bytes, the C driver's and link editor's bytes (not
their paths, and no version output), and the link recipe: the editor's argv
and the path and bytes of every file the link reads. Digests are cached by
device/inode, length, mode, nanosecond mtime and ctime; replacement and
in-place changes refresh them. Link recipes, the editor name a driver gives
and toolchain manifests are records in the per-user content store's `k/`,
under keys of the target, driver bytes, capture environment and editor; an
object a capture compiles is an `o/` blob there. Incidental metadata changes
do not invalidate a link whose tool content is unchanged. PATH selection is repeated
on each link request; a missing selected editor is a named link refusal. A warm build reads integrity-checked products
and compiles/links nothing; an implementation edit rebuilds its object and relinks.
A link reads the package-image objects it reaches as one image unit: `ld -r` of
them in link order by the link's own linker, stored in the image's store under a
key of the toolchain, link recipe, target, flags and the ordered object
identities, and checked on every read. A damaged unit, or a frame under another
key, is prelinked again. A fresh store links through the same unit, so a primed
store's executable is a fresh store's; it is not byte-equal to a link of the
separate objects (`ld -r` renumbers local and section symbols).
`--debug-lines on|off` controls whether native DWARF5 line tables are embedded in
the executable for build, run and test. Native builds always emit debug rows and write
a detached DWARF debug file beside the executable; `--debug-lines on` additionally
embeds DWARF in the executable itself (see `docs/spec/cli.md`).
`explain debug-lines` describes the same choice. Native link keys include it;
object emission and object keys are the same either way. Failure-site symbols,
source tables and exact failure reports stay enabled either way.
Native body products contain clean ELF code and a separate
record of machine ranges, item-relative byte offsets and inline scopes. The
object keys contain no absolute source position. One shared link-time writer
resolves those records against the current checked source snapshot and writes
one DWARF5 debug object.
The engines publish clean ELF bytes and their rows together; link inputs
are never rewritten. LLVM extracts rows once when it first produces an object;
Cranelift publishes its machine rows directly. Moving an unchanged item
therefore rechecks and compiles no existing body; only the link product changes.
Published source text has its own indexed rows and never requires decoding a
library AST or reopening source files. Installed products contain those bytes.

The driver emits the fresh entry/link object directly on x86-64 and AArch64,
using Cranelift's C ABI and the same Object ELF writer on both program engines.
It contains startup, the Result/cleanup callback and static argument, grant and
source descriptors. There is no generated C translation unit or C compilation
step. A C driver runs only to capture the link recipe (CRT, sysroot and
library selection), at install or for an explicit `--cc`. Runtime headers remain embedded ABI/cache inputs. Entry code
generation runs only on a link-cache miss.
The shipping runtime archive has its Rust debug metadata removed once at
publication, preserving code, public symbols and unwind data. All profiles
link that same archive; the linker performs no per-program debug stripping.

The fresh entry object also defines the linked item ordinals and hands the
borrowed current-source table to `hirsel_core_os_run_with_sources`. An exit101
report resolves each cause's original raising operation to file:line:column;
columns count Unicode scalars and a tab counts one. Core and generated code
inherit the nearest non-core caller's site.

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
directories. `--grant <kind>` explicitly supplies `Net`, `Clock`, `Random`, `Stdout`,
`Stderr`, `Stdin`, `Env`, `Process`, `Tasks`, `Secret`, `Cred`, `Foreign`, or `Exec`. Args is always present, including empty argv.
These options configure the built executable and are part of its link cache key.
For example, `hirsel run ... --dir . --grant Stdout -- input.txt normal`
grants the current directory and stdout and passes two program arguments.
Missing grants are diagnosed by build/run before native emission, naming the main
parameter and the required --grant or --dir option. Declaring a capability
parameter does not configure its grant. For example, a main with Stdout needs
`--grant Stdout`; omitting it is a CAP-4 configuration refusal. The runtime closes grants
after main cleanup and diagnostics, then the bootstrap frees process metadata.
All four runtime headers are embedded and included in link keys. Owned error
results are cleaned before the outcome is reported.

## Trust contracts

The driver trusts three things beyond what it checks. Each is a contract, not a
gap to be closed later.

- **Store records are trusted beyond their checksum.** Every byte a read uses is
  covered by an xxh3-64 checksum: a record or selection carries it in its frame,
  and a bare object's sits beside its address in the record that names it. A
  record whose format version, length and checksum check is used as written.
  SHA-256 is computed once, at write, only to name content. The checksum catches
  damage, not intent: a record crafted to carry a matching checksum is outside the
  threat model. A store is as trusted as the user account that writes it.
- **The store is never synced.** Writers rename a locked temporary from
  `<store>/tmp` into place; opening a store removes each temporary no writer
  holds. After a power loss a torn, short, zero-filled or missing record fails
  its check or is absent, so it is a miss and is rebuilt. A build's own code
  cache (`<store>/code-store`, `<store>/code`) follows the same rule: each
  publication appends one unsynced pack (payloads, an index of xxh3-64-checked
  rows, a checked trailer), and a pack whose trailer or index fails its check is
  ignored and a row whose payload fails its check is a miss. This contract covers the
  driver's build cache only. A host's code store, image store and assembly records
  back retained releases, are synced in the order
  [launch.md](../../docs/spec/launch.md#release-recipe-and-host-assembly) gives, and
  never live in this cache.
- **Cached mappings come only from their producer.** A cache entry that maps a
  key (a native object's body, annex and emission environment, or a final link's
  recipe and toolchain fingerprint) to bytes is used only where this driver or
  host wrote it under that exact key. Objects read from another code store are
  checked against their hashes and used as content, never as a mapping from a
  key to a result.
- **Tool inputs are trusted by the two-sighting stat rule.** The link driver or
  installed recipe, link
  editor, runtime archive and every file the link recipe names are hashed when
  first seen. A file seen again with the same stat and bytes at least two seconds
  later (boot clock) is settled, and a settled file whose stat still matches is
  trusted without reading it. This rule does not defend four cases:
  1. a write through an already-writable shared mapping, which moves no
     timestamp until writeback;
  2. a filesystem whose timestamp granularity is coarser than two seconds;
  3. a file clock set back so that a later edit repeats an earlier timestamp
     exactly;
  4. an input changed during the request that reads it.
  An identity that does not rest on times would mean hashing every tool on every
  request: about 230 MB (libLLVM and ld.lld), 615 M instructions against a
  no-change request's 56 M. Hashing once per boot costs the same after each boot
  and still misses a rollback within it.
- **Libraries that LLVM itself loads are not keyed.** The libLLVM the compiler
  opens is part of every LLVM object key. Anything that library loads in turn
  (its own shared-library dependencies, plugins) is not. If one of those changes
  under an unchanged libLLVM, clear the caches to rebuild: the request's
  `--store` and `$XDG_CACHE_HOME/hirsel` (by default `~/.cache/hirsel`).

## Native compilation units

Development emits one object per reached checked definition. LLVM release
lowers the program's bodies, over the release std, into one unit that exports only
the entry. Units carry complete local
type, static, signature, instance and source-anchor tables. Cranelift receives
recipes computed from `module.types`; LLVM computes the same recipes internally.
Both retain finalizer and drop inputs. Release exposes direct bodies for inlining
and failure analysis; dev keeps body-sized rebuilds. Function references and
exported symbols retain the external status ABI.

A unit's type finalizers are native dependencies alongside calls and function
references. The same dependency walker serves instance collection and diagnostic
reachability, including finalizers imported during generic materialization. The
producer's complete type table makes finalizers behind record/variant, Option,
List and Box recipes visible without source calls to the drop block.

The package format and producer also accept core+std together, including existing
module extensions and checked constants/templates. A combined-image regression
adds an application extension and reaches the core, std and application bodies.
Builds select the published std product automatically; source mode remains
available for agreement controls. Both canonical-image rows are accepted.

`build` and `run --logical-charge-report <file.json>` create a diagnostic build.
The bundled toolchain selects a charge observation runtime for the target and
recording mode. When passing `--runtime` explicitly, use an archive built with
`charge-observation`.
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

`hirsel test <package-dir> --limit <bytes> --runtime <archive>` discovers
`@test` functions in the request's source modules, including private functions.
Installed dependency tests are not rediscovered. Tests take only unmarked
capabilities, no `Args` or type parameters, and return `Result[(), E]`; only
other tests may refer to them. A test retains normal access to its module's
private helpers. `--filter <substring>` matches any full definition path
containing the substring. `--exact <definition>` matches the complete path,
such as `app@1/main/smoke`. Repeat `--exact` to select multiple paths.
The selectors are mutually exclusive. The driver visits matching paths in
lexical order. No matching test exits 2.

Each selected test builds through the ordinary reachable closure and runs in
its own process. When `hirsel.toml` declares `[tests]`, its `limit`, `dirs`, and `grants`
keys configure the run (and CLI `--limit`/`--dir`/`--grant` are refused); otherwise `--limit`
is mandatory and supplies each run's root limit while `--dir` and `--grant` configure
capabilities exactly as for `run`. The typed API exposes
`CheckProduct::tests` and `run_test(test, options)`, allowing each invocation its
own explicit configuration. A run stop ends only that test's process. All later
tests still run after an error, failure, stop, setup refusal or build refusal.
Returned owned errors are dropped by emitted FORM glue before their outcome is
reported; a failure from that cleanup is a failed test too. An `Exit` never makes
an `Error` pass. The driver does not share a test's run or root bookkeeping.

`-j N` limits concurrent children and defaults to the CPU count in the
process affinity mask. `-j 1` runs everything serially. Tests with a granted
Dir, Net, Stdin or Secret run alone, in discovery order. Captured output
streams, Tasks, Env snapshots and Process stop handles are local to each
child. Clock and Random grants also allow parallel runs. Replay, emulator
runs and runs with a shared charge report run serially. Test children start
without inherited host place launch settings. Rows always appear in discovery order.

Human output names the verdict, `cached` or `executed`, and the definition in
each selected test's row. Error outcomes say `error`. Failures, stops and setup refusals
include the run's stderr, with lines joined by ` | `. `--json` instead retains
each test's status and captured stdout/stderr or build-stop reason. Each row
includes child wall time in milliseconds (`wall_ms` in JSON), excluding build
and link time. Replay rows sum the recording and replay child times. Cached
rows and refusals without a child outcome have no time (`null` in JSON).
Exit 0 means at least one test matched and every selected test passed. Exit 1
means a selected test failed. Exit 2 means no tests matched or configuration
could not be read. `--backend` and `--release` select the same engines and
options as `build`. `--output` is a
test-executable directory, defaulting to a scratch directory in the request's store.

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
are refused without changing their contents. Installed toolchains select their
registered runtime archive and link kit automatically; uninstalled source builds
require an explicit `--runtime` archive.

`env --json` reports the executable, optional installation root, selected package
cache and actual product roots, target and selected runtime. Explicit
`--core-product`, `--runtime` and `--target` take precedence over installed defaults.
With no registered runtime, env reports null and native commands require --runtime.

`std-product --install ROOT --runtime ARCHIVE [--target TRIPLE] [--link-kit DIRECTORY]`
makes ROOT a self-contained installation and `core-product` takes the same
options. A request from ROOT/hirsel loads nothing outside ROOT and the system:
no environment variable, no checkout and no C driver.

    ROOT/hirsel                      this compiler, its RPATH patched to $ORIGIN/lib
    ROOT/lib/                        libLLVM.so.23.1 and the libraries it loads from the pinned tree
    ROOT/bin/ld.lld                  the link editor
    ROOT/libexec/hirsel/             the platform host, pin_reader and store_tool binaries
    ROOT/link/<target>.json          the bundle-relative link recipe
    ROOT/link/kit/                   the startup objects and link-only libraries
    ROOT/runtime/<target>/<hash>/    runtime archives, by content
    ROOT/core-packages/              the core, std, and store-1.hpkg images
    ROOT/hirsel-core-toolchain.json  the selection, committed last

The system supplies runtime libraries such as glibc and libstdc++. Installing
the host runtime requires `--link-kit`, which supplies the editor, startup
objects and link-time libraries. Another target's runtime is registered without
a recipe and its links take `--cc` and `--sysroot`. A missing piece of the prefix is a named refusal at the
request that needs it; nothing falls back. --store and --image cannot combine
with --install; product --runtime and core-product --target require --install.
`std-product --target TRIPLE` with --store or --image prebuilds that image's
release std for TRIPLE into the image's store. The distribution
assembler uses this same producer. It does not switch the currently installed
old CLI.
A copied new compiler beside the registration uses these products/runtime from
any working directory; moving the entire installation preserves relative paths.

`fmt [--check] <package-dir>` formats every .hirsel source recursively through
core-syntax's package formatter. Check mode prints differing paths and exits 1
without writing. Write mode prints changed paths. A syntax refusal exits 1 and
changes no file in the package; a resource refusal exits 2. Use --signatures for
core declaration headers without bodies. The public formatter does not exempt
coordinate-bearing diagnostic fixtures selected by a developer.

`explain RULE` and `explain --list` use the catalogue generated from docs/spec.
`primer` and `explain primer` print docs/PRIMER.md, compiled in.
--json emits the same Rule fields or sorted array; human output identifies the
spec file and full-rule anchor. Unknown IDs and list/rule conflicts exit 2.
No old guidance catalogue is consulted.

Explicit QEMU execution validates the selected emulator and GNU runtime loader
before building. Execution commands are reconstructed on each run/test; no
execution result is cached. A replacement emulator or sysroot is therefore used
immediately with unchanged compiled objects. Invalid selections give named input
refusals. The C cross driver selects its link sysroot; --sysroot names QEMU's
runtime root and is not a hidden C compiler flag.
