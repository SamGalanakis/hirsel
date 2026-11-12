# Reserved

What later versions may add, and what v1 already guarantees so that the additions change
no accepted v1 program. Each clause is normative for v1: a v1 rule that contradicts one is
an error in this specification.

## Reservation clauses

Each clause is numbered as in the risk audit it comes from, and says where v1 meets it.

| clause | v1 guarantee | where |
|---|---|---|
| R1 | `send` is checked from declarations and capture lists; `Rc`, scoped and stable values are never `send`; every sent block keeps a limit that outlives its holders, ultimately the root | CONC-2, MEM-22 |
| R2 | Sent, received and joined values keep their original charge and are refunded to that limit; queue storage is admitted at creation; spawn fails before the task exists; join admits nothing; cross-task charges and refunds are ordered events. v1 meets this with per-task limits that pass surviving charges to their parent, not with a separate shared domain | MEM-22, CONC-4, CONC-6, CONC-10 |
| R3 | `Arc` payloads are `shareable + send` and immutable; its count operations are ordered events. Mutable shared handles (mutexes, atomics) will be separate types holding no authority, never `shareable`, with exclusive scoped access and a failed access releasing the payload unseen | MEM-15, CONC-11, CONC-12 |
| R4 | Own layout and the calling convention are private; only compact stable layout is declared. A later layout or foreign declaration never reinterprets an existing type | MEM-9, MEM-13 |
| R5 | No foreign code in v1. Foreign loans will last only for the call; no failure will unwind across foreign frames; foreign operations will need explicit authority and stay outside replay unless a provider models them | not in v1 |
| R6 | `String`/`List` and `Text`/spans are distinct types; `FixedSpan`, `SeqSpan` and `IndexedSpan` are distinct and only fixed and indexed spans index; compact fields declare inline or linked storage and links stay in one blob; wire formats are separate; optimisation keeps declared shapes | MEM-9 to MEM-12, MEM-30 |
| R7 | Literals evaluate fields before admission; `build_in` admits before its writer runs; `prepare_in` copies; only completed builders are public; `finish` cannot fail or allocate; failed prefixes stay charged until close | MEM-3, MEM-4, FAIL-4, FAIL-6 |
| R8 | A later `isolate` will catch a failure only after the failing scopes are cleaned, leaving installed mutations; host death is never caught; `shield` will defer cancellation only. v1 cleans every scope on every exit edge already, so catching adds no new state; recovering will reserve a replacement primary cause cell before failure-capable code resumes, and if that admission fails the recovery returns a bare `Capacity` and leaves the existing chains unchanged | FAIL-8, FAIL-9, FAIL-14 |
| R9 | Recording a failure and its causes never admits: cause storage is admitted with each task and resource; causes are never truncated and cleanup is never skipped | FAIL-10, MEM-26 |
| R10 | Every outside input and every scheduling or cross-task choice passes the replay point, which can supply outcomes without host effects; pure code needs no events or yields | CAP-5, CAP-8, CONC-7, CONC-10 |
| R11 | No typed codec, equality or hash dictionaries in v1. Later typed operations will take explicit behaviour values; decoding will validate into fresh ownership | not in v1 |
| R12 | Later derivation will be opt-in, owner-local, keep field order and opacity, and produce ordinary checked code; adding a derive never gives a generic body a new obligation. v1 bounds are explicit and rigid | OWN-4, LANG-12, PROG-2 |
| R13 | Later code descriptors will carry the definition path and signature (PROG-5) plus a semantic code revision, start with top-level monomorphic definitions, and never serialise captures | PROG-5 |
| R14 | No `@noalloc` in v1. A result type without `Capacity` does not show that a function allocates nothing; finalizer allocation is declared by `grant`. A later `@noalloc` will mean no admission on any path, cleanup included | MEM-23, MEM-29 |
| R15 | `View[T]` promises typed access and an anchor, not a pointer; a later raw-byte operation will need an explicit contiguity contract | OWN-8 |
| R16 | Scoped types declare every anchor; their owned fields make them affine; they never escape their anchors, enter storable fields or move to a task; ordinary generic `T` stays storable | OWN-19, OWN-22, OWN-23 |
| R17 | Refusals promise one accurate rule and site, not a fixed first diagnostic | PROG-7, PROG-8 |
| R18 | A later task-local append log will admit before commit, expose no read while open, and hand over its events without allocating; its items are independently owned and never scoped, stable or resources | not in v1 |
| R19 | Capability types declare their authority by type; narrowing never regains a parent's grant; no inferred effect list replaces a declared one | CAP-2, CAP-3, CAP-7 |
| R20 | Every allocation is charged to the current limit; a bound scope covers own, counted, region and task storage; its reservation debits the parent and its cleanup precedes its refund. v1 lets own values leave a bound scope by handing their blocks to the parent at exit, which already holds those bytes, so no value outlives the limit that funds it | MEM-22, MEM-27 |
| R21 | Results that borrow name their anchors; a later cursor or iterator protocol will distinguish yields anchored to input from yields anchored to the cursor, and storable iterators will never hold a view | OWN-20, OWN-21 |
| R22 | Dropped handles cancel and wait shielded. A later completion-selection primitive will be provider-ordered, separate from join, and clean its losers before returning | CONC-8 |
| R23 | Later host operations will state committed prefixes, effect-free cancellation and unknown outcomes separately, with real sync and fencing guarantees; shielding never promises rollback across process death | CAP-5 |

## Hooks for later libraries

| hook | v1 | later, without changing v1 programs |
|---|---|---|
| O, scoped ownership and authority | scoped types, scoped callbacks, declared anchors, explicit regions and limits | further scoped types, bounds and owned adapters that admit forms v1 refuses, never loosening an existing loan |
| F, failure boundary | cleanup on every exit edge; one owner per survivor; ordered causes | explicit `isolate` and `shield`; old functions keep failing to the task boundary |
| C, derivation | nominal identity, opacity and rigid bounds; field order is declaration order | derive syntax, typed shape traversal, codec bounds and formats |
| D, code descriptors | callable types carry modes and `uses`; code identity | opt-in descriptors, registries and typed dispatch adapters |
| L, append-only log | none needed | the `Log[E]` type of R18 and its scoped wrappers |
| I, providers | every effect through a capability and its provider; `waits` declared | more operations, recording decorators, versioned request formats |
| T, tasks | spawn, join, channels, cancellation points, `Arc`, the event order | executors, completion selection, shared limits, mutexes, atomics, distributed waits |

## Not in v1

User-made shared limits; mutexes, atomics and cells; weak counted references; catching
failures; deadlines and timeouts; task sets and selection; `@noalloc`; compile-time
evaluation beyond `const`; unsafe code and foreign functions; layout declarations other
than `@compact`; methods, traits and implementation contracts; imports; list literals;
format strings; `forget`; persistent mutable views; derivation and codecs; contracts and
facts; the store, runners and places; recording formats.
