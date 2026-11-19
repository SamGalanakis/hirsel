# Reserved

What later versions may add, and what v1 already guarantees so that the additions change
no accepted v1 program. Each clause is normative for v1: a v1 rule that contradicts one is
an error in this specification. Where a clause describes a later mechanism, only the
guarantee it asks of v1 binds v1.

## Reservation clauses

Each clause is numbered as in the risk audit it comes from; R25 and R26 come from the
review of compact sequences. Clauses R3, R5, R8, R11,
R14, R15, R18, R19, R21 and R23 are quoted from that audit, and R13 restates its clause
with registration; the line after each says where
v1 meets it.

**R1.** `send` is checked from declarations and capture lists; `Rc`, scoped and region
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
sequentially consistent." *v1:* MEM-15, CONC-11, CONC-12, CONC-13; mutexes are later, and
atomics are not planned: publication is `Latest`, whose operations are sequentially
consistent events that hand out no view, and counting needs nothing shared. No later shared mutable type may hand out a view of storage that another handle can
write, move or free while the view lives ([OWN-8](ownership.md#own-8-views)), through a
non-locking `get_mut`-style accessor or any other.

**R4.** Own layout and the calling convention are private; only compact layout is
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
optimisation keeps declared shapes. *v1:* MEM-9 to MEM-12, MEM-28; `SeqSpan` and
`IndexedSpan` hold variable-size elements in the same blob.

**R7.** Literals evaluate fields before admission; `build_in` and `build_stream_in` admit
one stated size before their writer runs, and nothing infers a size, reruns a writer or
grows a published prefix; `clone_in` copies a value that may hold a link
as its published blob whole, and a streamable value as exactly its complete bytes; only completed builders are public; `finish` and
`finish_trim` cannot fail or allocate; failed prefixes stay charged until close, and only
`finish_trim` refunds, the unused end of a completed builder. *v1:* MEM-3, MEM-4, MEM-12,
MEM-17, FAIL-4, FAIL-6.

**R8.** "A future isolate catches language failure only after inner loans, owners,
captures and pending results are cleaned; outer installed mutations/effects persist.
Public mutating APIs preserve initialized ownership and their abstraction invariants on
failure or poison/discard the affected state. Environment death is not catchable.
`shield(body)` defers cooperative cancellation only and does not suppress bug failure."
*v1:* FAIL-16: `isolate` reserves a cell for the current obligation before its body runs, returns
the refused `Capacity` and leaves the chain as it is when that admission fails, catches
after FAIL-8's cleanup, lets outer effects persist, never catches `Cancelled` or a run stop,
and accepts only a body whose capture descriptor lends nothing `mut`. `shield` has no v1
form, and a commit of several provider operations needs none: cancellation reaches a task
only at a cancellation point and a host operation only at its issue, which completes it
([CONC-7](tasks.md#conc-7-cancellation-points)); an issued operation delivers exactly one
result (CAP-8); a write's error states its committed prefix; a cancelled task's cleanup
finalizes its files, which closes them and ends their locks (FAIL-8,
[core](core.md#contracts-the-signatures-do-not-state)); and `host.sync` makes durable every
byte of a file, through whichever handle it was written. So a commit cancelled between two
of its operations leaves on disk exactly the bytes its issued writes committed, and the
file's lock free, the state a killed process leaves: a reopen that syncs the file and
then reads it recovers both, by the one path.

**R9.** Recording a failure and its causes never admits: cause storage is admitted with
each task and resource; causes are never truncated and cleanup is never skipped. *v1:*
FAIL-10, MEM-24, MEM-27.

**R10.** Every outside input and every scheduling or cross-task choice passes the replay
point, which can supply outcomes without host effects; pure code needs no events or
yields. *v1:* CAP-5, CAP-8, CAP-9, CONC-7, CONC-10.

**R11.** "Typed operations take explicit behavior dictionaries:
`encode(view T,mut Sink,Codec[T])->Result[(),EncodeError]`;
`decode(view Bytes,place,Codec[T])->Result[T,DecodeError|Capacity]`; Eq/Hash/Format are
separate declared behaviors. Container equality/hash/order dictionaries have coherent
branded identities. Codec version/schema/order are explicit; decoding validates outside
bytes into fresh ownership and grants no authority or brands. Stable decoding uses a
separate result signature naming its destination root." *v1:* SEM-10: a codec is the one
dictionary, `Codec[T]` from `codec.of()` for a type with the `codec` bound (OWN-4), and a
decode returns `Result[Result[T, DecodeError], Capacity]` into fresh ownership holding no
authority; its schema and the writer's are explicit, resolved by field name. No region type
is `codec`, so there is no region decoding. Eq, Hash and Format dictionaries are not in v1,
and SEM-5 fixes one equality and hash per key type. An owner replaces a generated codec
only by `via` a codec type, and no owner writes bytes.

**R12.** Later derivation will be opt-in, owner-local, keep field order and opacity, and
produce ordinary checked code; adding a derive never gives a generic body a new
obligation. Typed codecs ship together with derivation. *v1:* OWN-4, LANG-12, PROG-1,
PROG-2: a type's identity is its definition identity, so no two declarations can be
confused, and v1 bounds are explicit and rigid. The one derivation is a codec (SEM-10):
its owner declares `codec` on the definition, which is checked once against the fields'
declared bounds; it is generated in field order as ordinary checked code; an opaque type's
fields stay private, and only its owner's declaration lets decoding build one; and a
generic body that has not written `T: codec` cannot ask for a codec, so declaring it gives
no body a new obligation. With `via`, decoding passes the owner's `from`, so an opaque
type's values are still built only through code its owner wrote.

**R13.** Selected entry descriptors carry a versioned nominal path and signature and a
semantic code and runtime-contract identity; a generic identity includes its type, size,
dictionary and generator dependencies, never backend addresses or clone numbers. An entry
is made by library code at registration, never declared: module `runner` of package
`store` registers a top-level non-generic function `f` by its `StaticFn` with `compute`
(`fn(take A) -> Result[R, Capacity]`), `transact` (`fn(mut Tx, H, take A) -> Result[R,
E]`), `query` (`fn(Tx, H, take A) -> Result[Cover, Capacity]`) or `granted` (`fn(G, take
A) -> Result[R, Capacity] waits`), each with `A`, `R` and `E` bounded `codec`, where `H:
send` is the place's handles (its Tables and Indexes), passed by view and built at open by
the place's `handles(mut db: Db) -> Result[H, store.Error] waits`. The key is `f`'s
identity bytes with the schema fingerprints of `A` and `R`, so a changed codec schema
changes the key; a key already registered is refused and the registry is unchanged. Each
kind has one generic adapter that decodes an `A`, calls `f` and encodes the result, all
inside the call's byte bound and one `isolate` (FAIL-16). There is no `derive wire` form and no hand-written adapter. A
place's entries are exactly those its capability-free `place(mut r: Registry[G, H])`
registers; the server runs it to serve and the SDK generator runs it, bounded, to read
`runner.describe`; no other export list exists. Every task that runs entries runs `place`
itself, because an owning closure is never shareable. Requests resolve only to registered
entries; an ID grants no code execution authority and serializes no closure captures.
The [runner launch contract](launch.md) fixes describe mode and the immutable
SHA-256 executable digest, target and `n_place`, privately supplied by the runtime.
*v1:* PROG-5, PROG-6, PROG-9, SEM-8: closed named functions without type parameters have
identity bytes; the runner is a library over v1, not a language form; the code revision
is `code.revision` (PROG-9).

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
allocation. E is independently owned inert data, never scoped/region/resource. This does
not allow writes through ordinary frozen views; queries cannot report success after a
failed tracking append." *v1:* MEM-29: a `Log[E]` is a handle to a separately admitted
node whose entries no view exposes, `log.finish` returns them as a `List[E]`, and `E` is
inert at every depth.

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
View." *v1:* OWN-20, OWN-21; SEM-6 iterates only built-in sources. A borrowed cursor is
a value of a scoped type holding a view and a position (OWN-22's `Cursor`), anchored by
OWN-20, advanced by functions that take it `mut`, and kept out of every storable value by
OWN-19; a position may equally be a `u64` passed in and returned, as `string.next_scalar`
does. No core type is a cursor and `for` takes no user cursor.

**R22.** Dropped handles cancel and wait shielded. A later completion-selection primitive
will be provider-ordered, separate from join, and clean its losers before returning.
*v1:* CONC-8.

**R23.** "Host operations specify committed prefixes, no-effect cancellation and unknown
outcomes separately, and expose real sync/fencing/conditional-update guarantees where
needed. Durable providers can prepare before issue and persist completion before delivery.
Shielding never promises rollback/exactly-once effects across process death; revocation
and external readiness are explicit provider outcomes." *v1:* CAP-5, CAP-8; `WriteError`
states the committed prefix of a write; cancellation reaches an operation only at issue;
`host.sync` and `host.sync_dir` are the sync barriers, and a failed one is an unknown
outcome ([core](core.md#contracts-the-signatures-do-not-state)). Fencing and conditional
update are later.

**R24.** Static pool brands may later let a pool skip its identity check. *v1:* a pool is
library code whose handles are plain data checked at run time (MEM-18); no rule, keyword or
checker state names a brand, so a later brand form will be written with contextual words
and changes the meaning of no v1 program.

**R25.** `@varint` is reserved as a later opt-in encoding of a compact integer field, or of
a `Text` length whose value is known before it is written. *v1:* every compact encoding is
the fixed width of MEM-9 and MEM-12; a later `@varint` changes no unannotated declaration,
and the counts and byte lengths of sequences, which are patched at close, stay fixed width.

**R26.** Documents that grow in segments, or are committed in pieces, will be new types
with their own charge contract. *v1:* `SeqSpan` and `IndexedSpan` are one blob of one
admitted size (MEM-4); a later type never reinterprets them, moves a published value or
turns `finish` into a copy, flattening it into a blob is an admitted copy, and no
allocator chunk size becomes a logical capacity (MEM-28).

## Hooks for later libraries

| hook | v1 | state |
|---|---|---|
| O, scoped ownership and authority | scoped types and callbacks, declared anchors, regions and limits; authority only through arguments | open: authority facades (R19), static brands (R24) and callable results anchored to a place ([LANG-13](language.md#lang-13-callables)) |
| F, failure boundary | cleanup on every exit edge; one owner per survivor; ordered causes; `isolate` (FAIL-16) | open: a single `Result[R, Causes]` form, which needs a per-boundary cause cell; a form that moves owned state into the body and back; `shield` |
| C, derivation and codecs | nominal identity, opacity, rigid bounds, declaration field order; the `codec` bound (OWN-4), decode-only field defaults, the `via` override, tombstones, appended variants and the `u32` tag, schema resolution and the decode contract (SEM-10) | open: how the checker hands a `codec` declaration's traversal to the library that generates its codec; order-preserving key codecs; the schema fingerprint that keys a registered entry (R13) and the content hash of a declaration for stored data |
| D, code descriptors | code identity; identity bytes of closed named functions (PROG-9), including typed `StaticFn` values passed into generic parameters; no address or order dependence; the revision, `code.revision(s)` (PROG-9), never part of equality | open: for registration (R13), identity bytes for instances, so that each instance of the generic adapter has them |
| L, append-only log | `Log[E]` (MEM-29) | open: a readable log or cell, which waits for R3 |
| I, providers | host primitives in core, `host.rename` and `host.sleep_until` among them; issue and delivery replay points; tapes and their substitutes (CAP-9); `WriteError`; the sync barriers `host.sync` and `host.sync_dir`, and `host.random` ([core](core.md#contracts-the-signatures-do-not-state)) | socket declarations and owned resources ([Net](net.md)); open: socket OS attempts/readiness; providers for `Stdin`, `Env`, `Process`; watching, renaming across directories |
| T, tasks | spawn, join, channels, cancellation points, `Arc`, the event order | settled for v1 with flows (CONC-14). Selection, shared limits, mutexes and atomics later. |

## Not in v1

User-made shared limits; mutexes and cells; atomics, which are not planned (R3); weak counted references; catching
failures other than by `isolate`; deadlines and timeouts other than `host.sleep_until` and the network operations' declared `until`; task sets and selection; `@noalloc`; compile-time
evaluation beyond `const`; unsafe code and foreign functions; layout declarations other
than `@compact`; methods, traits and implementation contracts; imports; list and map
literals (an `Inline` array literal is not one); format strings; `forget`; static brands;
conversion of errors by `?`; ranges over `char`, which would need to step over the surrogate
gap `D800`–`DFFF`; shareable owning callables (`own[shareable] fn`), which would
need every capture `shareable` and OWN-4 to admit the closure, its calls already only
viewing the environment (LANG-14), and would add a second calling mode, from several tasks; code generic over the numeric types: the bounds `integer` and
`float` are written only by core declarations ([OWN-4](ownership.md#own-4-bounds)), and a
library writes one function per width; generics over an `Inline` length; float lanes, 64-byte vectors, vectors in fields and collections, and checked lane arithmetic, whose `+` would fail with `Overflow` when any lane overflows, at the operator ([SEM-11](semantics.md#sem-11-vectors)); persistent mutable views; derivation other than a codec's (SEM-10); contracts and
facts; `@varint` (R25); documents in segments (R26); the store, runners and places; recording formats; a step grant for
finalizers, which would let cleanup under an exhausted step bound take steps ([MEM-30](budgets.md#mem-30-step-bounds)).
