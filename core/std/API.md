# Hirsel standard library seed: API contracts

The seed contains the library contracts that the nine M1 workloads (`NINE.md`) need, plus the few
operations that complete their families. Each module is one signature-only source file in
this directory. A file holds the module's types, its function signatures and their contracts, and
no bodies. This document holds what the files share: how to read a contract, the design
of each family, and the spec gaps that still shape a signature. Every signature lives in exactly
one place, its `.hirsel` file. The operation index at the end is generated from those files.

**Authority.** The language rules are spec revision 2 (`docs/spec/*.md` at a46e3f51; rule ids
such as MEM-26 refer to it). core.md's operations are restated here with their full contracts
and the signature copied unchanged. Everything else is library code over them. Where revision 2
gives the library no way to write an operation, the operation is still written in its natural
shape and its contract starts `NEEDS CORE (Pn | Nn)`. The full gap text, with the smallest spec
change for each, is `SPEC-GAPS.md` in the brief folder; "Spec gaps" below summarises it. No rule
is bent to make a signature legal.

**Prior art.** Names and shapes come from Rust std (with indexmap, slotmap and bumpalo), Go,
Zig std and Goose's standard library, in that order of preference. A shape differs from all of them only
where a Hirsel rule forces the difference. `PRIOR-ART.md` in the brief folder gives, for each family,
where the shape was taken from and which rule forced each difference.

## Files

| Module | File | Types it declares | What it is |
|---|---|---|---|
| list | list.hirsel | (List is core) | growable own sequence; also Bytes, which is `List[u8]` |
| bytes | bytes.hirsel | | the one byte-only helper, `sum` |
| span | span.hirsel | (spans, Builder are core) | stable sequences in regions, builders (core, restated) |
| string | string.hirsel | | growable own UTF-8 |
| text | text.hirsel | | stable UTF-8 in regions (core, restated) |
| char | char.hirsel | | Unicode scalars |
| int | int.hirsel | | integer parsing and order |
| float | float.hirsel | | float parsing |
| map | map.hirsel | (Map is core) | insertion-ordered hash map |
| set | set.hirsel | Set | insertion-ordered hash set |
| pool | pool.hirsel | Pool, Id, Stale | generation-checked slot pool |
| dir | dir.hirsel | | directory operations on a Dir |
| file | file.hirsel | Error, Lock, LockError | files and the OS lock |
| stdout | stdout.hirsel | | standard output (core, restated) |
| args | args.hirsel | | the run's arguments (core, restated) |
| channel | channel.hirsel | | the bounded channel (core, restated) |
| task | task.hirsel | | spawn, join, cancel (core, restated) |

Revision 2 made `IoError`, `WriteError`, `ParseError`, `FloatError`, `Order`, `OpenMode` and
`HostFile` core types, so the seed's `io` module is gone. `path` is gone because every host
primitive takes a `String` path that its Dir resolves and confines; a wrapper that checks nothing
adds nothing. `clock` is gone because `host.wall_ms` and `host.mono_ns` are already the whole
operation.

The extension is `.hirsel` so the old toolchain's `.hir` discovery never sees these
files. `core/` is outside its discovery roots anyway.

## Reading a contract

Each `pub fn` has `///` lines with these fields. A field that does not apply is left out.

- **Summary**: what the operation does, said once, with the rule ids it relies on. Library code
  names the core operations it is built from.
- **CORE**: the operation is in core.md. The file restates it with its full contract,
  and the signature is copied from core.md, not changed.
- **NEEDS CORE (Pn | Nn)**: no library body can be written over revision 2's core; the gap
  names the smallest core addition.
- **Fails:** how the operation can go wrong and what the caller gets back:
  - `Error(x)` is the operation's typed error.
  - "Bounds (failure)" is a failure in the FAIL sense, used only for caller bugs that a
    typed error could not usefully report: a base outside 2 to 36, an index out of range.
  - Each refused admission returns every value the operation was given to take, untouched,
    inside the error (`Error((c, x))`), as core.md requires of its own operations.
- **Alloc:** the bytes the operation admits, which limit they are charged to, and when they
  are refunded. "Current limit" means the innermost `bound` or the task root in force at
  the call (MEM-21). "Nothing" means the operation admits no bytes.
- **View:** what a returned view is anchored to (OWN-20, `in p`) and what it freezes.
- **Prefix:** for capability operations, what has already happened on the host when the
  operation fails or is cancelled part way.
- **Cost:** time, in the length of the arguments. Amortised costs say so.
- **Ex:** one smallest call. An argument for a `take` parameter is written `take p` when it is a
  place and bare when it is a fresh value (OWN-7); patterns bind without `take`.

The files use two defined terms:

- **GROWTH**: when the elements no longer fit, the operation admits a new block of
  `max(needed, 4, 2 x capacity)` elements at the limit where the container was made (MEM-28),
  not the current limit. It copies the elements and refunds the old block. The new block is
  admitted before the old one is refunded, so a growing container briefly holds both. If
  admission is refused, the container is unchanged. This is Rust's `Vec` growth with a minimum
  of 4.
- **FREEZE x**: while the returned view lives, `x` is frozen (OWN-10). It cannot be
  written, moved, given out as `mut` or `take`, or cleaned. Viewing it again is allowed.

## Conventions

**Module-qualified calls, no methods.** A call is `list.push(mut xs, take x)`. A file
declares `pub fn push`, and its name in other modules is `list.push`. A type a library module
declares is written with its module path outside it (`set.Set`, `pool.Pool`, `file.Lock`,
LANG-7); core types are written bare. `list`, `string`, `map`, `text`, `span`, `char`, `int`,
`float`, `args`, `stdout`, `channel` and `task` are core modules to which the seed adds members.
Where those members live is N1.

**Errors carry what was not taken.** An operation that takes ownership and can fail
returns the taken values in its error, in parameter order: `Result[(), (Capacity, T)]`,
`Result[(), (Capacity, K, V)]`, `Result[file.Lock, (file.LockError, HostFile)]`. This is core.md's
rule for its own operations, applied to every taking operation in the library.

**Admission is the commit point.** Every operation that allocates admits all of its bytes
before it changes anything observable. A refused admission therefore leaves every
argument as it was. When an operation also has a host effect, admission happens before the
host is asked (`file.try_lock` makes its Lock before calling `host.try_lock`).

**No hidden copies, allocation or retention.**
- A `View` result never copies.
- An operation that copies says so, and its result is a new owner (`string.copy`, `text.copy_in`).
- No operation keeps a reference to an argument after it returns.
- No operation grows a container the caller did not pass as `mut`. `file.read_into` fills only
  spare capacity and never grows `buf`, as `host.read` does.

**Option for absence, Result for refusal.** `list.get` and `map.get` give `Option`: absence is a
normal answer. `int.parse` and `pool.get` give `Result`: the error says why.

**Capability operations are library code over host primitives.** core.md lists the provider
operations (CAP-5) as `host.*` and states that the standard library builds its capability modules
from them. Each `dir` and `file` operation names the primitive or primitives it calls. They pass
the replay point at issue and at delivery (CAP-8), and are `waits`, so each is a cancellation
point (CONC-7). Paths are `String`s resolved relative to their Dir, which they may not leave.

**Costs are stated for the worst case unless marked amortised or expected.** Hash
operations are "expected O(1)". The hash is private and fixed per key type (SEM-5); CAP-6 lets
the engine key it with a per-run seed, and G14 asks that it must.

## Sequences: list, bytes, span

**list** follows Rust's `Vec` and Go's slices. Core gives `new`, `with_capacity` (exact),
`len`, `capacity`, `reserve`, `get`, `slice`, `push`, `pop`, `set`, `clear`, `fill`, `append`
and `sum`. The library adds `remove`, `insert`, `swap_remove`, `swap`, `truncate`, `reverse` and
`sort`, each built from `pop`, `push` and `set` chains: `set` returns the old element, so moving
elements needs no view and no placeholder.
- `get` returns `Option[View[T]] in xs`. Indexing `xs[i]` is the same read failing with Bounds.
- `slice` is a sub-view (OWN-8): a view of `from..to`, anchored to and freezing the whole list.
- Removing never shrinks storage. `clear` and `truncate` clean the removed elements
  from the highest index down (FAIL-11 order).
- `sort` is unstable pattern-defeating quicksort with a heapsort fallback (Rust's
  `sort_unstable`), so it allocates nothing. Its `before` is a `scoped fn` (LANG-13). A stable
  sort would need a buffer of len/2 elements, admitted visibly; it is left out of the seed.

**Bytes is `List[u8]`** (core.md). Every list operation applies, so the byte-specific module
shrinks to `bytes.sum`, which sums in u64 where `list.sum` would overflow u8.

**span** restates the core builders. The seed's `span.reserve` and `span.copy_in` are gone:
revision 2's `build_in` (a sized destination, MEM-4) and `prepare_in` (a staged own value copied
in one admission) are the core way to admit a final size once, and a library helper over them
adds nothing.

## Text: string, text, char, int, float

**string** is Rust's `String` vocabulary under module names.
- Core: `new`, `from`, `copy`, `from_utf8` (returns the Bytes on failure), `into_bytes`,
  `as_bytes` and `slice` (sub-views), `next_scalar`, `byte_len`, `push`, `push_char`, `push_u64`.
- Library: `with_capacity` (`from_utf8` of `list.with_capacity`), `push_i64`, `push_uint`
  (base, width, pad), `make_ascii_upper` (through `into_bytes` and `from_utf8`, with an O(n)
  revalidation), `clear`, `is_boundary`, `find`, `starts_with`, `ends_with`, `compare`.

Positions are byte offsets, as in Rust and Go. `slice` fails with Bounds at a non-boundary,
so code cutting at positions from input checks `is_boundary` first.

**Cutting lines and fields is `find` plus `slice`.** OWN-20 lets a function's result denote
"the anchored argument, a part of it, or (for a core operation) a sub-view". A library
`split_once` or `strip_prefix` would return a sub-view it got from `string.slice`, which reads as
a core-only permission (N2). So both are left out and callers write:
```
let Some(colon) = string.find(line, 0, ":") else { return Error(Malformed) }
let name = string.slice(line, 0, colon)
let value = string.slice(line, colon + 1, string.byte_len(line))
```
If N2 is settled the other way, both come back with their Rust and Go shapes.

**Iterating scalars** is `while let Some((c, next)) = string.next_scalar(s, at)`, Go's
`utf8.DecodeRuneInString` loop and the form language.md uses. `for` iterates only built-in
sources (SEM-6), so the seed's `scalars` and `positions` cursors are gone.

**text** is the stable twin of String: a `SeqSpan[u8]` holding valid UTF-8 in a region. Both
operations are core: `len` and `copy_in`. `as_string` and `from_utf8_in` are gone: no core
operation gives a String view of a Text or a Bytes, so both would copy through a temporary (N2).

**char**: `from_u32` and `to_u32` are core; `utf8_len`, `to_ascii_upper` and `digit(c, base)`
are library code. These are the Rust names.

**int**: `parse` (core), `parse_radix` and `compare`. `ParseError = Empty | Invalid(u64) |
OutOfRange` and `Order` are core types.

**float**: `parse` (JSON number syntax, correctly rounded, overflow gives infinity) and
`is_finite`. The algorithm is Rust's dec2flt, which never allocates. It cannot be written over
revision 2's core: there is no named integer-to-float conversion and no `from_bits` (N4). Float
formatting is left out of the seed: none of the nine needs it, because json keeps its number
lexemes as text.

## Map and Set

**Shape.** `Map[K: key, V]` is insertion-ordered (SEM-5) with the indexmap / CPython 3.7+ layout:
- an entry array in insertion order, where each entry is `(hash, key, value)` or a tombstone;
- an open-addressed index of entry positions, at least twice the entry capacity so the load factor
  stays at or below 1/2.

`map.insert` on a present key replaces the value in place and keeps the stored key and its
position. This is indexmap's `insert`, Python's dict, and the json workload's duplicate-key rule.

**Charges.** One block of `capacity x (size(K) + size(V) + 16)`, rounded up to 8. The 16 bytes
per entry pay for the 8-byte hash and two index slots of 4 bytes each. 4-byte index slots limit a
map to 2^32 - 1 entries, and growing past that fails Capacity. The block is charged to the limit
where the map was made (MEM-28).

**Removal and compaction.**
1. `map.remove` turns the entry into a tombstone, marks its index slot deleted, and returns the
   value. It is O(1) expected: indexmap's `shift_remove` order semantics without its O(n) shift.
2. When `insert` finds the entry array full, there are two cases. If at least half the slots are
   tombstones, it compacts in place and rebuilds the index, with no admission, at O(capacity)
   amortised against the removals. Otherwise it grows, and the copy also drops the tombstones.

So a map whose live size stays bounded never grows without limit under churn. The lru workload
depends on this (64 live entries, 10,000 puts).

**No in-place update (P5).** Revision 2 has no mutable view results and no core operation that
writes a present key's value without a key, and Map's representation is core's, so a library
`map.update` cannot reach the value. The generics tally therefore pays a key copy, an
admission and a hash per repeat:
```
let n = match map.get(counts, w) { Some(n) => n, None => 0 }
map.insert(mut counts, string.copy(w)?, n + 1)?     // drops the copied key on a hit
```
The seed specifies `map.set(mut m, k, take v) -> Result[V, V]`, the map twin of `list.set`, and
marks it NEEDS CORE (P5). With it a repeat costs no allocation. A general `update(f)` for an
own `V` would still need a placeholder value to swap in, so it is left out of the seed.

**Iteration.** `for (k, v) in m` binds `(View[K], View[V])` for each live entry in insertion
order and freezes `m` for the loop (SEM-6).

**Set** is `opaque type Set[K: key] { m: Map[K, ()] }`, which is Rust's `HashSet` over `HashMap`,
written `set.Set` outside its module. Its storage is `capacity x (size(K) + 16)`. `set.insert`
returns whether the key was new, and on refusal returns `(Capacity, K)`. A set is not iterable
(SEM-6 allows only built-in sources); none of the nine iterates one.

## Pool

**Shape.** This is a slotmap: `Pool[T] { me: Unique, slots: List[Slot[T]], free: u64, live: u64 }`,
where `Slot[T] = Occupied(generation, T) | Vacant(generation, next_free)`.
- Vacant slots form an intrusive free list, so `remove` never allocates and `insert` reuses
  the most recently vacated slot first.
- A slot's generation rises by one on each removal. A slot whose generation reaches 2^64 - 1 is
  retired and never reused, so an old Id can never validate against a later occupant.
  slotmap wraps 32-bit versions instead, which can alias.

**One handle, checked at run time** (round-3 ruling: no static brands in the first version).
`Id[T]` is plain data: the pool's identity, a slot index and a generation, 24 bytes, `plain + key`.
Every access (`get`, `get_or`, `contains`, `update`, `remove`) compares all three and returns
`Error(Stale)` on a mismatch, with `Stale = WrongPool | Removed`. Because an Id is plain, it is
stored anywhere: lru's index is a `Map[String, pool.Id[Entry]]`, its entries link with
`Option[pool.Id[Entry]]`, and its saved first handle is an Id. Dropping the pool cleans what is
left; removal is explicit. There is no `of p`, no SlotOwner and no second handle type.

**Pool identity needs one core primitive (P6).** A library cannot make a value unique within
a run: there are no cells, no global state and no address facts. The pool asks core for
`type Unique` (plain, `key`, 8 bytes, equality only, no order and no conversion to an integer)
and `unique.new() -> Unique`, which never repeats within a run and allocates nothing. Equality
only keeps the value unobservable apart from "same or not", so a task-schedule-dependent counter
does not leak into output or replay. The pool also needs `list.with_capacity`, `len`, `get`,
`push` and `set` (core) and checked 64-bit arithmetic; nothing else. Assigning a new pool to a
place gives the place a new identity, so old Ids answer `WrongPool`. A `clone` of a pool
(MEM-17) would copy the identity, so the original's Ids resolve in the copy to equal values, as
in slotmap's `Clone`.

**Update without a mutable view.** `pool.update(mut p, h, f)` takes the value out of its slot with
`list.set` and a `Vacant` placeholder, calls `f` on the owned local, and installs it again. This is
legal library code because the pool owns its slot list. If `f` fails, the value is cleaned with
the local and the slot is left vacant at a new generation.

**Not in the seed:** `pool.clear` and pool iteration.

## Capabilities: args, dir, file, stdout

**Model.** A capability value designates host authority (CAP-1). `main`'s parameters are
granted by the run configuration (CAP-4). `Dir`, `Clock`, `Stdout` and `Stderr` are plain and
`send`. `host.wall_ms(c)` and `host.mono_ns(c)` are used directly; `stderr.write` is the twin of
`stdout.write`.

**dir**: `create_dir` (`host.mkdir`), `remove_all` (`host.remove_tree`) and `create_temp`
(`host.mkdtemp`, which returns a Dir for the new directory and not its name). The seed's `narrow`
is gone: no primitive returns a Dir for an existing directory. A temporary directory cannot be
removed by the seed, because `host.remove_tree` needs its name in the parent (N3).

**file** works on core's `HostFile`, a `send` resource whose finalizer closes it and ignores the
outcome, as Rust's `Drop for File` does.
- `open(d, p, mode)` with core's `OpenMode = Read | Write | Append | CreateNew`. The arms are not
  defined (N5); the seed assumes Rust's `File::open`, `File::create` (create or truncate),
  append-create and `File::create_new`. There is no read-write mode.
- `read_into` fills spare capacity (`host.read`), `write_all` returns core's
  `WriteError { written, error }`, and `close` reports the close error.
- `read`, `read_string`, `write` and `remove` are the whole-file forms. `read` and `read_string`
  allocate, and `IoError` has no Capacity or invalid-data arm, so they return
  `file.Error = Io(IoError) | Capacity(Capacity) | NotUtf8`.
- core.md says no host primitive admits; budgets.md charges every resource value 32 bytes for
  its cause cell. Which holds for `host.open` is N6; the seed assumes it admits nothing.

**Lock** is the OS lock: advisory, exclusive, whole-file, with flock semantics (Rust's
`File::try_lock`).
- It is a library resource owning the locked file: `file.try_lock(take f) -> Result[Lock,
  (LockError, HostFile)]` and `file.unlock(take l) -> Result[HostFile, (IoError, HostFile)]`. The
  lock is released exactly once, by `unlock` or by the finalizer.
- `try_lock` makes the Lock (32 bytes for its cause cell) before asking the host, so a refused
  admission leaves the host untouched. `LockError = Held | Io(IoError) | Capacity(Capacity)`.

**stdout.write** and **args** are core, restated.

## Tasks and channels

`task` and `channel` restate the core operations with their full contracts, and add nothing.
- There is no wrapper type. The core channel is already bounded, admits all of its queue storage at
  creation (`cap x size(T) + 64`), and never allocates afterwards. Capacity 0 fails with Bounds.
- `channel.sender` admits nothing: Sender, Receiver and Task have no cause cell.
- Backpressure: `send` waits while the queue is full; `try_send` returns `Full(x)` with the item.
- Dropped endpoints: CONC-9. When the last Sender is gone, the channel closes for receiving
  after its queued items. When the Receiver is gone, the channel closes for sending and the queued
  items are cleaned from the highest index down.
- `task.spawn` takes a plain `fn(take X) -> R waits`, which captures nothing, plus the `take`
  payload. `JoinError = Failed(Causes) | Cancelled(Causes)`.

## Regions and builders

A region (MEM-1 to MEM-5) holds stable values: spans and Text. Its storage is freed in one step
when the region closes. A builder (MEM-3) appends to a growing block in the region and
publishes a span when it finishes; an outgrown block stays charged until the region closes.
`build_in` and `prepare_in` admit a final size once.

The request-scratch pattern is a region per request, holding a span of plain position records
(`Header { name: (u64, u64), value: (u64, u64) }`) built over the request's text, with views
taken back through `string.slice`. Nothing is copied out of the request.

## Spec gaps

Each line gives the gap and the smallest change asked for. Full text, with the revision 2
verdict on each of G1 to G18 and P1 to P4: `SPEC-GAPS.md`. Open after revision 2:

- **N1** Where std lives relative to core modules. The seed adds members to `list`, `string`,
  `map`, `text` and `char`, which are core modules. Change: say that a std module of a core
  module's name extends it, or name the std modules differently.
- **N2** Library sub-view forwarding. OWN-20 allows sub-views only "for a core operation".
  Change: a function may return a sub-view that a core operation gave it of its anchored argument.
  This restores `split_once`, `strip_prefix` and `text.as_string`.
- **N3** `host.mkdtemp` returns a Dir but not its name, so nothing can remove the directory.
  Change: return `(Dir, String)`, or add `host.remove_dir(d: Dir)` for a Dir's own tree.
- **N4** No integer-to-float conversion operation and no `from_bits`. Change: add
  `float.from_int[T, U]` (round to nearest even, as SEM-2 already states) and `float.from_bits`.
- **N5** `OpenMode`'s arms are not defined. Change: define them (create, truncate, read or write),
  and add a read-write mode or say there is none.
- **N6** core.md: "None of them admits"; budgets.md: a resource value charges 32 bytes. Change:
  say which applies to `host.open`, and whether a failed admission is an `IoError`.
- **P5** No in-place map update. Change: add `map.set(mut m, k, take v) -> Result[V, V]`.
- **P6** Pool identity. Change: core `type Unique` (plain, key, equality only) and
  `unique.new() -> Unique`.
- **G14** CAP-6 says the map hash "may" be keyed by a per-run seed. Change: "is keyed", so that
  HashDoS is ruled out rather than left to the engine.
- **G7, G10, G12, G18** are intended gaps; the seed writes what revision 2 says to write instead.

## Operation index

Generated from the `.hirsel` files: module, operation count, how many are CORE, and the names
(`*` = CORE, `!` = NEEDS CORE). For signatures, read the file.

| Module | Ops | CORE | Operations |
|---|---|---|---|
| list | 21 | 14 | new*, with_capacity*, len*, capacity*, reserve*, get*, slice*, push*, pop*, set*, clear*, fill*, append*, sum*, remove, insert, swap_remove, swap, truncate, reverse, sort |
| bytes | 1 | 0 | sum |
| span | 7 | 7 | builder*, fixed_builder*, indexed_builder*, push*, finish*, len*, get* |
| string | 22 | 12 | new*, with_capacity, from*, copy*, from_utf8*, into_bytes*, as_bytes*, slice*, next_scalar*, byte_len*, is_boundary, push*, push_char*, push_u64*, push_i64, push_uint, make_ascii_upper, clear, find, starts_with, ends_with, compare |
| text | 2 | 2 | len*, copy_in* |
| char | 5 | 2 | from_u32*, to_u32*, utf8_len, to_ascii_upper, digit |
| int | 3 | 1 | parse*, parse_radix, compare |
| float | 2 | 0 | parse!, is_finite |
| map | 7 | 5 | new*, len*, get*, contains, insert*, set!, remove* |
| set | 5 | 0 | new, len, contains, insert, remove |
| pool | 9 | 0 | new, with_capacity, len, insert, get, get_or, contains, update, remove |
| dir | 3 | 0 | create_dir, remove_all, create_temp |
| file | 10 | 0 | open, read_into, write_all, close, read, read_string, write, remove, try_lock, unlock |
| stdout | 1 | 1 | write* |
| args | 2 | 2 | len*, get* |
| channel | 5 | 5 | new*, sender*, send*, try_send*, receive* |
| task | 4 | 4 | spawn*, join*, cancel*, check_cancel* |
| **total** | **109** | **55** | 2 marked NEEDS CORE |
