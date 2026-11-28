# Command-line interface

Run `hirsel help` for the command catalog. Use `hirsel help <command>` for
the options accepted by a command.

## Operator reads

`hirsel op get host/CATALOG/REQUEST [--wait[=DURATION]] [--context NAME|/ABSOLUTE/FILE] [--json]`
reads the host's committed Status row. CATALOG is the 32 hex digits of the
catalog Directory StoreId (high word first). REQUEST is the 16-byte caller ID
in hex. Both survive a host restart. The row names the group and incarnation
of the original request, including after a name is reused.

An absolute job file selects the host named by its `default_context`.
A named context looks up the name in the workstation's
`$XDG_CONFIG_HOME/hirsel/config.hcfg` (fallback `~/.config/hirsel/config.hcfg`).
Both files use the same schema. A job pin can select a group and incarnation.

```hcfg
user_v1.Config(
    contexts: [("ci", Host(state: "/srv/hirsel/private", catalog_id: "c01d3dedc01d3dedc01d3dedc01d3ded",
        principal: Uid(1001), group: Some("orders"), incarnation: Some(17)))],
    default_context: Some("ci"),
    routes: [],
)
```

The last two fields narrow the read and are optional. Preflight prints the
chosen file and pin on stderr. Resolution never reads a project manifest or
registry route. The client verifies the private directory, socket and peer UID,
then sends Identify on that connection. The host returns its StoreId. After
checking the pin the client sends Pinned(high, low, encoded request). The host
checks the ID against its open catalog before dispatch, including for writes.
A missing pin, an invalid locator or a changed identity stops the exchange.

Converted finite commands write one v1 JSON document and newline on stdout
under `--json`. The document contains `v, command, contract, exit, target,
operation, outcome, result, error, next`. Unknown commands and parser errors
use the same document on terminals and pipes. A guest argument after `--`
cannot select the renderer. IDs use hex and u64 counters use decimal strings.
The four exits are 0 for a completed read, 1 for a definitive negative result,
2 for invalid or stale input and 75 for a blocked or unresolved request.
Reading a known failed operation succeeds. Its phase and effect describe the
observed operation. Pending and reconciling installations report possible
effects, and Installed reports committed even when the first start failed.
A failed read carries a code, detail, retry disposition and optional field.

`hirsel help --json` lists the declared grammar. `hirsel help op get --json`
returns that command's declaration, including its read contract, target,
authority, completion, receipt and output properties. Both work offline.

### Submitting a release

`hirsel deploy [DIR [--source ROOT] | --recipe R] [--create SPEC] [--plan] [--wait[=DURATION]] [--request-id D]`
uses the selected host context. Select a group with the context or `--group NAME`. For an attested recipe, `--code-store DIR` selects its local object store.
`--create` reads a `host_v1.Group` document. Creation requires a decision on the proposed name, listener, store and group policy. The first release shows its grants against empty authority.
An ordinary deployment can proceed as soon as policy clears its effects. A hold returns 75. An explicit plan returns 0 at the hold with effect `none`.

Before submission the client saves the request identity, original precondition, input digest and local input coordinates in a private file under `$XDG_STATE_HOME/hirsel/operations` (default `~/.local/state/hirsel/operations`). It flushes the qualified operation ID on stderr. Resubmission takes the original bytes and request ID.

`hirsel op decide OP [--decision FILE] [--wait[=DURATION]]` reads a `host_v1.Decision`. Use `--decision -` for standard input. Without a file the command reads an explicit verdict and any held fates from the controlling terminal. The digest identifies the exact plan returned by the host. For example, a decision with no held entries supplies `verdict: Approve, entries: []` and the plan's `plan_digest`. A refusal has no fates. An approval covers every held path or supplies a single `all: Some(Release)` or `all: Some(Cancel)`.
A wait ends on install, failure, decision, ambiguity, deadline or local interruption. Duration suffixes are `ms`, `s`, `m` and `h`. Interruption leaves the host operation running.

## Output paths

Build, pack, and SDK generation accept `-o` or `--output`. Use a file
path for `build` and `pack`, and a directory for `sdk ts`.

## Output kinds

A native build emits an executable or a static library, chosen by `--emit exe` <!-- doc-ref: planned -->
or `--emit staticlib` <!-- doc-ref: planned --> ([EMB-1](embed.md#emb-1-output-kinds)). Each target
admits the kinds its row in EMB-1's table names. `hirsel build` takes either kind; a
command that runs what it builds (`run`, `test`, `sdk ts`, `sdk json`) takes only `exe`;
`sdk swift` <!-- doc-ref: planned --> builds static libraries itself and refuses `--emit` <!-- doc-ref: planned -->.
Any other pair is refused at request validation, before installation lookup or work.

## Checking packages

`hirsel check` reports the number of definitions checked in the sources,
including cached checks. With `--json`, this is `checked_definitions`.
The `rechecked` field counts bodies recomputed on this request.

## Native build options

`hirsel run` uses a temporary executable in the request's store by default.
Pass `-o PATH` to keep the executable at a specific path. A build stores its
executable and detached debug file in the user's content store
(`$XDG_CACHE_HOME/hirsel/cas`) and delivers a reflink of it, or a copy where the
filesystem has none; a stored executable that fails its check is linked again,
never run.

### Remote cache

A build or test request can also read a remote cache, configured in
`$XDG_CONFIG_HOME/hirsel/config.hcfg` (default `~/.config/hirsel`):

```hcfg
user_v1.Config(
    contexts: [],
    routes: [],
    cache: Some(Remote(url: "https://cache.example.test", trusted: ["<64 hex digits>"],
        signing_key: Some("/path/to/builder.pk8"), record_bytes: 268435456,
        blob_bytes: 1073741824, work_bytes: 2147483648)),
)
```

Every configured remote requires a transfer policy. `record-bytes` bounds the
whole signed envelope, including its signature. `blob-bytes` bounds each blob
or std side pack, including temporary storage for that body. `work-bytes`
bounds the sum of blob lengths named by one record. All three are positive
and `work-bytes` is at least `blob-bytes`. The same limits apply to reads and
uploads. Blobs and packs stream through bounded buffers. A size refusal does
not mark the endpoint down.

An unknown field in `[cache.remote]` is an error. The remote is a plain HTTP
store: `GET`, `HEAD` and `PUT` of `o/<sha256>` blobs and of
`r/<key id>/<key>` signed records, where a key id is
`hirsel.builder-key.v1` over the public key. The trust rules:

- A blob is used only when its bytes hash (SHA-256) to the name it was fetched
  under. Reads from the local store keep their xxh3 check.
- A record is used only when it is signed by a key in `trusted`, names the
  requested key and is of the requested kind. `trusted` is empty by default,
  so a remote with no listed key is never read. A received record is kept in
  the local store's `signed/<key id>/` and checked again on every read; removing a
  key from `trusted` withdraws every record it signed.
- Only three kinds travel: release std sides (one manifest record and one
  pack of their blobs), object records (release program parts and the unit
  objects of a development build), and Cranelift development instances. No kind exists for an executable, a test verdict or coverage;
  a coverage build and a program with foreign grants (FFI-8) neither read nor
  write the remote.
- Without `signing-key` the remote is read-only. With it, the request uploads
  what it built itself in the background (blobs first, then the record signed
  with that key) and waits for the uploads before it exits. A received record
  is never signed again.
- Received records never enter `keys/`. The launcher and the runner host open no
  remote, so they reuse only mappings this host produced, and an executable
  linked over received content is stored under a link key apart from one
  linked over none.
- A failed request pauses requests to that remote for 15 seconds. A missing
  name is cached for 30 seconds. Connecting times out after 5 seconds, and
  each read times out after 30 seconds.

Native binaries from both backends include `.eh_frame` unwind
information. Native builds write detached DWARF alongside the executable.
`--debug-lines on` also embeds DWARF. Failure positions are available in
either mode.
The native unwind records mark where a stack walk ends at a fiber boundary.

Native commands default to the compiler host's target. Use `--target` for a
different target.

Register the host runtime with `core-product --install` or
`std-product --install` by passing `--link-kit DIRECTORY` alongside
`--runtime ARCHIVE`. The installed compiler links user programs with the
copied kit and a bundle-relative recipe. Installing the std product for a
target also compiles its release std to machine code, so release builds over
the installed std compile only the program. `std-product --store
CACHE --target TRIPLE` (or `--image IMAGE --target TRIPLE`) does the same for
that product's image and target.

`--logical-charge-report FILE` selects the installed charge observation
runtime for the build's target and recording mode. Product installations
register these archives with `--observed-runtime ARCHIVE` and
`--observed-recording-runtime ARCHIVE`. An explicit `--runtime` must name a
charge observation archive.

A `--frozen` request reads, offers and uploads nothing remote: its transport permission
([packages §8](packages.md#8-hirsellock)) is absent, so the remote cache is skipped for that
request as if none were configured, with no connection attempt, no name lookup and no
endpoint marked down.

## Resident compiler

`hirsel serve` keeps one compiler process per user and toolchain. That
process answers `build`, `run` and `test` requests from every directory and
store, and keeps in memory the state those requests derive from unchanged
inputs, shared between working copies of the same sources. A toolchain is
named by the content identity of its compiler executable: two copies of one
compiler build share a server. The process listens on `<toolchain>.sock`
in a directory private to the user and holds `<toolchain>.lock` there while
it runs. It uses `hirsel/` under an absolute `XDG_RUNTIME_DIR`, or under
the host's private per-user runtime directory when available. Linux uses
`/run/user/<uid>` and macOS uses the system's per-user temporary directory.
If neither root is available, it uses `/tmp/hirsel-<uid>`.

`hirsel build`, `hirsel run` and `hirsel test` send their request to their
toolchain's resident compiler when one is running, and otherwise compile in
their own process. `--resident` also starts a resident compiler when none
runs. A resident compiler runs the request on the same path a one-shot
compiler takes, with the client's arguments, working directory, environment,
umask and standard streams, and then restores its own. Its output is
byte-identical to a one-shot build. A `run` request's program runs in the
client process, under the client's terminal and signals.

A `test` request is a session on its connection. The compiler checks
the request, builds each test and hands the client each launch; the
client runs every test, in a private working directory under the
client's own environment and execution deadline, reads its law and
coverage files, and writes a pass to the pass cache. The compiler reads
the pass cache, reports each row as its test ends, in order, and folds
coverage, the law lock and a law baseline when the client finishes the
session. A one-shot `test` runs the same session in one process, so its
rows, verdicts and pass-cache writes are the same. The compiler serves
the session alone; closing the connection drops it.

If the connection ends before the compiler answers, `--resident`
reconnects to a resident compiler, starting one if needed, and retries
compilation once. A `test` request or a request that can publish to a
durable code store or invoke a custom linker reports the lost answer. The
client starts a `run` program after receiving the complete launch answer.

A compiler reads a few variables once, when it starts:
`HIRSEL_MEMORY_LIMIT`, `HIRSEL_GUARD_ORACLE`, `HIRSEL_LLVM_NO_FACTS` and
`HIRSEL_LLVM_EXPERIMENT_BUILD_ID`. A resident compiler refuses a request
whose digest of those variables differs from its own, and a request from
another toolchain. The client then compiles in its own process, and the
server keeps serving. A server re-reads its executable's identity for each
request, refuses the request and exits once the file holds another
toolchain or is gone.

A request that panics fails alone: its client is answered with status 101,
and the server drops every remembered derivation, since the request may
have left one half made, and serves the next request. After each request,
the server releases the derivations its working set last used before its
previous request. Its memory budget derives from the memory limit
(`HIRSEL_MEMORY_LIMIT`, or the lower limit the server inherited): before and
after each request it evicts least recently used working sets until its
resident memory fits the retained budget, `--max-memory BYTES` or by default
a quarter of the limit, sparing only the working set being served or just
served; during a request it evicts the others whenever its resident memory
passes half the limit. A server refuses to start with `--max-memory` above
the limit. The set served before a request is not spared through it, so
distinct packages built in turn peak at the largest alone; an edit loop on a
working set larger than the budget keeps its derivations and its remembered
link. An exact memo entry
shared by several working sets stays cached while any of those sets uses
it. The working sets' remembered development links together hold at most
256 MiB; past that, the sets that linked least recently forget theirs and
link afresh. A server returns its free
memory to the system after a second without requests, and exits after ten
minutes without requests or launched clients. `hirsel serve --stop` stops
the toolchain's server.

No request holds the server. The server serves one request at a time, and
a client that has waited two seconds behind a running request is refused
and compiles in its own process, so a long request never queues a short
one longer than that; `hirsel serve --stop` is answered at once and stops
the server once the running request ends. Each request has a CPU budget:
the process CPU time the server spends on it, every thread's, 300 seconds
unless the client sets `HIRSEL_RESIDENT_CPU_SECONDS` to another positive
whole number of seconds (any other value fails the request with status 2).
Time a request spends waiting, on its client, a test launch or a linker,
does not count. A request that passes its budget before it is answered is
stopped: its client reports the budget on its standard error and exits
with status 71, the memory limit's status, and is not retried. The server
then refuses the clients it holds, which compile in their own processes,
and restarts in place: it executes its own compiler again in the same
process, which keeps its socket, its lock and its launched clients and
drops every remembered derivation, as after a panic. Clients still waiting
to be accepted are served by the restarted server. A one-shot compile has
no CPU budget: it runs in the client's own process, under its terminal.

A resident compiler takes each request's transport permission from that request
([packages §8](packages.md#8-hirsellock)). While it serves a `--frozen` request it makes no
connection and no name lookup on that request's behalf, even while it serves an ordinary
request at the same time; there is no process-wide offline switch.

## Dev loop

`hirsel dev` rebuilds a local Place as its sources change and keeps its store across
sessions. A failed compile leaves the last activated edit serving.

| Form | Does |
|---|---|
| `hirsel dev [DIR] [--listen ADDR] [--store DIR] [--fresh] [--n-place BYTES] [--json] [-- ARGS...]` | Runs the session. In JSON mode it streams edit receipts with attempted, active and superseded identities. |
| `hirsel dev wait [DIR] [--json]` | Waits for the edit selected at invocation. Exits 0 when it activates, 1 on a failed edit and 75 if the outcome cannot be observed. |
| `hirsel status [DIR] --context dev [--ready] [--json]` | Reads the live session's current serving state. |
| `hirsel dev stop [DIR] [--json]` | Stops the session for the selected package. |

`--n-place` defaults to 16 MiB. The session uses this allowance for describe, install and
serve. `--fresh` starts a new dev-owned store generation. A user-supplied `--store DIR`
persists across sessions. The build options remain fixed for each session.

## Workloads

`hirsel ps [--context NAME|/FILE] [--after NAME] [--json]` lists groups in the selected
authority. A page includes the current activation, health and observation age. Reads
leave cold groups parked.

`hirsel status --group NAME[@INCARNATION] [--ready] [--json]` reads one host snapshot.
Plain status exits 0 for a readable group. Readiness exits 0 for fresh serving evidence,
1 for a known unready state and 75 when evidence is missing or indeterminate. Archive
data outside the host's observation coverage is identified in the result.

`hirsel logs --group NAME[@INCARNATION] [--follow] [--after CURSOR] [--json]` emits
bounded NDJSON pages of lifecycle metadata with a cursor scoped to the host and
incarnation. Use the cursor to resume. A `gap` record requires a fresh snapshot.
The controlled final `end` record carries the finite result and exit. Diagnostic
text, guest bytes and tape frames are excluded.

`hirsel rollback ACTIVATION --group NAME[@INCARNATION] [--wait] [--json]` resolves a
retained release in this incarnation and submits it through the deploy path against
the current predecessor and store.

## Store commands

Directory Store commands grant the installed `store_tool` the source and destination directories. Archive commands grant the installed `archive_tool` a work directory and credentials for the requested operation. [STORE-SPEC §15](../../core/programs/store/STORE-SPEC.md) has the full contract.

| Form | Does |
|---|---|
| `hirsel store backup DIR TARGET` | locks the offline store at `DIR`, checks it and copies its recovered head into the empty directory `TARGET`, which it creates if absent. Prints `backup <store id> head <H>` |
| `hirsel store restore --from SOURCE --target SELECTOR --to DIR [--plan] [--json]` | checks a stamped backup or an archive (`@descriptor.json`) at the selected boundary and records the plan outside the store. With `--plan` it exits 0. Submit the exact digest with `op decide` to publish a fresh child at the reviewed destination. |
| `hirsel store archive describe ARCHIVE [--json]` | reads the binding, holder, intervals, gap and floor from the archive head and index. |
| `hirsel store archive verify ARCHIVE [--deep] [--json]` | checks retained objects and journal history. Deep verification also walks page graphs. |
| `hirsel store archive expire ARCHIVE (--keep-from BASE or --at MS) [--json]` | returns the proposed floor and reviewed head. An approved typed decision uses the expiry credential to advance that floor and reclaim unreachable objects. |
| `hirsel store inspect DIR` | captures the store beside a live writer and prints its identity and head, then each table and index with its state, revision and exact row count |
| `hirsel store stats DIR` | the `inspect` lines with each collection's charged bytes, plus the journal's segments and bytes, the account at the cut, pins and pack rows |
| `hirsel store rehearse DIR --release EXE [--json]` | Runs the candidate migration on a private copy of the offline store. |
| `hirsel store verify DIR` | scrubs a copy of the store and prints each fault |
| `hirsel store verify DIR --release EXE` | on a copy, declares the release's schema, scrubs, and checks every Public index against its table under the release's extractors; prints each changed revision, fault and wrong entry; changes nothing |
| `hirsel store repair DIR --release EXE` | on DIR itself: demotes each index with a wrong entry and builds every index to the end; refuses while a deploy's export lease is held |

`hirsel tape check DIR --recorded WRITER --release WITNESS` checks the live journal on
a copy of an offline Place store. Use the original executable and the same program built
with the other compiler backend. [RUNNER-SPEC §7](../../core/programs/runner/RUNNER-SPEC.md#7-recording-and-re-execution-r5-at-store-spec-v24)
defines the report and verdicts.

`inspect` and `stats` do not open the store. They reserve the prepared capture's computed need
inside `store_tool`'s fixed 1 GiB root, and print `hirsel store <verb> needs N bytes` on stderr
when they succeed. A `Full` refusal prints the need's parts; its remedy line names the need and
the fixed root, then says that no `store_tool` route reads a catalog this large.

A backup opens the store as its writer, so a store another process holds exits 75 and is not
changed. A store a place serves is backed up through its host, which is not built yet. A
restored store keeps its source's identity: restore replaces a lost store and does not clone one.
An interrupted restore leaves a directory that no `open` accepts; remove it and run the restore
again. The `--release` forms exit as the release does when it refuses to start (nonzero,
`store verify found N problems` on stderr).

| Exit status | Meaning |
|---|---|
| 0 | Done |
| 1 | Refused with a reason on stderr, or `verify DIR` found a scrub fault |
| 2 | Usage |
| 75 | A writer holds the store |
| 104 | `store rehearse` or a `--release` form refused the named executable because it is not a place |

## Coverage

*[Coverage](../coverage.md) is the full text.* A coverage build adds reached flags at
function entries and source control-flow arms. It is the same build
([CAP-9](capabilities.md#cap-9-recording-is-outside-the-program)), and no program reads a
flag (R28).

| Form | Does |
|---|---|
| `hirsel test [package-dir] --coverage [--json]` | builds the coverage build, runs the selected tests and reports reached entries and arms; with `--laws`, also each test's law rows |
| `--coverage-out FILE` | also writes the merged profile |
| `--coverage-base FILE` | adds a diff against an earlier profile |
| `--coverage-targets FILE` | restricts the uncovered list to the source ranges in FILE |
| `--coverage-runs` | keeps one row per run in the profile |
| `--seed-base K` | with `--seeds N`, runs seeds K to K+N-1 |
| `hirsel coverage merge FILE|DIR|@LIST... [--map FILE]... [--base FILE] [--targets FILE] [--json] [-o FILE]` | merges profiles, and raw files read against their site maps, and reports; runs nothing |
| `hirsel coverage diff BASE NEW [--json]` | lists the sites reached in one profile and not the other |
| `hirsel coverage diff PROFILE --test DEFINITION [--json]` | lists the sites only the profile's other tests reached, and only that test reached |

Each coverage option implies `--coverage`. A narrowed run is allowed. Each coverage test
build writes its site map beside its executable as `HEX.covmap`. Coverage never changes an
exit code: `hirsel test` exits as it does without it, and `hirsel coverage` exits 0 when it
reports, 1 when it refuses an input (another format version, or inputs from builds of
different sources) and 2 on usage errors.

`hirsel laws` stays static and about the lock: `check` and `write` run no test, and it has
no coverage option. Coverage never enters a law row or `hirsel.laws`.

## Registry publication and routing

Phase 1 routes each scoped package by longest whole-component prefix in the invoking user's
`hirsel/config.hcfg`. A route pins its HTTPS origin, both log keys and independent witnesses.
The default origin is `https://pkg.hirsel.ai`. The command
`hirsel registry publish [--workspace]`<!-- doc-ref: planned --> checks and uploads the member.
The workspace form publishes dependencies first and reports which members committed if a
later member fails. A retry resumes by verifying those releases. `hirsel build --emit recipe --code-store DIR` keeps
its executable recipe meaning. [Registry protocol and client](registry.md)

## Adding a dependency

`hirsel add SOURCE [--as ALIAS] [--root sha256:HEX] [--workspace] [--dry-run] [--package-store DIR] [--json]`<!-- doc-ref: planned -->
binds one dependency in the nearest package manifest and updates `hirsel.lock`. The row's key
is the alias, the last component of the package name unless `--as`<!-- doc-ref: planned --> gives another; the row
holds the package's scoped name, major, root and source. `SOURCE` is an
`https://` URL (one archive), a git source `git+https://HOST/PATH[?dir=DIR][#REF]` or its
shorthand `github:OWNER/REPO[/DIR][@REF]` (a commit, bound by `git` and `rev`; untagged, the
default branch's tip), a `.hpkg` path, a `sha256:` root already in the package store, a toolchain
`NAME@MAJOR` (a bare name), a listed member's directory, or a scoped registry name with optional major or exact version. `add` never moves a root already
bound to the same source; that is `hirsel update`<!-- doc-ref: planned -->. `--workspace` declares it in the root's
`[workspace.dependencies]` and binds the member with `{ workspace = true }`, one manifest at a
time. It verifies any archive and resolves the workspace before publishing the manifest. It
exits 0 for added, updated, unchanged or a dry run, 2 for a refusal before publication, and 1
for a failure after publication begins. [Packages](packages.md#adding-a-dependency)

## Updating dependencies

`hirsel update [ALIAS|ID[@VERSION]] [--dry-run] [--package-store DIR] [--json]`<!-- doc-ref: planned -->
re-reads the named dependency rows' sources (a git row's `ref`, a URL or archive's bytes),
rebinds rows whose source now yields another root, and, for every identity whose edges do not
agree on one root, selects the highest root every edge accepts and writes it to the root
manifest's `[selections]`. It checks the edited workspace on an overlay first and writes
nothing when the check fails. It prints each changed package's versions and roots, each
library now built against another version than it was published with, and each package's
per-export authority diff. Named registry selection verifies inclusion and replays the witnessed
status log. Unqualified add and update wait 72 hours. An exact version bypasses age only. A
retired pin whose sole fix is younger makes update refuse and print the exact upgrade command.
Update refuses `--locked` and `--frozen`, and its exit statuses are
`add`'s. [Packages](packages.md#updating-dependencies)

## Auditing dependencies

`hirsel audit [DIR] [--json]`<!-- doc-ref: planned --> prints, for every package of the
workspace's closure but the toolchain's, its version, root, source and binding row, every edge
whose effective root differs from the one its publisher tested, and its authority card:
carried authority, reached kinds and verdict. It exits 2 when a row's `authority` does not
cover what its package reaches. For registry roots it checks both logs and reports retirement,
takedown and active OSV advisories. Failure to verify a fresh complete status prefix refuses
the audit. [Registry verification](registry.md#3-the-two-logs) [Packages](packages.md#auditing-dependencies)

## CPU profiles

```text
hirsel prof run [package-dir] [build-opts] [--tasks] [--mem[=sampled]] [--out FILE.hprof] [--freq 997] [--pmu LIST] [--stack-bytes 32768] [-- args...]
hirsel prof replay [package-dir] --tape TAPE [--tasks] [--mem[=exact|sampled]] [--out FILE.hprof]
hirsel prof attach <PID> [--duration SECS] [--freq 997] [--pmu LIST] [--debug PATH] [--out FILE.hprof]
hirsel prof summary <FILE.hprof> [--json]
hirsel prof tail <FILE.hprof> [--limit 20] [--json]
hirsel prof schema <FILE.hprof> [--json]
hirsel prof top <FILE.hprof> [--by self|inclusive] [--event NAME] [--group def_path|instance] [--filter GLOB] [--limit 20] [--json]
hirsel prof annotate <FILE.hprof> <DEF_PATH|FILE:LINE> [--json]
hirsel prof peak-mem <FILE.hprof> [--limit 20] [--json]
hirsel prof callers <FILE.hprof> <DEF_OR_SYM> [--depth 5] [--json]
hirsel prof callees <FILE.hprof> <DEF_OR_SYM> [--depth 5] [--json]
hirsel prof diff <BASE.hprof> <HEAD.hprof> [--by self|inclusive] [--group def_path|instance] [--min-delta-pct 1.0] [--json]
hirsel prof sql <FILE.hprof> "<SQL>" [--limit 500] [--timeout-ms 5000] [--json]
hirsel prof export <FILE.hprof> --format pprof --out <PATH>
```

For each target, `core-product --install` and `std-product --install` accept
`--prof-runtime ARCHIVE` and `--prof-recording-runtime ARCHIVE` to register
the profiling and combined profiling/recording archives. `hirsel env` lists
both archive paths. `hirsel --version` reports their content identities.

`prof run` takes the native build options of `run`. Its `--tasks` option selects the
profiling runtime and adds task, wake, host-stack and provider I/O rows.
When CPU sampling is unavailable, a task capture still runs and reports the
perf diagnostic in its quality header. It passes arguments after
`--` to the executable, leaves its standard streams connected, and returns
a nonzero guest status as exit 1. `--out` names the profile and defaults to `profile.hprof`. In structured `run` and `prof run`, redirect guest stdout to fd 3 and stderr to fd 4. Result stdout carries one JSON document and preserves the guest exit or signal.
The command writes a [portable SQLite profile](profiling.md) after the
process exits. The sampler resolves verified detached DWARF, embedded DWARF
and ELF symbols.

`prof replay` executes the tape serially with the combined profiling and
recording runtime. `--tasks` captures runtime observations. The replay
verdict determines the command's status. A refused or divergent replay
leaves no profile.

`prof attach` snapshots mappings before sampling a running process without
stopping it. It ends the capture after `--duration`, at process exit, or on
Ctrl+C. `--debug` selects a detached DWARF file to verify when the executable
has no embedded DWARF.

For `prof replay`, use `--mem=exact` for exact peak counters and backing
lifetimes, or `--mem=sampled` for weighted allocation estimates. A bare
`--mem` selects sampled mode. `prof run --mem` samples every live worker and
reports weighted allocated and resident backing bytes at the peak and
last RSS reads, plus allocation rates by callsite. Live
memory is sampled only: `prof run --mem=exact` refuses and names replay.
`prof attach --mem` refuses, because attaching cannot sample allocations
made before it. `prof peak-mem` reads the memory results and budget scopes
from a profile.

Start with `prof summary` for data quality and the five hottest functions
per event. Use `prof schema` for tables and columns, and `prof sql` for
read-only queries. Query commands accept `--limit` and `--json`. The default
limit is 20 rows, or 500 for SQL. JSON includes a `quality` object and a
command-specific `result` object. Unsigned values above 2^53 use decimal strings.

`prof top` defaults to self cost and groups by definition path. `prof diff`
uses those same defaults, refuses incompatible targets, clocks and primary
events or units, and reports unmatched functions. A live-only capture
cannot be compared with replay. A single cycle or wall-time comparison
reports possible sampling noise.

`prof annotate` returns one row per source line, with saved source text and
separate `cpu`, `remarks` and `allocations` arrays. It sums CPU periods and
sample counts per event across the selected functions on each line.

`prof sql` accepts one read-only statement that returns rows. An SQLite
authorizer denies writes, ATTACH, and connection-changing statements.
The result includes the ordered column names, rows, row limit, returned row
count, and a truncation flag. The profiler interrupts a query that exceeds
`--timeout-ms`.

`prof export --format pprof` writes a standard uncompressed pprof protobuf to
the requested path.
