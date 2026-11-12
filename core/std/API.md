# Hirsel standard library seed: API contracts

The seed holds the library contracts that the nine M1 workloads (`NINE.md`) need, plus the few
operations that complete their families. Each module is one signature-only source file in this
directory: the module's types, its function signatures and their contracts, and no bodies. This
document holds what the files share: how to read a contract, the design of each family, and the
spec gaps that still shape a signature. Every signature lives in exactly one place, its `.hirsel`
file. The operation index at the end is generated from those files.

**Authority.** The language rules are spec revision 3 (`docs/spec/*.md` and `core/core/*.hirsel`
at 7f47d961; rule ids such as MEM-26 refer to it). The core library is source in `core/core/`; the
seed restates none of it. Everything here is library code over core. No rule is bent to make a
signature legal; what revision 3 makes awkward is listed under "Spec gaps" and in full in
`SPEC-GAPS.md` in the brief folder.

**Prior art.** Names and shapes come from Rust std (with indexmap and slotmap), Go, Zig std and
Goose's standard library, in that order of preference. A shape differs from all of them only where
a Hirsel rule forces the difference. `PRIOR-ART.md` in the brief folder says, for each family,
where the shape was taken from and which rule forced each difference.

## Files

A module with a core module's name **extends** it (LANG-7): `list.push` is core's and `list.sort`
the seed's, read through one name. No extension declares a core member's name.

| Module | Extends core | Types it declares | What it adds |
|---|---|---|---|
| list | yes | | removal, insertion, reordering, sort, first and last |
| bytes | | | `sum` in u64 for `Bytes` (= `List[u8]`) |
| string | yes | | capacity, formatting, search, cutting by sub-view |
| char | yes | | ASCII helpers and digit values |
| int | yes | | radix parsing at u64 and i64 |
| float | yes | Error | correctly rounded parsing at f64 and f32, `is_finite` |
| order | | | `compare`, `min`, `max` over the `ordered` bound |
| map | yes | | `contains`, `get_or` |
| set | | Set | insertion-ordered hash set over Map |
| pool | | Pool, Id, Stale | generation-checked slot pool (MEM-18) |
| result | | | explicit error conversion for `?` |
| dir | | | directory operations on a Dir |
| file | | Error, Lock, LockError | files and the OS lock |

Core modules the seed does not extend, and uses as they are: `core` (the prelude), `box`, `rc`,
`arc`, `unique`, `text`, `span`, `dest`, `task`, `channel`, `args`, `host`, `stdout`, `stderr`.

The extension is `.hirsel` so the old toolchain's `.hir` discovery never sees these files.

## Reading a contract

Each `pub fn` has `///` lines with these fields. A field that does not apply is left out.

- **Summary**: what the operation does, with the rule ids it relies on. Library code names the
  core operations it is built from.
- **Fails:** how the operation can go wrong and what the caller gets back:
  - `Error(x)` is the operation's typed error.
  - "Bounds (failure)" is a failure in the FAIL sense, used only for caller bugs that a typed
    error could not usefully report: a base outside 2 to 36, an index out of range.
  - Each refused admission returns every value the operation was given to take, untouched,
    inside the error (`Error((c, x))`), as core.md requires of core operations.
- **Alloc:** the bytes the operation admits, which limit they are charged to, and when they are
  refunded. "Current limit" means the innermost `bound` or the task root in force at the call
  (MEM-19). "Nothing" means the operation admits no bytes.
- **View:** what a returned view is anchored to (OWN-20, `in p`) and what it freezes.
- **Prefix:** for capability operations, what has already happened on the host when the
  operation fails or is cancelled part way.
- **Cost:** time, in the length of the arguments. Amortised and expected costs say so.
- **Ex:** one smallest call. An argument for a `take` parameter is written `take p` when it is a
  place and bare when it is a fresh value (OWN-7); patterns bind without `take`. An example
  ending in `?` assumes the enclosing function's error type is exactly the call's (LANG-18).

Two defined terms:

- **GROWTH**: when the elements no longer fit, the operation admits a new block of
  `max(needed, 4, 2 x capacity)` elements at the limit where the container was made (MEM-26),
  not the current limit, copies the elements and refunds the old block. The new block is admitted
  before the old one is refunded. If admission is refused, the container is unchanged.
- **FREEZE x**: while the returned view lives, `x` is frozen (OWN-10): it cannot be written,
  moved, given out as `mut` or `take`, or cleaned. Viewing it again is allowed.

## Conventions

**Module-qualified calls, no methods.** A call is `list.push(mut xs, take x)`. A type a library
module declares is written with its module path outside it (`set.Set`, `pool.Pool`,
`file.Lock`); core types are written bare.

**Errors carry what was not taken.** An operation that takes ownership and can fail returns the
taken values in its error, in parameter order: `Result[(), (Capacity, T)]`,
`Result[file.Lock, (file.LockError, HostFile)]`. This is core.md's rule, applied to every taking
operation in the library.

**`?` does not convert.** LANG-18 propagates an error only into a function whose error type is
exactly the same. `result.map_err(r, f)` converts with a scoped callback, and
`result.capacity_only(r)` drops the returned argument from a `(Capacity, X)` error. A one-off
conversion is a `match`.

**Admission is the commit point.** Every operation that allocates admits all of its bytes
before it changes anything observable, so a refused admission leaves every argument as it was.
When an operation also has a host effect, admission happens before the host is asked
(`file.try_lock` makes its Lock before calling `host.try_lock`).

**No hidden copies, allocation or retention.** A `View` result never copies. An operation that
copies says so, and its result is a new owner (`string.from`, `text.copy_in`). No operation keeps
a reference to an argument after it returns, and none grows a container the caller did not pass
as `mut`.

**Option for absence, Result for refusal.** `list.get` and `map.get` give `Option`: absence is a
normal answer. `int.parse_uint` and `pool.get` give `Result`: the error says why.

**One width at a time for numbers.** No bound names the integer types or the float types
(OWN-4), so a library function cannot be generic over them: `int.parse_uint` is at u64,
`float.parse64` at f64. A narrower result is `int.try_narrow` of the wide one; a float is parsed
at its own width (`parse32`), because narrowing an f64 rounds twice.

**Capability operations are library code over host primitives** (`core/core/host.hirsel`). Each
`dir` and `file` operation names the primitives it calls. They pass the replay point at issue
and at delivery (CAP-8), and are `waits`, so each is a cancellation point (CONC-7). Paths are
`String`s resolved relative to their Dir, which they may not leave.

**Costs are worst case unless marked amortised or expected.** Hash operations are expected
O(1); the hash is keyed per run (SEM-5).

## Sequences: list, bytes

**list** follows Rust's `Vec` and Go's slices. Core gives `new`, `with_capacity` (exact), `len`,
`capacity`, `reserve`, `get`, `slice`, `push`, `pop`, `set`, `clear`, `fill`, `append` and `sum`.
The library adds `remove`, `insert`, `swap_remove`, `swap`, `truncate`, `reverse` and `sort`, each
built from `pop`, `push` and `set` chains (`set` returns the old element, so moving elements
needs no view and no placeholder), and `first` and `last`, which return `list.get`'s sub-view
(OWN-20).
- Removing never shrinks storage. `clear` and `truncate` clean the removed elements from the
  highest index down.
- `sort` is unstable pattern-defeating quicksort with a heapsort fallback (Rust's
  `sort_unstable`), so it allocates nothing. Its `before` is a `scoped fn` (OWN-21). A stable sort
  would need a buffer of len/2 elements; it is left out of the seed.

**Bytes is `List[u8]`.** Every list operation applies, so the byte module is only `bytes.sum`,
which sums in u64 where `list.sum` would overflow u8.

**Spans are core only.** v1 keeps `FixedSpan[T]` of fixed-size plain elements and `Text`
(MEM-12); `span.builder`, `span.push` and `span.finish` build one in a region (MEM-3), and
`build_in` and `prepare_in` build compact values (MEM-4). The seed adds nothing to them.

## Text: string, char, int, float

**string** is Rust's `String` vocabulary under module names.
- Core: `new`, `from` (the copy), `from_utf8` (returns the Bytes on failure), `into_bytes`,
  `as_bytes` and `slice` (sub-views), `next_scalar`, `byte_len`, `push`, `push_char`, `push_u64`.
- Library: `with_capacity` (`from_utf8` of `list.with_capacity`), `push_i64`, `push_uint` (base,
  width, pad), `make_ascii_upper` (through `into_bytes` and `from_utf8`, an O(n) revalidation),
  `clear`, `is_boundary`, `find`, `starts_with`, `ends_with`.
- Cutting, by sub-view (OWN-20): `split_once`, `strip_prefix`, `strip_suffix`, `trim_ascii`. Each
  returns what `string.slice` gave it for s, so it allocates nothing and freezes s:
  ```
  let Some((name, value)) = string.split_once(line, ": ") else { return Error(Malformed) }
  ```
- Order is `order.compare`; there is no `string.compare`.

Positions are byte offsets, as in Rust and Go. `slice` fails with Bounds at a non-boundary, so
code cutting at positions from input checks `is_boundary` first.

**Iterating scalars** is `while let Some((c, next)) = string.next_scalar(s, at)`, Go's
`utf8.DecodeRuneInString` loop. `for` iterates only built-in sources (SEM-6).

**text** is core only (`len`, `copy_in`). `text.as_string` stays impossible: Text and String have
different representations (a count word then bytes, against an own header), so no String view of
a Text exists to forward.

**char**: `from_u32` and `to_u32` are core; the library adds `utf8_len`, `to_ascii_upper` and
`digit(c, base)`. The type `char` and the module `char` live in separate namespaces (LANG-7).

**int**: core has `parse` at every width (decimal). The library adds `parse_uint(s, base) -> u64`
and `parse_int(s, base) -> i64` (Go's ParseUint and ParseInt).

**float**: `parse64` and `parse32` (JSON number syntax, correctly rounded, `OutOfRange` when the
value rounds to an infinity) and `is_finite`. The algorithm is Rust's dec2flt over core's
`float.from_int` and `float.from_bits64`/`from_bits32`: Clinger's exact fast path, then
Eisel-Lemire, then a big-decimal slow path. Three things cost more than in Rust:
- the 651 powers of five are a private function over one `match`, because a `const` holds one
  plain value and v1 has no fixed-size array;
- each 64 x 64 -> 128 product is four 32 x 32 products, because v1 has no u128 or high-half
  multiply;
- the slow path's 768-digit buffer is a `Bytes`, admitted once, because it cannot be a stack
  array; so the error is `float.Error = Parse(ParseError) | Capacity(Capacity)`.

Float formatting is left out of the seed: json keeps its number lexemes as text.

## Map and Set

**Shape.** `Map[K: key, V]` is insertion-ordered (SEM-5) with the indexmap / CPython 3.7+ layout:
an entry array in insertion order, where each entry is `(hash, key, value)` or a tombstone, and
an open-addressed index of entry positions, at least twice the entry capacity. `map.insert` on a
present key replaces the value in place and keeps the stored key and its position: indexmap's
`insert`, Python's dict, and the json workload's duplicate-key rule.

**Charges.** One block of `capacity x (size(K) + size(V) + 16)`, rounded up to 8: the 16 bytes per
entry pay for the 8-byte hash and two 4-byte index slots, so a map holds at most 2^32 - 1 entries.
The block is charged to the limit where the map was made (MEM-26).

**Removal and compaction.** `map.remove` turns the entry into a tombstone and returns the value,
O(1) expected (indexmap's `shift_remove` order without its O(n) shift). When `insert` finds the
entry array full and at least half of it is tombstones, it compacts in place with no admission;
otherwise it grows and the copy drops the tombstones. A map whose live size stays bounded never
grows without limit under churn (lru: 64 live entries, 10,000 puts).

**Update.** Core `map.set(mut m, k, take v)` replaces a present key's value, takes no key and
admits nothing. A count bump is `map.get_or` then `map.set`: two probes, where Rust's entry API
does one. A general `update(f)` for an own `V` would need a placeholder value to leave behind, so
it is left out.

**Iteration.** `for (k, v) in m` binds `(View[K], View[V])` in insertion order and freezes `m`.

**Set** is `opaque type Set[K: key] { m: Map[K, ()] }` (Rust's `HashSet` over `HashMap`), written
`set.Set` outside its module, with storage `capacity x (size(K) + 16)`. `set.insert` returns
whether the key was new. A set is not iterable (SEM-6); none of the nine iterates one.

## Pool

**Shape.** MEM-18's library pool, as a slotmap: `Pool[T] { me: Unique, slots: List[Slot[T]],
free: u64, live: u64 }`, where `Slot[T] = Occupied(generation, T) | Vacant(generation,
next_free)`. Vacant slots form an intrusive free list, so `remove` never allocates and `insert`
reuses the most recently vacated slot first. A slot's generation rises by one on each removal;
at 2^64 - 1 the slot retires and is never reused, so an old Id never validates against a later
occupant (slotmap wraps 32-bit versions instead, which can alias).

**One handle, checked at run time.** `Id[T]` is plain data: the pool's `Unique`, a slot index
and a generation, 24 bytes, `plain + key`. Every access (`get`, `get_or`, `contains`, `update`,
`remove`) compares all three and returns `Error(Stale)` on a mismatch, with `Stale = WrongPool |
Removed`. An Id is stored anywhere: lru's index is a `Map[String, pool.Id[Entry]]` and its entries
link with `Option[pool.Id[Entry]]`. Dropping the pool cleans what is left. A `clone` of a pool
keeps its identity, so an Id of the original resolves in the copy (MEM-18).

**Update without a mutable view.** `pool.update(mut p, h, f)` takes the value out of its slot
with `list.set` and a `Vacant` placeholder, calls `f` on the owned local, and installs it again.
If `f` fails, the value is cleaned with the local and the slot is left vacant at a new
generation.

**Not in the seed:** `pool.clear` and pool iteration.

## Capabilities: dir, file

`main`'s capability parameters are granted by the run configuration (CAP-4). `args.get`,
`stdout.write`, `stderr.write`, `host.wall_ms` and `host.mono_ns` are core and used directly.

**dir**: `create_dir` (`host.mkdir`), `remove_all` (`host.remove_tree`) and `create_temp`
(`host.mkdtemp`, returning the new Dir and its name in the parent, so `remove_all(d, name)`
removes it). No primitive returns a Dir for an existing directory, so there is no `narrow`.

**file** works on core's `HostFile`, a mechanical `send` resource (MEM-24): opening admits
nothing, and its finalizer closes it and ignores the outcome, as Rust's `Drop for File` does.
- `open(d, p, mode)` with core's `OpenMode`: Read (existing, for reading), Write (create or
  empty), Append (create or append), CreateNew (create, `Exists` if present). There is no
  read-write mode.
- `read_into` fills spare capacity (`host.read`), `write_all` returns core's
  `WriteError { written, error }`, and `close` reports the close error.
- `read`, `read_string`, `write` and `remove` are the whole-file forms. `read` and
  `read_string` allocate, and `IoError` has no Capacity or invalid-data arm, so they return
  `file.Error = Io(IoError) | Capacity(Capacity) | NotUtf8`.

**Lock** is the OS lock: advisory, exclusive, whole-file, flock semantics (Rust's
`File::try_lock`). It is a library resource owning the locked file: `file.try_lock(take f) ->
Result[Lock, (LockError, HostFile)]` and `file.unlock(take l) -> Result[HostFile, (IoError,
HostFile)]`, so the lock is released exactly once, by `unlock` or by the finalizer. Lock is not
mechanical, so making one admits its 32-byte cause cell (MEM-24); `try_lock` makes it before
asking the host. `LockError = Held | Io(IoError) | Capacity(Capacity)`.

## Tasks, channels and regions

The seed adds nothing to `task`, `channel`, `span`, `text` or regions; their contracts are core's.
The request-scratch pattern is a region per request holding a `FixedSpan` of plain position
records (`Header { name: (u64, u64), value: (u64, u64) }`) built with `span.builder` over the
request's text, read back through `string.slice`; nothing is copied out of the request.

## Spec gaps

Every revision-2 gap is closed by revision 3 (verdicts in `SPEC-GAPS.md`). What revision 3 still
makes awkward for the library, most useful first (R-numbers in `SPEC-GAPS.md`):

- **R1** `?` has no conversion, so every allocation inside a function with its own error type is
  a `match` or `result.map_err`. Intended; the seed adds `result`.
- **R2** No numeric bound: a library function cannot be generic over the integer or the float
  types, so `parse_uint`/`parse_int` and `parse64`/`parse32` are written per width.
- **R3** No fixed-size array: `float.parse*`'s slow path allocates (hence `float.Error`), and its
  power-of-five table is a 651-arm `match`.
- **R4** OWN-20 says a result may be "a sub-view that a call gave for that argument". `pool.get`
  returns part of what `list.get` gave for `p.slots`, a part of the argument. The text should say
  that a part of the argument counts as the argument here.
- **R5** No wide multiply (`u128` or a high half), so Eisel-Lemire costs about 4x the multiplies.
- **R6** `span.builder` has no capacity hint, so a builder whose count is known still abandons
  its growth blocks to the region (request-scratch: a few hundred bytes per request).
- **R7** Compact sequences of variable-size elements are not in revision 3. They will return
  in the first version (goose-parts-897); json's matched variant, one blob per document, waits
  for them. The seed's json uses own `List` and `Map` and is unaffected (NINE.md).

## Operation index

Generated from the `.hirsel` files: module, whether it extends a core module, operation count
and names. For signatures, read the file.

| Module | Extends | Ops | Operations |
|---|---|---|---|
| list | yes | 9 | first, last, remove, insert, swap_remove, swap, truncate, reverse, sort |
| bytes | | 1 | sum |
| string | yes | 13 | with_capacity, is_boundary, push_i64, push_uint, make_ascii_upper, clear, find, starts_with, ends_with, split_once, strip_prefix, strip_suffix, trim_ascii |
| char | yes | 3 | utf8_len, to_ascii_upper, digit |
| int | yes | 2 | parse_uint, parse_int |
| float | yes | 3 | parse64, parse32, is_finite |
| order | | 3 | compare, min, max |
| map | yes | 2 | contains, get_or |
| set | | 5 | new, len, contains, insert, remove |
| pool | | 9 | new, with_capacity, len, insert, get, get_or, contains, update, remove |
| result | | 2 | map_err, capacity_only |
| dir | | 3 | create_dir, remove_all, create_temp |
| file | | 10 | open, read_into, write_all, close, read, read_string, write, remove, try_lock, unlock |
| **total** | **7 of 13** | **65** | |
