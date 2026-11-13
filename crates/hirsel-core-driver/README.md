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
Image open reads the directory only; the declaration section decodes on its first
signature demand, and file/text/body rows decode on individual demand. Signature
imports neither read library sources nor decode callee bodies. Image byte damage
is reported as a cache miss requiring an image rebuild.

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
backend, optimization and runtime bytes. Executables additionally key the entry
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
to package check but do not block native assembly. Resolved signature/body reads
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
Missing grants refuse configuration before main starts. The runtime closes grants
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
