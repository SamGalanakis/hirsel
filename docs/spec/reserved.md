# Reserved

What later versions may add, and what v1 already guarantees so that the additions change
no accepted v1 program. Each clause is normative for v1: a v1 rule that contradicts one is
an error in this specification. Where a clause describes a later mechanism, only the
guarantee it asks of v1 binds v1.

## Reservation clauses

Each clause is numbered as in the risk audit it comes from. Clauses R3, R5, R8, R11, R13,
R14, R15, R18, R19, R21 and R23 are quoted from that audit; the line after each says where
v1 meets it.

**R1.** `send` is checked from declarations and capture lists; `Rc`, scoped and stable
values are never `send`; every sent block keeps a limit that outlives its holders,
ultimately the root. *v1:* CONC-2, MEM-20.

**R2.** Sent, received and joined values keep their original charge and are refunded to
that limit; queue storage is admitted at creation; spawn fails before the task exists;
join admits nothing; cross-task charges and refunds are ordered events. *v1:* MEM-20,
CONC-4, CONC-6, CONC-10, with per-task limits whose records hand over to their parent
instead of a separate shared domain.

**R3.** "Arc requires immutable `shareable + send` payloads and exposes no mutable view;
count/recovery/release operations join the domain order. Mutable shared handles are
separate authority-bearing types, excluded from shareable/stabledata; mutex payloads
contain no authority-bearing handles/grants, access is a scoped exclusive callback with no
authority, a failed callback permanently hides/releases its payload, and safe atomics are
sequentially consistent." *v1:* MEM-15, CONC-11, CONC-12; mutexes and atomics are later.

**R4.** Own layout and the calling convention are private; only compact stable layout is
declared. A later layout or foreign declaration never reinterprets an existing type.
*v1:* MEM-9, MEM-13.

**R5.** "Foreign loans last only for the call and may not be retained; ownership transfer
requires an explicit release contract. Foreign callbacks obey declared capture/thread/alias
rules. No language failure unwinds across foreign frames; foreign crash/stack overflow is
environmental termination. Unsafe/foreign operations require explicit authority and are
outside replay unless an explicit model/provider contract admits them." *v1:* no foreign
code; FAIL-15 makes stack exhaustion a run stop.

**R6.** `String`/`List` and `Text`/`FixedSpan` are distinct types; compact fields declare
inline or linked storage and links stay in one blob; wire formats are separate;
optimisation keeps declared shapes. Sequences of variable-size or stable elements, and an
indexed form of them, will be new types; they are planned for v1 and open in this revision
(below). *v1:* MEM-9 to MEM-12, MEM-28.

**R7.** Literals evaluate fields before admission; `build_in` admits before its writer
runs; `clone_in` copies a published blob whole; only completed builders are public; `finish` cannot fail or
allocate; failed prefixes stay charged until close. *v1:* MEM-3, MEM-4, FAIL-4, FAIL-6.

**R8.** "A future isolate catches language failure only after inner loans, owners,
captures and pending results are cleaned; outer installed mutations/effects persist.
Public mutating APIs preserve initialized ownership and their abstraction invariants on
failure or poison/discard the affected state. Environment death is not catchable.
`shield(body)` defers cooperative cancellation only and does not suppress bug failure."
*v1:* FAIL-8, FAIL-9; FAIL-14 says no v1 code may rely on a failure ending its task.
Recovering will reserve a new primary cause cell before failure-capable code resumes; if
that admission fails, the recovery returns a bare `Capacity` and leaves the chain as it is.

**R9.** Recording a failure and its causes never admits: cause storage is admitted with
each task and resource; causes are never truncated and cleanup is never skipped. *v1:*
FAIL-10, MEM-24, MEM-27.

**R10.** Every outside input and every scheduling or cross-task choice passes the replay
point, which can supply outcomes without host effects; pure code needs no events or
yields. *v1:* CAP-5, CAP-8, CONC-7, CONC-10.

**R11.** "Typed operations take explicit behavior dictionaries:
`encode(view T,mut Sink,Codec[T])->Result[(),EncodeError]`;
`decode(view Bytes,place,Codec[T])->Result[T,DecodeError|Capacity]`; Eq/Hash/Format are
separate declared behaviors. Container equality/hash/order dictionaries have coherent
branded identities. Codec version/schema/order are explicit; decoding validates outside
bytes into fresh ownership and grants no authority or brands. Stable decoding uses a
separate result signature naming its destination root." *v1:* no codecs or dictionaries;
SEM-5 fixes one equality and hash per key type.

**R12.** Later derivation will be opt-in, owner-local, keep field order and opacity, and
produce ordinary checked code; adding a derive never gives a generic body a new
obligation. Typed codecs ship together with derivation. *v1:* OWN-4, LANG-12, PROG-1,
PROG-2: a type's identity is its definition identity, so no two declarations can be
confused, and v1 bounds are explicit and rigid.

**R13.** "Selected entry descriptors carry a versioned nominal path/signature and semantic
code/runtime-contract identity; generic identity includes type/size/dictionary/generator
dependencies, never backend addresses/clone numbers. Start with closed named monomorphic
wrappers. Requests resolve only to registered typed decode/call/encode adapters; IDs grant
no code execution authority and serialize no closure captures." *v1:* PROG-5, PROG-6,
SEM-8; the revision hash itself is later.

**R14.** "Callable and erased-value signatures carry independent invocation-allocation and
cleanup-allocation summaries; omission conservatively permits allocation. `noalloc_drop`
is an explicit generic obligation. Absence of Capacity in Result proves nothing. Future
`@noalloc` means no own/counted/region allocation or growth admission on any path,
including cleanup; fixed local scalar storage is permitted." *v1:* every v1 callable type
omits the invocation summary, so a call through it may allocate (MEM-21), and its cleanup
summary is fixed: a closure's cleanup admits nothing (FAIL-13); finalizer allocation is
declared by `grant` (MEM-27).

**R15.** "`View[T]` promises typed access and an anchor, not a contiguous aligned pointer.
A raw-byte/FFI operation requires an explicit contiguous/alignment contract or conversion;
column and packed projections retain the same lexical freeze/overlap rules. New layout
attributes do not change existing unannotated declarations." *v1:* OWN-8, MEM-13.

**R16.** Scoped types declare every anchor; their owned fields make them move; they never
escape their anchors, enter storable fields or move to a task; ordinary generic `T` stays
storable. *v1:* OWN-3, OWN-19, OWN-22, OWN-23.

**R17.** Refusals promise one accurate rule and site, not a fixed first diagnostic. *v1:*
PROG-7, PROG-8.

**R18.** "Reserve an opaque task-local append capability:
`append(view Log[E],take E)->Result[(),(Capacity,E)]`; append admits before commit, exposes
no read/count/clear, and `finish(take Log[E])->OwnedEvents[E]` transfers without
allocation. E is independently owned inert data, never scoped/stable/resource. This does
not allow writes through ordinary frozen views; queries cannot report success after a
failed tracking append." *v1:* OWN-8 names this as the one reserved exception to "nothing
is written through a view".

**R19.** "Opaque capability facades declare their nominal public authority independently
of a private implementation-authority ceiling, including captured/destructor grants.
Projection restores a field's authority; narrowing cannot regain a parent's grant. No
body's inferred effect list or public concrete grant list substitutes for that declared
facade contract." *v1:* there are no authority summaries: a function's authority is
exactly the capabilities in its arguments, and an owning closure captures none (CAP-3,
LANG-14), so a later declared facade restricts only what v1 already passes explicitly.
CAP-7.

**R20 (amended).** Every allocation is charged to the current limit, except collection growth, which keeps its storage's limit (MEM-26, MEM-27); a bound scope covers
own, counted, region and task storage; its reservation debits the parent and all of its
cleanup precedes its refund. *Amended clause:* values a scope funded may leave it; at exit
the scope's limit record is handed over to its parent, which already holds those bytes,
so every surviving block is funded by a live limit, a refund costs amortised inverse-Ackermann time, and
`Capacity.available` names the limit the admission was tried against. This replaces the
audit's "child-domain/root values cannot escape". *v1:* MEM-20, MEM-25, MEM-26.

**R21.** "Borrowed reader/cursor/yield signatures name their anchors; a yield rooted in
input and a yield rooted in mutable cursor state are distinct contracts. Generic consumers
use explicit scoped callbacks/bounds, retain invocation/destruction effects, and release
each conflicting view before advance/refill. Ordinary storable Iter never secretly stores a
View." *v1:* OWN-20, OWN-21; SEM-6 iterates only built-in sources.

**R22.** Dropped handles cancel and wait shielded. A later completion-selection primitive
will be provider-ordered, separate from join, and clean its losers before returning.
*v1:* CONC-8.

**R23.** "Host operations specify committed prefixes, no-effect cancellation and unknown
outcomes separately, and expose real sync/fencing/conditional-update guarantees where
needed. Durable providers can prepare before issue and persist completion before delivery.
Shielding never promises rollback/exactly-once effects across process death; revocation
and external readiness are explicit provider outcomes." *v1:* CAP-5, CAP-8; `WriteError`
states the committed prefix of a write; cancellation reaches an operation only at issue.

**R24.** Static pool brands may later let a pool skip its identity check. *v1:* a pool is
library code whose handles are plain data checked at run time (MEM-18); no rule, keyword or
checker state names a brand, so a later brand form will be written with contextual words
and changes the meaning of no v1 program.

## Hooks for later libraries

| hook | v1 | state |
|---|---|---|
| O, scoped ownership and authority | scoped types and callbacks, declared anchors, regions and limits; authority only through arguments | open: authority facades (R19) and static brands (R24) |
| F, failure boundary | cleanup on every exit edge; one owner per survivor; ordered causes; FAIL-14 promises no task end | open: `isolate` has its v1 obligations (R8); `shield` has no v1 form |
| C, derivation and codecs | nominal identity, opacity, rigid bounds, declaration field order | open: the content hash of a declaration for stored data is not specified |
| D, code descriptors | code identity; instance identity; no address or order dependence | open: the semantic revision hash is not specified |
| L, append-only log | the R18 exception named in OWN-8 | open: the `Log[E]` type |
| I, providers | host primitives in core; issue and delivery replay points; `WriteError` | open: providers for `Net`, `Random`, `Stdin`, `Env`, `Process` |
| T, tasks | spawn, join, channels, cancellation points, `Arc`, the event order | settled for v1; selection, shared limits, mutexes and atomics later |

## Not in v1

User-made shared limits; mutexes, atomics and cells; weak counted references; catching
failures; deadlines and timeouts; task sets and selection; `@noalloc`; compile-time
evaluation beyond `const`; unsafe code and foreign functions; layout declarations other
than `@compact`; methods, traits and implementation contracts; imports; list and map
literals (an `Inline` array literal is not one); format strings; `forget`; static brands;
conversion of errors by `?`; code generic over the numeric types, for which v1 has no bound
(a library writes one function per width; only core operations such as `int.parse` range
over every integer type); `text.as_string`, because `Text` and `String` have
different representations (an author copies with `string.from` of the `Text` view, or keeps
a `String`); generics over an `Inline` length; persistent mutable views; derivation and codecs; contracts and
facts; the store, runners and places; recording formats. A later opt-in execution-budget
scope may stop code at a loop back edge or a call while such a scope is present; no v1
rule assumes that code cannot be stopped there.

## Open in this revision

Each item below is marked at its rule with "OPEN"; implementers must not rely on what the
rule says about it.

| item | rule | what is coming |
|---|---|---|
| Compact sequences of variable-size elements | [MEM-12](memory.md#mem-12-stable-sequences) | Planned for v1: a stable sequence of variable-size elements walked in order, an optional declared index, and a nested in-place builder that writes a document into one blob with no staging copy. They reuse MEM-2's element eligibility (plain or stable) and MEM-4's destinations and `Child` values, and take new type names; the names `SeqSpan` and `IndexedSpan` are kept free for them. |
