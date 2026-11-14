# Hirsel

Hirsel is a systems-level, general-purpose language whose primary user is a coding agent.
It is procedural with value semantics: loops and local mutation, no raw pointers, no
methods. Every type declares once how its values are represented: own, stable in a
region, a resource, or plain data. Values are read through lexical views, shared only
through explicit `Rc` and `Arc`, and allocated against limits that fail with an ordinary
`Capacity` value. Tasks share nothing, and every outside effect arrives through a
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
   plain, own, stable, resource, counted or scoped, and every use inherits it
   ([OWN-1](ownership.md#own-1-representation-is-declared-once)). Costs follow from the
   declaration, not from an analysis.
3. **Safe.** Accepted programs have no undefined behaviour, no data races, no use after
   free and no leaked resources.
4. **Authority is explicit.** Files, network, clocks, randomness, tasks and output are
   values passed as parameters ([capabilities](capabilities.md)).
5. **Deterministic.** Given the same recorded inputs and limits, every run that no run
   stop ends behaves identically on every engine and build ([SEM-8](semantics.md#sem-8-determinism)),
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
| [Budgets](budgets.md) | MEM-19 to MEM-28 |
| [Failure](failure.md) | FAIL-1 to FAIL-16 |
| [Tasks](tasks.md) | CONC-1 to CONC-12 |
| [Capabilities](capabilities.md) | CAP-1 to CAP-8 |
| [Semantics](semantics.md) | SEM-1 to SEM-9 |
| [Programs](programs.md) | PROG-1 to PROG-6, PROG-9 |
| [Diagnostics](diagnostics.md) | PROG-7 and PROG-8 |
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
the comment says.

## Glossary

| term | defined in |
|---|---|
| anchor | [OWN-19](ownership.md#own-19-scoped-values) |
| blob | [MEM-9](memory.md#mem-9-compact-types) |
| cancellation point | [CONC-7](tasks.md#conc-7-cancellation-points) |
| capability | [CAP-1](capabilities.md#cap-1-authority-is-explicit) |
| commit point | [FAIL-3](failure.md#fail-3-commit-points) |
| copyable | [OWN-3](ownership.md#own-3-kinds) |
| current limit, limit | [MEM-19](budgets.md#mem-19-limits) |
| event | [CONC-10](tasks.md#conc-10-event-order) |
| extent, view | [OWN-8](ownership.md#own-8-views) |
| failure, error, run stop | [FAIL-1](failure.md#fail-1-errors-failures-and-run-stops) |
| grant | [MEM-27](budgets.md#mem-27-finalizer-grants) |
| identity bytes | [PROG-9](programs.md#prog-9-code-descriptors) |
| hand-over, limit record | [MEM-20](budgets.md#mem-20-limit-records-and-hand-over) |
| kind | [OWN-3](ownership.md#own-3-kinds) |
| outlives | [MEM-6](memory.md#mem-6-roots) |
| packed, streamable | [MEM-12](memory.md#mem-12-stable-sequences) |
| pending result | [FAIL-9](failure.md#fail-9-pending-results) |
| place, slot | [checker state](ownership.md#checker-state) |
| provider | [CAP-5](capabilities.md#cap-5-provider-operations) |
| replay point | [CAP-8](capabilities.md#cap-8-the-replay-point) |
| reservation | [OWN-11](ownership.md#own-11-call-arguments) |
| scoped | [OWN-19](ownership.md#own-19-scoped-values) |
| sub-view | [OWN-8](ownership.md#own-8-views) |
| twin | [MEM-17](memory.md#mem-17-clone) |
| type summary | [OWN-2](ownership.md#own-2-type-summaries) |
| writable | [OWN-5](ownership.md#own-5-availability) |
| writer | [MEM-4](memory.md#mem-4-sized-destinations) |

## Status

This is the specification of the v1 core language. The compiler and the conformance
corpus are being moved to it; until they are, the implementation follows the previous
specification, which is kept unchanged as test data of the old corpus. That copy is
deleted with the old corpus at M3.
