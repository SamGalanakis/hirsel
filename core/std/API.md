# Hirsel standard library seed: API contracts

The seed contains the library contracts that the nine M1 workloads (`NINE.md`) need, plus the few
operations that complete their families. Each module is one signature-only source file in
this directory. A file holds the module's types, its function signatures and their contracts, and
no bodies. This document holds what the files share: how to read a contract, the design
of each family, what is provisional, and the spec gaps. Every signature lives in exactly one place, its
`.hirsel` file. The operation index at the end is generated from those files.

**Authority.** The language rules are the spec draft at aae09667 (`docs/spec/*.md`; rule
ids such as MEM-26 refer to it). That draft was attacked (spec-attack-887, findings F01 to F18)
and is being revised. Signatures that depend on a finding the revision will settle carry
a `PROVISIONAL (Fnn)` line. "What is provisional" below lists all of them. Where the draft's
rules block an operation this library needs, the operation is still written in its natural
shape and tagged with the gap: `Gn` for a rule that blocks it and `Pn` for a missing core primitive.
The full gap text, with the smallest spec change for each, is `SPEC-GAPS.md` in the brief folder;
"Spec gaps" below summarises it. No rule is bent to make a signature legal.

**Prior art.** Names and shapes come from Rust std (with indexmap, slotmap and bumpalo), Go,
Zig std and Goose's standard library, in that order of preference. A shape differs from all of them only
where a Hirsel rule forces the difference. `PRIOR-ART.md` in the brief folder gives, for each family,
where the shape was taken from and which rule forced each difference.

## Files

| Module | File | Types it declares | What it is |
|---|---|---|---|
| list | list.hirsel | (List is core) | growable own sequence |
| bytes | bytes.hirsel | (Bytes is core) | growable own bytes |
| span | span.hirsel | (spans, Builder are core) | stable sequences in regions, builders |
| string | string.hirsel | Utf8Error, Scalars, Positions | growable own UTF-8 |
| text | text.hirsel | TextError | stable UTF-8 in regions |
| char | char.hirsel | | Unicode scalars |
| int | int.hirsel | ParseError, Order | integer parsing and order |
| float | float.hirsel | | float parsing |
| map | map.hirsel | (Map is core) | insertion-ordered hash map |
| set | set.hirsel | Set | insertion-ordered hash set |
| pool | pool.hirsel | Pool, Id, Handle, Stale | generation-checked slot pool |
| io | io.hirsel | IoError, WriteError | the error types of the capability modules |
| path | path.hirsel | Path | a relative path below a Dir |
| dir | dir.hirsel | | directory operations on a Dir |
| file | file.hirsel | File, OpenOptions, Create, Lock, LockError | files and the OS lock |
| stdout | stdout.hirsel | | standard output |
| clock | clock.hirsel | | wall and monotonic time |
| args | args.hirsel | | the run's arguments |
| channel | channel.hirsel | | the bounded channel (core, restated) |
| task | task.hirsel | | spawn, join, cancel (core, restated) |

The extension is `.hirsel` so the old toolchain's `.hir` discovery never sees these
files. `core/` is outside its discovery roots anyway.

## Reading a contract

Each `pub fn` has `///` lines with these fields. A field that does not apply is left out.

- **Summary**: what the operation does, said once, with the rule ids it relies on.
- **CORE**: the operation is in core.md. The file restates it with its full contract,
  and the signature is copied from core.md, not changed. An implementation provides it
  as a core operation.
- **Fails:** how the operation can go wrong and what the caller gets back:
  - `Error(x)` is the operation's typed error.
  - "Bounds (failure)" is a failure in the FAIL sense, used only for caller bugs that a
    typed error could not usefully report. Examples: a base outside 2 to 36, or index
    arithmetic out of range in a `set`.
  - Each refused admission returns every value the operation was given to take, untouched,
    inside the error (`Error((c, x))`), so nothing the caller owned is lost.
  - "Fails: never" is usually left out.
- **Alloc:** the bytes the operation admits, which limit they are charged to, and when they
  are refunded. "Current limit" means the innermost `bound` or the task root in force at
  the call (MEM-21). "Nothing" means the operation admits no bytes; any host memory a provider
  uses is not charged to the run, and the contract says so explicitly.
- **View:** what a returned view is anchored to (OWN-20, `in p`) and what it freezes.
- **Prefix:** for capability operations, what has already happened on the host when the
  operation fails or is cancelled part way.
- **Cost:** time, in the length of the arguments. Amortised costs say so.
- **Ex:** one smallest call, written with explicit modes and `take` at every owning
  argument (F16).

The files use four defined terms:

- **GROWTH**: when the elements no longer fit, the operation admits a new block of twice
  the capacity (4 elements the first time), at the limit where the container was made
  (MEM-28), not the current limit. It copies the elements and refunds the old block.
  The new block is admitted before the old one is refunded (MEM-26), so a growing container briefly holds
  both blocks. If admission is refused, the container is unchanged. This is Rust's
  `Vec` amortised growth with a minimum of 4. Its 1.5x relative in Go (`append`) and the 2x in Zig
  (`ArrayList`) were not taken, because MEM-28 fixes doubling.
- **FREEZE x**: while the returned view lives, `x` is frozen (OWN-10). It cannot be
  written, moved, given out as `mut` or `take`, or cleaned. Viewing it again is allowed.
- **VIEWRANGE**: the result is a view of a sub-range or a retyped prefix of the argument's
  storage, such as a slice of a String or the bytes of a String. The draft has no such view
  (G1), so every operation tagged VIEWRANGE waits on G1.
- **Prefix**: as above.

## Conventions

**Module-qualified calls, no methods.** A call is `list.push(mut xs, take x)`. A file
declares `pub fn push`, and its name in other modules is `list.push`. The type
names a module declares are written unqualified everywhere (`Path`, `Pool`, `IoError`).
The draft only lets core types be written that way (G3).

**Errors carry what was not taken.** An operation that takes ownership and can fail
returns the taken values in its error, in parameter order: `Result[(), (Capacity, T)]`,
`Result[(), (Capacity, K, V)]`, `Result[Lock, (LockError, File)]`. The caller can retry,
store the value elsewhere, or clean it. This is the core rule for `list.push` (MEM-24),
applied to every taking operation in the library.

**Admission is the commit point.** Every operation that allocates admits all of its bytes
before it changes anything observable. A refused admission therefore leaves every
argument as it was. When an operation also has a host effect, admission happens before the
host is asked (for example `file.open`, `file.try_lock` and `dir.create_temp`).

**No hidden copies, allocation or retention.**
- A `View` result never copies.
- An operation that copies says so, and its result is a new owner (`string.copy`, `text.copy_in`).
- No operation keeps a reference to an argument after it returns. The one retention is a
  provider's designation for a narrowed Dir; `dir.narrow` states it, and it is host memory that
  lasts for the run.
- No operation grows a container the caller did not pass as `mut`. In particular
  `file.read_into` fills only spare capacity and never grows `buf`.

**Option for absence, Result for refusal.** `list.get` gives `Option`: absence is a normal
answer. `int.parse` gives `Result`: the error says why. The draft's own core operations
follow this split, and so does the library.

**No labels or or-patterns in calls.** Arguments are positional (F16). Record values use
constructor syntax, for example `OpenOptions { read: true, write: true, create: IfMissing }`. The doc comments write
alternative failures as `Error(NotFound | Denied)`; that is prose, not a pattern.

**Every capability operation is a provider operation.** It passes the replay point at
issue and at delivery (CAP-8). It is `waits` when the host may block, which also makes
it a cancellation point (CONC-7). It never charges the run for host memory. The draft has
no declaration form that lets a library module define a provider operation (P4), so
`dir`, `file`, `stdout` and `clock` are written as if that form existed: each body
would be one host call.

**Costs are stated for the worst case unless marked amortised or expected.** Hash
operations are "expected O(1)". The hash is fixed by the key type and unseeded (SEM-5),
so a caller who controls keys can make every operation O(len) (G14).

## Sequences: list, bytes, span

**list** follows Rust's `Vec` and Go's slices: `push`, `pop`, `insert`, `remove`,
`swap_remove`, `swap`, `truncate`, `reverse`, `fill`, `sort`.
- `get` returns `Option[View[T]] in xs`. Indexing `xs[i]` stays the language's checked form, which fails with Bounds.
- Removing never shrinks storage. `clear` and `truncate` clean the removed elements
  from the highest index down, the same order as cleaning a list (FAIL-11 order).
- `sort` is unstable pattern-defeating quicksort with a heapsort fallback (Rust's
  `sort_unstable`), so it allocates nothing. A stable sort would need a buffer of len/2
  elements, admitted visibly; it is left out of the seed.
- `list.capacity` and `list.reserve` expose storage. core.md has no such primitive (P1).
  A library body cannot read a core List's capacity without one.

**bytes** is `List[u8]` with the byte-specific operations: `extend` from another Bytes,
`sum`, and `slice` (VIEWRANGE). If G2 is accepted, `Bytes` becomes the type `List[u8]`
and this module shrinks to `extend`, `slice` and `sum`. Until then every operation except
`sum` needs a core primitive over the engine's byte block (P2). The representation is the same either way: a
24-byte header and a block of capacity x 1 byte, rounded up to 8.

**span** restates the core builders and adds two region helpers, `reserve` and `copy_in`.
They exist because a builder that outgrows its block abandons it inside the region
(MEM-3), and the abandoned block stays charged until the region closes. Admitting the final
size once avoids that waste. The design is in "Regions and builders" below.

**No list views of sub-ranges.** `list.slice`, Rust's `&xs[a..b]`, is the most-used sequence
operation in every reference library. Here it is VIEWRANGE (G1), and it is left out of the
seed rather than specified twice. Callers pass `(xs, start, end)`.

## Text: string, text, char, int, float

**string** is Rust's `String` vocabulary under module names:
- `push` appends text; there are also `push_char`, `push_u64`, `push_i64`, and `push_uint` with a base, width and pad, for `\u00XX` and hex.
- `clear`.
- `from_utf8` and `into_bytes`, which move the block without copying.
- `find`, `starts_with`, `ends_with`, `strip_prefix`, `split_once`, `compare`.
- `make_ascii_upper`.

Positions are byte offsets, as in Rust and Go. An operation that takes a position requires a
scalar boundary and checks it in O(1) with one byte test, as `is_boundary` does. Out-of-range or
non-boundary positions give `None`, never a failure, because a caller computing positions
from input needs to branch on them.

**Iterating scalars.**
- `string.next_scalar(s, i) -> Option[(char, u64)]` decodes the scalar at `i` and
  returns it with the next position. This is Go's `utf8.DecodeRuneInString` loop. It works in
  the draft today, with no new rule, so the nine use it.
- `scalars` and `positions` are the `for` forms, Rust's `chars` and `char_indices`. They are
  scoped cursor types, and `for` over a library type needs G12.

**Views into strings.** `as_bytes`, `slice`, `strip_prefix` and `split_once` return views of
part of a String, or of its bytes retyped. Every one of the nine cuts lines and fields
this way. The draft's views denote whole places only (OWN-8), so all four are VIEWRANGE (G1).
They are specified now because the alternative, copying every field, contradicts the
no-hidden-copy rule and the request-scratch workload, whose point is that nothing is copied.

**text** is the stable twin of String: a `SeqSpan[u8]` holding valid UTF-8 in a region.
- `copy_in` (CORE) publishes a copy of a String.
- `from_utf8_in` validates bytes and publishes them, so text read from a file reaches a region with one
  copy rather than two.
- `as_string` views a Text as a String (G1), so the string operations apply to it.

Text has no slicing in the seed. A sub-range of a SeqSpan has no header of its own, so it
needs G1, or a representation change, or a copy.

**char**: `from_u32` and `to_u32` (P3, missing from core), `utf8_len`, `to_ascii_upper`,
and `digit(c, base)`. These are the Rust names.

**int**: `parse` (CORE), `parse_radix` and `compare`. `ParseError = Empty | Invalid(u64) |
OutOfRange`, where `Invalid` carries the byte position of the first bad byte. `Order = Less | Equal
| Greater` is declared here because core.md names it without defining it (G4). The library
declares `IoError`, `ParseError` and `Order` and asks core.md to adopt them.

**float**: `parse` (JSON number syntax, correctly rounded, overflow gives infinity) and
`is_finite`. The algorithm is Rust's dec2flt: an Eisel-Lemire fast path, with a
big-decimal slow path that keeps at most 768 significant digits in a fixed-size local record. So
it never allocates. Float formatting is left out of the seed: shortest round-trip
printing (Ryu or Grisu with a fallback) is a large contract that none of the nine needs,
because json keeps its number lexemes as text.

## Map and Set

**Shape.** `Map[K: key, V]` is insertion-ordered (SEM-5) with the indexmap / CPython 3.7+ layout:
- an entry array in insertion order, where each entry is `(hash, key, value)` or a tombstone;
- an open-addressed index of entry positions, at least twice the entry capacity so the load factor
  stays at or below 1/2.

`map.insert` on a present key replaces the value in place and keeps the stored key and its
position. This is indexmap's `insert`, Python's dict, and the json workload's duplicate-key rule.

**Charges.** One block of `capacity x (size(K) + size(V) + 16)`, rounded up to 8. The 16 bytes
per entry pay for the 8-byte hash, and for two index slots of 4 bytes each. 4-byte index slots limit a map to 2^32 - 1
entries, and growing past that fails Capacity. The block is charged to the limit where the map was made (MEM-28).
GROWTH applies to capacity counted in entries.

**Removal and compaction.**
1. `map.remove` turns the entry into a tombstone, marks its index slot deleted, and returns the
   value. It is O(1) expected. This is indexmap's `shift_remove` order semantics without its O(n) shift.
2. When `insert` finds the entry array full, there are two cases. If at least half the slots are tombstones, it
   compacts in place and rebuilds the index. That needs no admission, and its cost is O(capacity), amortised
   against the removals that made the tombstones. Otherwise it grows, and the copy into the new block also
   drops the tombstones.

So a map whose live size stays bounded never grows without limit under churn. The lru workload depends on
this (64 live entries, 10,000 puts).

**Updates without mutable views.** The draft has no mutable view results: a returned
view is read-only. So the `get_mut` and `entry` shapes cannot be expressed.
`map.update(mut m, k, f) -> Option[R]` runs `f` on the value in place and returns its
result, or `None` when `k` is absent, in which case `f` is not called. The counting idiom is:
```
if map.update(mut counts, w, fn(mut n: u64) { n += 1 }) == None {
  map.insert(mut counts, take string.copy(w)?, take 1)?
}
```
This hashes the key twice on a first sight. Rust's `entry` avoids that by holding a mutable
borrow, which Hirsel has no way to return. `f` is a scoped callable (OWN-21). Its declared
summaries are PROVISIONAL (F15).

**Iteration.** `for (k, v) in m` binds a view of each live key and value in insertion order and
freezes `m` for the loop. The draft does not say what `for` over a map binds (G13).

**Dictionaries.** The draft fixes a key type's equality and hash by the type (SEM-5). So two maps
compare and hash keys alike exactly when their key types are the same type. There is no
per-map hasher parameter and no seed. The cost is HashDoS: input that collides
degrades every operation to O(len) (G14). The workloads' inputs are fixed, so it does not
affect them; it matters for services.

**Set** is `opaque type Set[K: key] { m: Map[K, ()] }`, which is Rust's `HashSet` over `HashMap`. Its
storage is `capacity x (size(K) + 16)`. `set.insert` returns whether the key was new, and on
refusal returns `(Capacity, K)`.

## Pool

**Shape.** This is a slotmap: `Pool[T] { slots: List[Slot[T]], free: u64, live: u64 }`, where
`Slot[T] = Occupied(generation, T) | Vacant(generation, next_free)`.
- Vacant slots form an intrusive free list, so `remove` never allocates and `insert` reuses
  the most recently vacated slot first.
- A slot's generation rises by one on each removal. A slot whose generation reaches 2^64 - 1 is retired and
  never reused, so an old handle can never validate against a later occupant.
- The pool is built only from what MEM-19 allows: opaque fields, checked arithmetic and
  indexing of its own list.

**Two handle kinds.**
- `Id[T] of p` is branded (MEM-18). The checker ties it to the pool value `p`, so an id
  from one pool is refused at compile time on another pool. `pool.get(p, id)` still checks the
  generation at run time. An Id lives beside its pool, in a local or in a sibling field written
  `Id[T] of .pool`.
- `Handle[T]` is the same 16 bytes without a brand. It is what a pool entry stores to link to
  another entry (lru's prev and next) and what a `Map` stores as a value. The brand cannot be written
  in either place (G6, G7). `pool.handle(p, id)` drops the brand, and `pool.resolve(p, h)` checks the
  generation and gives back an `Id[T] of p` or `Stale`.

The lru workload's stale probe is `pool.resolve(cache.pool, first)`, which returns `Error(Stale)` after reuse.

**Brand soundness gap (G5).** While a callee holds a `mut` loan of a Pool, it can assign a
whole new pool to the parameter. An old Id would then validate against the new pool, because the
indices and generations are just numbers. The draft refuses replacing a branded owner's place in the
owner's scope, but not through a `mut` parameter. The fix asked for: a `mut` parameter of a type that is
opaque in the callee's module cannot be assigned or replaced as a whole. This is also F14 (brand epochs).

**Not in the seed:**
- `pool.insert_owned`, which returns a `SlotOwner[T] of p` resource (MEM-20). The draft's example names it
  `pool.insert`; renaming it is asked for in G8.
- `pool.clear` and pool iteration.

## Capabilities: args, path, dir, file, stdout, clock

**Model.** A capability value designates host authority (CAP-1). `main`'s parameters are
granted by the run configuration (CAP-4). Every operation is a provider operation; see Conventions.
- `Dir` is plain and `send`. It is an 8-byte designation the provider resolves.
- `dir.narrow` gives a Dir for a subdirectory, never with more authority (CAP-7).

**Path.** `opaque type Path { s: String }`, always relative, made by `path.of(take s)` without a check. The
provider checks at each use and refuses `..`, absolute paths and empty components with
`InvalidPath` before any host effect. A Path therefore never names anything outside its Dir.
Checking at construction would mean a fallible `path.of` for every literal.

**IoError** is one closed enum for all capability operations: `NotFound | Exists | Denied | InvalidPath |
InvalidData | NotEmpty | BrokenPipe | Os(i32) | Capacity(Capacity)`. This is Go's flat error set and Rust's `ErrorKind`, without the
open-ended parts. An operation's `Fails:` line names the arms it can return. `Capacity` is an
arm because an operation that allocates a result and talks to the host should not need two
error layers. `WriteError { written, error }` carries the committed prefix of a write. That is Go's
`n, err` from `Write`, made a value.

**File** is a `send` resource holding an open host handle.
- Making a File admits 32 bytes for its cause cell (MEM-26, FAIL-10) at the current limit.
- `file.close(take f)` reports the host's close error.
- The finalizer closes and ignores the error, as Rust's `Drop for File` does.
- The read and write operations are Rust's: `open`, `create`, `create_new`, `open_with(OpenOptions)`,
  `read_into` (Rust's `read` into spare capacity), `write_all`, `close`, and the whole-file `read`,
  `read_string`, `write` and `remove`.

**Lock** is the OS lock: advisory, exclusive, whole-file, with flock semantics (Rust 1.89's
`File::try_lock`).
- It is a resource that owns its File: `file.try_lock(take f) -> Result[Lock, (LockError, File)]`.
- `file.unlock(take l) -> Result[File, (IoError, File)]`. The lock is released exactly once, by
  `unlock` or by the finalizer.
- It lives in `file` rather than in its own module, because only `file` can read File's opaque
  handle. Shared and blocking locks are left out of the seed.

**stdout.write** is CORE. On failure, a prefix of the text of unknown length may have been written,
and core.md's error does not say how much (G15). **clock** has `now_ms` and `monotonic_ns`. Each reading
passes the replay point, so a replay returns the recorded value. **args** has `len`, and `get` returns a view
anchored to the Args.

**dir.create_temp** is Go's `os.MkdirTemp`: a private directory named by a prefix and a random
suffix, which the files workload needs. Removing it is the caller's job (`dir.remove_all`), as in Go.

## Tasks and channels

`task` and `channel` restate the core operations with their full contracts, and add nothing.
- There is no wrapper type. The core channel is already bounded, admits all of its queue storage at
  creation (`cap x size(T) + 64`), and never allocates afterwards, which is exactly what a library
  wrapper would add.
- Backpressure: `send` waits while the queue is full; `try_send` returns `Full(x)` with the item.
- Dropped endpoints: CONC-9. When the last Sender is gone, the channel closes for receiving
  after its queued items. When the Receiver is gone, the channel closes for sending and the queued items are
  cleaned from the highest index down.

Provisional points, all settled by the revision:
- `channel.sender` becomes fallible, admitting `2 x 32` bytes, or is explicitly exempt (F13).
- `task.spawn` takes a non-capturing function plus a `take` payload (F04). The current signature
  already has that shape; the callable's declared summaries are added by F15.
- `JoinError` becomes `Failed(Causes) | Cancelled(Causes)` (F11, G16).
- Channel capacity 0 is unspecified (G17). The library documents `cap >= 1`.

## Regions and builders

A region (MEM-1..5) holds stable values: spans and Text. Its storage is freed in one step
when the region closes. A builder (MEM-3) appends to a growing block in the region and
publishes a span when it finishes. The library adds two helpers to the core builders:
- `span.reserve(mut b, n)` admits one block big enough for `n` more elements, so the next `n` pushes
  neither grow the block nor abandon one.
- `span.copy_in(r, xs)` publishes a FixedSpan copy of a plain list in one admission. This is bumpalo's
  `alloc_slice_copy`.

Both exist because an outgrown block stays charged to the region until it closes. These are
the bumpalo and Zig `ArenaAllocator` shapes, reduced to what the region rules allow.

The request-scratch pattern is: a region per request; a span of plain position records
(`Header { name: (u64, u64), value: (u64, u64) }`) built over the request's text; views
taken back through `string.slice`. Nothing is copied out of the request.

Every builder signature is PROVISIONAL (F02, F07). The revision makes the destination writer
and its Child token a built-in with a fresh destination brand, and the builders may sit on
it. The operation names, failures and charges stay; the brand in the result type may
change. `build_in` helpers are left out of the seed until that design exists (G9).

## What is provisional

| Finding (spec-attack-887) | Change coming | Signatures affected |
|---|---|---|
| F01 scoped values with a non-copyable component move | cursors and entry views | `string.scalars`, `string.positions`. Both hold only a view and a position, so they stay copyable; still marked |
| F02, F07 destination writer and Child become built-in, with a fresh brand | the builders sit on it | every `span` builder op, `span.copy_in`, `text.copy_in`, `text.from_utf8_in` |
| F04 spawn takes a non-capturing fn plus a take payload | callable type | `task.spawn` |
| F11 `JoinError = Failed(Causes) \| Cancelled(Causes)` | error shape | `task.join` |
| F13 sender duplication is fallible or exempt | result type | `channel.sender` |
| F15 callable types declare environment, send, authority and `waits` | callback types | `list.sort`, `map.update`, `pool.update`, `task.spawn` |
| F16 `take` at every owning call site; no ordinary labels or or-patterns | examples | all examples are already written that way |
| Capacity.available names the limit that was tried | field meaning | every `Error(c)`. The shape is unchanged; the meaning is sharpened |
| stable types hold only plain or stable fields | element bounds | `span.copy_in[T: plain]` already holds; builders' `T` will gain the bound |

A signature without a PROVISIONAL line does not depend on these points.

## Spec gaps

Each line gives the gap and the smallest change asked for. Full text: `SPEC-GAPS.md`.

- **G1** Views of sub-ranges and retyped prefixes. Change: core operations may return a view
  denoting a sub-range or a retyped prefix of an argument's storage, anchored and freezing
  as a whole view does. Blocks `string.as_bytes`, `slice`, `strip_prefix`, `split_once`,
  `bytes.slice` and `text.as_string`.
- **G2** `Bytes` is not `List[u8]`, and String is not an opaque record over it. Change: define them so. This removes P2.
- **G3** Library type names cannot be written unqualified. Change: a module-declared `pub type` name
  resolves unqualified in every module of the package unless shadowed.
- **G4** core.md names `IoError`, `ParseError`, `FloatError`, `Order` and `Args` without defining them.
  Change: adopt this library's declarations, or define them.
- **G5** A `mut` loan lets a callee replace a branded owner (F14). Change: a `mut` parameter of a type that is
  opaque in the callee's module cannot be replaced as a whole.
- **G6** A brand cannot be written inside a field's type arguments (`Map[String, Id[E] of .pool]`).
  Change: allow `of .field` in type arguments of a sibling field.
- **G7** Links inside one pool's entries have no brand form. Change: none asked; `Handle` and
  `resolve` cover it with a run-time check.
- **G8** MEM-20's example calls `pool.insert` for the SlotOwner form. Change: rename it `pool.insert_owned`.
- **G9** `build_in` and Child (F02, F07): waits on the revision.
- **G10** There is no declaration form for provider operations. Change: a `provider fn` form in core, with
  its replay point and `waits` rules.
- **G11** Scoped affinity (F01): waits on the revision.
- **G12** `for` over a library cursor type. Change: `for` accepts any type with a `next`
  operation of the documented shape, resolved by module.
- **G13** What `for` over a Map binds. Change: `(View[K], View[V])` in insertion order, freezing the map.
- **G14** The unseeded fixed hash allows HashDoS. Change: state it, or make the hash per-run seeded under replay.
- **G15** `stdout.write`'s error has no committed prefix. Change: return `WriteError`.
- **G16** `JoinError.Cancelled` has no causes (F11).
- **G17** Channel capacity 0 is unspecified. Change: refuse it with Bounds, or define it as a rendezvous.
- **G18** Release traces need a cell-free log. Change: none to the spec; the corpus's
  release-order witness costs a channel in single-task programs (NINE.md).

Missing core primitives:
- **P1** `list.capacity` and `list.reserve`.
- **P2** The byte-block primitives under Bytes and String, unless G2 is accepted.
- **P3** `char.from_u32` and `char.to_u32`.
- **P4** The provider host operations: open, read, write, close, lock, unlock, dirs, clock.

## Operation index

Generated from the `.hirsel` files: module, operation count, how many are CORE, and the names.
For signatures, read the file.

| Module | Ops | CORE | Operations (* = CORE) |
|---|---|---|---|
| list | 19 | 10 | new*, with_capacity*, len*, capacity, get*, push*, pop*, set*, clear*, fill*, sum*, reserve, insert, remove, swap_remove, swap, truncate, reverse, sort |
| bytes | 11 | 0 | new, with_capacity, len, capacity, get, push, extend, clear, truncate, slice, sum |
| span | 9 | 7 | builder*, fixed_builder*, indexed_builder*, push*, reserve, finish*, copy_in, len*, get* |
| string | 26 | 6 | new*, with_capacity, from*, copy*, byte_len*, push*, push_char, push_u64*, push_i64, push_uint, make_ascii_upper, clear, from_utf8, into_bytes, as_bytes, slice, is_boundary, next_scalar, scalars, positions, find, starts_with, ends_with, strip_prefix, split_once, compare |
| text | 4 | 2 | len*, copy_in*, from_utf8_in, as_string |
| char | 5 | 0 | from_u32, to_u32, utf8_len, to_ascii_upper, digit |
| int | 3 | 1 | parse*, parse_radix, compare |
| float | 2 | 0 | parse, is_finite |
| map | 9 | 5 | new*, with_capacity, len*, get*, contains, insert*, update, remove*, clear |
| set | 5 | 0 | new, len, contains, insert, remove |
| pool | 11 | 0 | new, with_capacity, len, insert, get, get_or, contains, update, remove, handle, resolve |
| io | 0 | 0 | (types only) |
| path | 2 | 0 | of, as_string |
| dir | 4 | 0 | narrow, create_dir, remove_all, create_temp |
| file | 13 | 0 | open, create, create_new, open_with, read_into, write_all, close, read, read_string, write, remove, try_lock, unlock |
| stdout | 1 | 1 | write* |
| clock | 2 | 0 | now_ms, monotonic_ns |
| args | 2 | 0 | len, get |
| channel | 5 | 5 | new*, sender*, send*, try_send*, receive* |
| task | 4 | 4 | spawn*, join*, cancel*, check_cancel* |
| **total** | **137** | **41** | |
