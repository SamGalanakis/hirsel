# Hirsel

Hirsel is a systems-level, general-purpose language whose primary user is a coding agent.
It is procedural with value semantics: loops and local mutation, no raw pointers, no
methods. Every type declares once how its values are represented: own, region data, a
resource, or plain data. Values are read through lexical views, shared only
through explicit `Rc` and `Arc`, and allocated against limits whose refusal is the ordinary failure
`Capacity`. Tasks share nothing, and every outside effect arrives through a
capability parameter.

This specification defines the v1 core language: its syntax, typing, ownership, memory,
budgets, failure, tasks, capabilities, the meaning of its built-in operations, programs
and diagnostics, and the signatures of the core operations those rules name.

## Fixed goals

These goals decide between designs. A rule that would break one of them is wrong.

1. **An agent can write correct code without global knowledge.** Every top-level
   definition has a complete signature, and checking a definition reads only its own body
   and the declarations it names ([LANG-8](language.md#lang-8-signatures)). No rule needs a
   solve, a fixpoint or a walk over other definitions' bodies. Before any body is checked,
   **declaration preprocessing** computes each type's summary
   ([OWN-2](ownership.md#own-2-type-summaries)) and each constant's value
   ([LANG-20](language.md#lang-20-constants)) once, in dependency order, from declarations
   alone. An engine lowers each checked definition once; it may call a known callback
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
   ([EXEC-1](exec.md#exec-1-an-exec-grant-confers-what-its-programs-can-do-and-hirsel-does-not-confine-them), unbuilt).
5. **Deterministic.** Given the same outside inputs and limits, every run that no run
   stop ends, and that reaches no foreign code, behaves identically on every engine and build ([SEM-8](semantics.md#sem-8-determinism)),
   including where it runs out of memory ([MEM-24](budgets.md#mem-24-logical-charges)).
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
| [Memory](memory.md) | MEM-1 to MEM-18, MEM-29 |
| [Budgets](budgets.md) | MEM-19 to MEM-28, MEM-30 |
| [Failure](failure.md) | FAIL-1 to FAIL-16 |
| [Tasks](tasks.md) | CONC-1 to CONC-13 |
| [Capabilities](capabilities.md) | CAP-1 to CAP-9 |
| [Semantics](semantics.md) | SEM-1 to SEM-11 |
| [Programs](programs.md) | PROG-1 to PROG-6, PROG-9 to PROG-11 |
| [Diagnostics](diagnostics.md) | PROG-7 and PROG-8 |
| [Packages](packages.md) | |
| [Foreign calls](foreign.md) | FFI-1 to FFI-9 |
| [Process creation](exec.md) | EXEC-1 to EXEC-8 |
| [Core library](core.md) | |
| [Reserved](reserved.md) | |
| [Examples](examples.md) | |

## Reading this specification

Each rule is a `### ID: Title` heading followed by one normative statement and, where it
refuses programs, the smallest accepted and refused programs. Rule IDs are numbered from
1 within each family without gaps, and each term is defined once, in bold, where it is
introduced. A refusal comment `// refused: ID` names the rule a refusal cites. The
[transition table](ownership.md#own-26-the-transition-table) is the single authority for
what every operation does to ownership, charges and failures. Examples are informative.
Every code block is a self-contained program fragment: it declares each type it uses that
the core library does not, and a block whose comment says `refused` is refused only where
the comment says. A block marked `hirsel unbuilt` belongs to its rule but shows what the
built `hirsel` does not implement yet; the mark is removed when it does.
`hirsel core` uses the core declaration rules. A `hirsel run` block is a complete
program. The next fence is `text` with its exact stdout, followed by an `Outcome:`
line that states success or failure.

The [Net socket declarations and ownership rules](net.md) define the network
capability and its explicit grant syntax.
The [TLS and secrets specification](tls.md) defines the client TLS provider
operations and `Secret`.
The [HTTP specification](http.md) defines std `http`, the runner's HTTP bindings
and keyed operations. It is unbuilt design.
The [packages specification](packages.md) defines `hirsel.toml`, `hirsel.lock`, local
workspaces, library roots and package archives. It is unbuilt design.
The [Regex specification](regex.md) defines std `regex`, regular expressions over
`String` with the Rust `regex` crate's syntax and meaning.
The [Unicode text specification](unicode.md) defines std's Unicode character classification,
general category and case conversion in `char` and `string`, pinned to Unicode 18.0.0. It is
unbuilt design.

The [process creation specification](exec.md) defines the `Exec` capability over
operator-approved launch profiles, spawning, waiting, signalling, killing a dropped child,
and served Exec. It is unbuilt design, as are the Net chapter's Unix-domain socket
rules, NET-12 to NET-14.

The [foreign calls specification](foreign.md) defines foreign modules over pinned native
archives, the `Foreign[I]` capability, the call ABI and its handles, and the `sqlite` package.

## Glossary

| term | defined in |
|---|---|
| anchor | [OWN-19](ownership.md#own-19-scoped-values) |
| ascription | [LANG-5](syntax.md#lang-5-grammar), [LANG-11](language.md#lang-11-typing-is-local) |
| as if, observables | [SEM-8](semantics.md#sem-8-determinism) |
| authentication: none in Hirsel or the runner; application code binds the Caller | [HTTP §4](http.md#4-keyed-operations), RUNNER-SPEC rule 13 |
| blob | [MEM-9](memory.md#mem-9-compact-types) |
| codec | [SEM-10](semantics.md#sem-10-codecs) |
| tombstone | [SEM-10](semantics.md#sem-10-codecs) |
| cancellation point | [CONC-7](tasks.md#conc-7-cancellation-points) |
| capability | [CAP-1](capabilities.md#cap-1-authority-is-explicit) |
| claim function, keyed operation | [K-2](http.md#k-2-the-principal-and-the-key-come-from-application-code), [K-1](http.md#k-1-the-declaration) |
| commit point | [FAIL-3](failure.md#fail-3-commit-points) |
| copyable | [OWN-3](ownership.md#own-3-kinds) |
| current limit, limit | [MEM-19](budgets.md#mem-19-limits) |
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

## Status

This is the specification of the v1 core language, which the `hirsel` compiler
implements. The previous specification and its corpus were deleted at M3.
