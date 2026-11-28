# Hirsel

Hirsel is a systems-level, general-purpose language whose primary user is a coding agent.
It is procedural with value semantics: loops and local mutation, no raw pointers, no
methods. Every type declares once how its values are represented: own, region data, a
resource, or plain data. Code reads values through lexical views and allocates against
limits whose refusal is the ordinary failure `Capacity`. `Rc`, `Arc` and `Shared` give
explicit access to immutable counted payloads. Every outside effect arrives through a
capability parameter.

This specification defines the v1 core language: its syntax, typing, ownership, memory,
budgets, failure, tasks, capabilities, the meaning of its built-in operations, programs
and diagnostics, and the signatures of the core operations those rules name.

## Fixed goals

These goals decide between designs. A rule that would break one of them is wrong.

1. **An agent can write correct code without global knowledge.** Every top-level
   definition has a complete signature. Ordinary body checking is local
   ([LANG-8](language.md#lang-8-signatures)). [CONC-14](tasks.md#conc-14-flows) defines
   the reach inspection for a flow transform. Before any body is checked,
   **declaration preprocessing** computes each type's summary
   ([OWN-2](ownership.md#own-2-type-summaries)) and each manifest constant's value
   ([LANG-20](language.md#lang-20-constants)) once, in dependency order, from declarations
   alone. A computed constant runs only after checking, and nothing checking decides reads
   its value. An engine lowers each checked definition once; it may call a known callback
   directly, and never checks a body again to do so.
2. **Representation is declared.** A type's declaration fixes whether its values are
   plain, own, region, resource, counted or scoped, and every use inherits it
   ([OWN-1](ownership.md#own-1-representation-is-declared-once)). Costs follow from the
   declaration, not from an analysis.
3. **Safe.** Accepted programs have no undefined behaviour, no data races, no use after
   free and no leaked resources. Native code a program calls through a foreign module is
   trusted, not checked: these goals hold while it keeps the native contract
   ([FFI-1](foreign.md#ffi-1-hirsel-trusts-audited-native-code-and-does-not-contain-it)).
4. **Authority is explicit.** Files, network, clocks, randomness, tasks and output are
   values passed as parameters ([capabilities](capabilities.md)). These bounds bind the
   program's own code. A child process started through an `Exec` grant holds its OS user's
   authority, and a recording of such a run is only as intact as that user's files
   ([EXEC-1](exec.md#exec-1-an-exec-grant-confers-what-its-programs-can-do-and-hirsel-does-not-confine-them)).
5. **Deterministic.** Given the same outside inputs and limits, every run that no run
   stop ends, and that reaches no foreign code, behaves identically on every run of one
   build, including its steps and where it runs out of memory, and the same on every engine
   and build under the conditions in [SEM-8](semantics.md#sem-8-determinism). Step counts,
   exact charges and stack refusal sites may vary by build. A
   program that fits its logical charges fits on every engine
   ([MEM-28](budgets.md#mem-28-charges-are-an-upper-bound)).
6. **One meaning.** No flag, build mode, optimiser or engine changes what is accepted or
   what a program means ([PROG-6](programs.md#prog-6-one-meaning)).
7. **One accurate rule and site per refusal.** Each refusal names the one rule the program
   breaks and where ([PROG-7](diagnostics.md#prog-7-one-rule-one-site)).

## Contents

| chapter | rules |
|---|---|
| [Syntax](syntax.md) | LANG-1 to LANG-5 |
| [Language](language.md) | LANG-6 to LANG-21 |
| [Ownership](ownership.md) | OWN-1 to OWN-26 |
| [Memory](memory.md) | MEM-1 to MEM-18, MEM-29; performance targets MEM-31 to MEM-32 |
| [Budgets](budgets.md) | MEM-19 to MEM-28, MEM-30, MEM-33, MEM-34 |
| [Failure](failure.md) | FAIL-1 to FAIL-17 |
| [Tasks](tasks.md) | CONC-1 to CONC-15, CONC-17 |
| [Capabilities](capabilities.md) | CAP-1 to CAP-10 |
| [Semantics](semantics.md) | SEM-1 to SEM-11 |
| [Programs](programs.md) | PROG-1 to PROG-6, PROG-9 to PROG-11 ([Map index](../internal/map-index.md) is an implementation document) |
| [Diagnostics](diagnostics.md) | PROG-7 and PROG-8 |
| [CLI](cli.md) | |
| [Profiling](profiling.md) | |
| [Packages](packages.md) | |
| [Launch](launch.md) | |
| [Foreign calls](foreign.md) | FFI-1 to FFI-16 |
| [Process creation](exec.md) | EXEC-1 to EXEC-8 |
| [In-process code loading](packing.md) | PACK-1 to PACK-17 |
| [SDK](sdk.md) | |
| [Registry](registry.md) | |
| [Fleet](fleet.md) | FLT-1 to FLT-8 |
| [Compute](compute.md) | COMP-1 to COMP-9 |
| [Embedding](embed.md) | EMB-1 to EMB-9 |
| [Core library](core.md) | |
| [Reserved](reserved.md) | |

## Reading this specification

[Worked examples](../examples.md) show eight complete programs and their outcomes.

Each rule is a `### ID: Title` heading followed by one normative statement and, where it
refuses programs, the smallest accepted and refused programs. MEM-31 and MEM-32 keep rule
headings but are non-normative performance targets
([performance targets](memory.md#performance-targets)). Rule IDs are numbered from
1 within each rule family, and each term is defined once, in bold, where it is
introduced. A refusal comment `// refused: ID` names the rule a refusal cites. The
[transition table](ownership.md#own-26-the-transition-table) is the single authority for
what every operation does to ownership, charges and failures. Examples are informative.
A plain Hirsel fence is a self-contained program fragment: it declares each type it uses that
the core library does not, and a block whose comment says `refused` is refused only where
the comment says. A block marked `` ```hirsel unbuilt `` belongs to its rule but shows what the
built `hirsel` does not implement yet; the mark is removed when it does, and the spec
test fails on a marked block that is already refused or accepted as its comments say.
Examples that require a package manifest link to their fixtures.
`` ```hirsel core `` uses the core declaration rules. A
`` ```hirsel signatures package/module `` block lists public declarations from that package
module. Each declaration must match the exported spelling, including the parameter names,
parameter modes and types, and result type. A `` ```hirsel run `` block is a complete
program. The next fence is `text` with its exact stdout, followed by an `Outcome:`
line that states success or failure.

The [Net socket declarations and ownership rules](net.md) define the network
capability and its explicit grant syntax.
The [TLS and secrets specification](tls.md) defines the client TLS provider
operations and `Secret`.
The [DTLS and SRTP specification](dtls.md) defines the sans-IO DTLS 1.2 endpoint, its
certificate fingerprints and SRTP exporter, and SRTP/SRTCP sessions (built; `dtls@1`, `srtp@1`).
The [RTP and RTCP specification](rtp.md) defines the sans-IO `rtp` package: headers and
extensions, RTCP, retransmission, congestion control, pacing and stream rewriting (built; `rtp@1`).
The [HTTP specification](http.md) defines std `http` and the Runner's HTTP bindings,
including keyed operations.
The [WebRTC specification](webrtc.md) defines the `webrtc_sfu` package (built; `webrtc_sfu@1`, RTC-1 to RTC-9; RTC-10 pooled rooms specified, not built):
JSEP SDP, the sans-IO PeerSession, the room router with dynacast, active speakers,
the revocation fence, `media_record`, and one service serving many rooms from one unit pool.
The [QUIC specification](quic.md) defines the sans-IO QUIC v1 endpoint written in Hirsel: RFC 9000, 9001, 9002 and 9221, a QUIC-only TLS 1.3 handshake with pinned peers, and NET-16 steered connection IDs (specified, not built).
The [WebTransport specification](webtransport.md) defines WebTransport over HTTP/3 (draft-ietf-webtrans-http3-17) with static-only QPACK and browser certificate hashes (specified, not built).
The [MoQ specification](moq.md) defines the Media over QUIC endpoint and relay on draft-ietf-moq-transport-23 (specified, not built).
The [SFU cascade specification](sfu_cascade.md) defines rooms that span SFU nodes over QUIC links, with a cross-node revocation fence that fails closed under partition (specified, not built; its room directory is a contract with an in-memory fake, and the Store adapter comes later).
The [Fleet specification](fleet.md) defines single-binary multi-machine orchestration over `objects@1`: the three workload archetypes, `ClaimAuth` + `ORIGIN` move fencing, controller takeover and scoped barriers, dedicated `Directory[K]` Places, proof-ready kernel rules K1–K13, and verification tiers T0–T3.
The [Embedding specification](embed.md) defines static libraries, the embedded C ABI, Swift bindings, Apple target records and iOS target availability.
The [in-process code loading specification](packing.md) defines how a pack host installs compiled Hirsel code while it runs: the `.hirsel-mod` v1 artifact, the out-of-process capped `Compiler` service, the write-xor-execute code slab, the counted `Code[F]` handle with `activate`, the closed import mask, the PC table, the hot context line and fused scope rows (PACK-1 to PACK-17).
The [Compute specification](compute.md) defines the `compute@1` worker, credit desk and dispatcher reducers for Archetype C.
The [TURN specification](turn.md) defines the sans-IO TURN client and relay, long-term credentials and the TCP fallback (built; `turn@1`).
The [SFrame specification](sframe.md) defines the `sframe` package: RFC 9605 end-to-end media encryption with AES-128-GCM, the sender-key ratchet, a replay window, and a ranged framing that keeps codec headers clear and authenticated (built; `sframe@1`, v4; v5 TLS-8 schedule slice E1 planned).
The [packages specification](packages.md) defines `hirsel.hcfg`, `hirsel.lock`, local
workspaces, library roots and package archives.
The [Regex specification](regex.md) defines std `regex`, regular expressions over
`String` with the Rust `regex` crate's syntax and meaning.
The [Unicode text specification](unicode.md) defines std's Unicode character classification,
general category and case conversion in `char` and `string`, pinned to Unicode 18.0.0.
The standard library contracts define [scripted simulation plans](../../core/std/api/sim/world.md#scripted-plans)
and [shrinking and replaying them with `std.prop`](../../core/std/api/prop.md#scripted-simulation).
The `std.sim` contracts also define [named protocol message holds](../../core/std/api/sim/message.md)
and [per-node clock pauses](../../core/std/api/sim/world.md#several-clocks).
[SM1 trace export](../../core/std/api/sim/trace.md) renders one opt-in codec tape per seed
as NDJSON for TLC trace validation or ITF for Quint and Apalache model-based testing.

The [process creation specification](exec.md) defines the `Exec` capability over
operator-approved launch profiles, spawning, waiting, signalling, killing a dropped child,
and served Exec; the Net chapter defines Unix-domain socket rules, NET-12 to NET-14.

The [foreign calls specification](foreign.md) defines foreign modules over pinned native
archives, the `Foreign[I]` capability, the call ABI and its handles, and the `sqlite` package.

## Glossary

| term | defined in |
|---|---|
| alias, scoped name, local name, package version | [packages §1](packages.md#1-canonical-library-content) |
| anchor | [OWN-19](ownership.md#own-19-scoped-values) |
| ascription | [LANG-5](syntax.md#lang-5-grammar), [LANG-11](language.md#lang-11-typing-is-local) |
| as if, observables | [SEM-8](semantics.md#sem-8-determinism) |
| authority ceiling, reached kinds, authority card | [packages §11](packages.md#11-dependency-authority) |
| authentication: none in Hirsel or the runner; application code binds the Caller | [HTTP §4](http.md#4-keyed-operations), RUNNER-SPEC rule 13 |
| blob | [MEM-9](memory.md#mem-9-compact-types) |
| codec | [SEM-10](semantics.md#sem-10-codecs) |
| tombstone | [SEM-10](semantics.md#sem-10-codecs) |
| cancellation point | [CONC-7](tasks.md#conc-7-cancellation-points) |
| capability | [CAP-1](capabilities.md#cap-1-authority-is-explicit) |
| capability reachability | [CAP-10](capabilities.md#cap-10-reachability) |
| claim function, keyed operation | [K-2](http.md#k-2-the-principal-and-the-key-come-from-application-code), [K-1](http.md#k-1-the-declaration) |
| commit point | [FAIL-3](failure.md#fail-3-commit-points) |
| computed constant, manifest constant, assertion | [LANG-20](language.md#lang-20-constants) |
| constant budget, package comptime budget | [MEM-34](budgets.md#mem-34-constant-budgets) |
| copyable | [OWN-3](ownership.md#own-3-kinds) |
| current limit, limit | [MEM-19](budgets.md#mem-19-limits) |
| exact binding, floor binding, tested root, selection, effective binding | [packages §1, §5](packages.md#5-resolution) |
| event | [CONC-10](tasks.md#conc-10-event-order) |
| extent, view | [OWN-8](ownership.md#own-8-views) |
| failure, error, run stop | [FAIL-1](failure.md#fail-1-errors-failures-and-run-stops) |
| foreign module, foreign function, interface, handle, loan, helper, foreign-reaching, environmental termination | [foreign](foreign.md) |
| grant | [MEM-27](budgets.md#mem-27-finalizer-grants) |
| construction in place, inline bytes | [MEM-31](memory.md#mem-31-construction-in-place) |
| engine obligations | [MEM-32](memory.md#mem-32-engine-obligations) |
| identity bytes | [PROG-9](programs.md#prog-9-code-descriptors) |
| inert | [OWN-4](ownership.md#own-4-bounds) |
| hand-over, limit record | [MEM-20](budgets.md#mem-20-limit-records-and-hand-over) |
| kind | [OWN-3](ownership.md#own-3-kinds) |
| lane, mask, vector, version | [SEM-11](semantics.md#sem-11-vectors) |
| launch profile, argument form, cwd label | [EXEC-2](exec.md#exec-2-launch-profiles-and-narrowing) |
| root label | [NET-12](net.md#net-12-unix-rules) |
| library root, package archive, package store | [packages §1, §7](packages.md#1-canonical-library-content) |
| lock, stale lock | [packages §8](packages.md#8-hirsellock) |
| mapping propagate | [LANG-18](language.md#lang-18-errors) |
| model world, simulation | [CAP-9](capabilities.md#cap-9-recording-is-outside-the-program) |
| outlives | [MEM-6](memory.md#mem-6-roots) |
| packed, streamable | [MEM-12](memory.md#mem-12-region-sequences) |
| pending result | [FAIL-9](failure.md#fail-9-pending-results) |
| place, slot | [checker state](ownership.md#checker-state) |
| Place, hosted program instance | [Place activation](launch.md#place-activation) |
| provider | [CAP-5](capabilities.md#cap-5-provider-operations) |
| punned field | [LANG-9](language.md#lang-9-nominal-types) |
| recording, recording build | [CAP-9](capabilities.md#cap-9-recording-is-outside-the-program) |
| region data, region value | [MEM-2](memory.md#mem-2-region-data) |
| replay point | [CAP-8](capabilities.md#cap-8-the-replay-point) |
| reservation | [OWN-11](ownership.md#own-11-call-arguments) |
| scoped | [OWN-19](ownership.md#own-19-scoped-values) |
| slice end, step, step bound | [MEM-30](budgets.md#mem-30-step-bounds) |
| shield | [CONC-7](tasks.md#conc-7-cancellation-points) |
| sub-view | [OWN-8](ownership.md#own-8-views) |
| type summary | [OWN-2](ownership.md#own-2-type-summaries) |
| workspace, member, top member | [packages §4, §5](packages.md#4-workspaces) |
| writable | [OWN-5](ownership.md#own-5-availability) |
| writer | [MEM-4](memory.md#mem-4-sized-destinations) |
