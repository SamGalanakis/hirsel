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
The real `hirsel_core_run` boundary maps PROG-3 outcomes after cleanup. There is no
temporary process runtime or scalar-main product path. Entry destination/layout,
capability bindings and owned error cleanup currently stop explicitly where the
checker has not supplied the necessary products. `//:core-driver-native` exercises
scalar calls, Exit mapping and checked failure on both engines with a C fixture
that supplies the still-missing Result carrier. This is partial native proof,
not acceptance of source-language main or M1.
