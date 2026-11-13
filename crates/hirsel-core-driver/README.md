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

`--metrics <file>` records exclusive user-instruction stages or a counter refusal.
Their sum is the independently read counter interval. External process-tree perf
also counts startup and reporting outside that interval; report that residual.
