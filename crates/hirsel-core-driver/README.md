# Core driver

`hirsel-core check <package-dir> --libraries core --store <private-store>` reads only
new-language `.hirsel` sources. The directory convention maps each relative file path
to a module in `app@1`; libraries map `core/core` and `core/std` to `core@1` and `std@1`.
`--inputs <json>` accepts the resolver's explicit `CorpusManifest` instead.

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

`--json` emits structured results; `--metrics <file>` records exclusive user-instruction
stages when Linux perf events are available, or an explicit counter refusal. Their sum
is the independently read interval total. External `perf stat -e instructions:u` also
counts process startup and metrics serialization; report that residual separately.
Current checker gaps make a request incomplete and exit 2, language refusals exit 1,
and accepted checks exit 0. A partial checker result never becomes acceptance.
