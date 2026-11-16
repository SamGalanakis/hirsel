# Hirsel standard library seed: API contracts

The seed holds the library contracts that the nine M1 workloads (`NINE.md`) need, plus the few
operations that complete their families. Each module is one source file in this directory:
its types, and each function's signature, contract and body. This document holds what the files
share: how to read a contract, the design of each family, and the spec gaps that still shape a
signature. Every signature and every implementation lives in exactly one place, its `.hirsel`
file. The operation index under "Files" is generated from those files by
`python3 core/tools/std_api_index.py --write` (`--check` exits 1 when it is stale).

**Core and std code.** `core/core/*.hirsel` contains the core types, engine-backed
operations and Hirsel bodies over private kernels. `core/std/*.hirsel` extends that library
with Hirsel bodies over the public core operations. A body can therefore occur in either
directory. `Deque[T]`, `Heap[T]`, `list.swap` and `list.sort_stable` belong to core;
`list.sort` is std's unstable sort. Each public operation is declared in one module.
A plain `fn` or `const` is private to its module. Contract tests live under
`core/core/tests` and `core/std/tests`: small functions with hand-derived
`// expect: run f() = R` oracles. `python3 core/tools/std_native.py --backend both`
runs them through the package product on the two native engines; `--filter` selects
named cases, and `--backend evaluator` runs host-free cases on the test evaluator.

**Naming std from another package.** There are no imports (LANG-7). A module that extends core
(list, string, char, int, float, map, result) merges into core's name, so `list.sort` and
`string.find` are written bare everywhere. Core collections use bare names: `Deque[T]`, `deque.push_back`, `Heap[T]`,
`heap.pop`, `list.swap` and `list.sort_stable`. The stable sort uses one scratch block
of `len * size(T)` bytes; a comparator failure preserves every element in unspecified
order, and Capacity leaves the list unchanged. The Deque ring uses uninhabited slots,
and the Heap uses hole sifts over one List. Their modules are in `core/core`, with no
std copies or re-exports. `string.make_ascii_upper` and `string.make_ascii_lower` are core too, written over the carrier without a second UTF-8 pass; std no longer defines them, and callers keep the same names. Modules that are std's alone (order, omap, set, pool, tally, dir,
file) are reached from another package through the package name: `std.order.compare`,
`std.set.Set[u64]`, `std.pool.Pool[T]`, `std.file.read(d, p)`, `std.file.Error`. Variants are
written bare (`Held(f)`, `NotUtf8`).

**json.** `std.json` is the matched json workload's library: the compact document
(`Json`, `Member`), its exact sizing pass (`size`), `parse` in one reservation, and the object
key index (`ObjectIndex`: `index`, `get`, `shown`). Walking and positional access use core's
`match`, `seq.len` and `indexed.get`. A key or a Str is read through `text.as_string`.

**Authority.** The language rules are spec round 4 (`docs/spec/*.md` and `core/core/*.hirsel`
at 0d649bad; rule ids such as MEM-26 refer to it). The core library is source in `core/core/`; the
seed restates none of it. Everything here is library code over core. No rule is bent to make a
signature legal; what round 4 makes awkward is listed under "Spec gaps" and in full in
`SPEC-GAPS.md` in the brief folder.

**Prior art.** Names and shapes come from Rust std (with indexmap and slotmap), Go, Zig std and
Goose's standard library, in that order of preference. A shape differs from all of them only where
a Hirsel rule forces the difference. `PRIOR-ART.md` in the brief folder says, for each family,
where the shape was taken from and which rule forced each difference.

## Files

A module with a core module's name **extends** it (LANG-7): `list.push` is core's and `list.sort`
the seed's, read through one name. No extension declares a core member's name.

| Module | Extends | Ops | Operations |
|---|---|---|---|
| arena |  | 3 | batch, copy_span, collect_span |
| char | yes | 7 | utf8_len, to_ascii_upper, to_ascii_lower, is_ascii_digit, is_ascii_alpha, is_ascii_whitespace, digit |
| clock |  | 1 | sleep_ns |
| codec |  | 141 | make, least, encode, decode, decode_at, need, full, put_unit, take_unit, put_bool, take_bool, put_u8, take_u8, put_u16, take_u16, put_u32, take_u32, put_u64, take_u64, put_i8, take_i8, put_i16, take_i16, put_i32, take_i32, put_i64, take_i64, put_f32, take_f32, put_f64, take_f64, put_char, take_char, put_string, take_string, put_tag, take_tag, put_with, take_with, put_list, take_count, room, take_list, put_map, take_map, put_option, take_option, put_result, take_result, take_via, of_unit, of_bool, of_char, of_u8, of_u16, of_u32, of_u64, of_i8, of_i16, of_i32, of_i64, of_f32, of_f64, of_string, of_decode_error, schema_unit, schema_bool, schema_char, schema_u8, schema_u16, schema_u32, schema_u64, schema_i8, schema_i16, schema_i32, schema_i64, schema_f32, schema_f64, schema_string, schema_of, schema_list, schema_map, schema_option, schema_result, schema_tuple, define, argument, record, sum, via, close, field, field_default, removed, variant, schema, fingerprint, fingerprint_of, resolve, read, read_at, key_make, encode_key, decode_key, key_schema, schema_key, put_key_with, take_key_with, put_key_u8, take_key_u8, put_key_u16, take_key_u16, put_key_u32, take_key_u32, put_key_u64, take_key_u64, put_key_i8, take_key_i8, put_key_i16, take_key_i16, put_key_i32, take_key_i32, put_key_i64, take_key_i64, put_key_bool, take_key_bool, put_key_bytes, take_key_bytes, put_key_string, take_key_string, key_u8, key_u16, key_u32, key_u64, key_i8, key_i16, key_i32, key_i64, key_bool, key_bytes, key_string |
| dir |  | 3 | create_dir, remove_all, create_temp |
| file |  | 10 | open, read_into, write_all, close, read, read_string, write, remove, try_lock, unlock |
| float | yes | 7 | parse64, parse32, is_finite, push_f64, push_fixed64, shortest64, exp |
| int | yes | 5 | parse_uint, parse_int, is_ascii_digit, is_ascii_alpha, to_ascii_lower |
| io |  | 7 | lines, prefix, read_line, out, pending, flush, write |
| json |  | 6 | size, parse, push_json, index, get, shown |
| list | yes | 16 | first, get_or, last, remove, insert, swap_remove, truncate, reverse, sort, sum_bytes, contains, binary_search, dedup, push_copy, extend, binary_search_by |
| map | yes | 3 | contains, get_or, get_or_insert |
| omap |  | 23 | new, snapshot, len, root, from_root, node_id, node_level, node_count, node_len, node_key, node_value, node_child, leaf, branch, from_sorted, get, insert, insert_owned, remove, first, last, range, next |
| order |  | 5 | compare, min, max, reverse, then |
| pool |  | 11 | new, with_capacity, len, insert, get, get_or, contains, update, remove, clear, next |
| result | yes | 2 | map_err, capacity_only |
| set |  | 6 | new, len, contains, insert, remove, entries |
| string | yes | 15 | with_capacity, is_boundary, push_i64, push_uint, repeat, clear, find, starts_with, ends_with, split_once, next_split, next_line, strip_prefix, strip_suffix, trim_ascii |
| tally |  | 7 | new, handle, index, update, flush, read, finish |
| **total** | **7 of 19** | **278** | |

**Known cost.** float's push_fixed64 and its push_ascii helper build their digits as Bytes
and pass them through `string.from_utf8`, revalidating about 20 ASCII bytes per number that
they wrote themselves. That revalidation stays in std (no core entry point) until it shows in a
profile.

## Ordered persistent map

`std.omap.OMap[K: ordered + shareable + send, V: shareable + send]` is a
persistent B+ tree with maximum fan-out 32 and minimum non-root occupancy 16.
`new` allocates nothing, `snapshot` shares the root in O(1) without allocation,
and `len` reads its entry count. `get`, `first` and `last` borrow one version.
An update returns a new version while every held version retains its contents.
Keys and values occupy contiguous node lists. Path copies clone their payloads
and share untouched child subtrees. Resources fail the ordinary `shareable` bound.
Versions are sendable; last-holder release frees unreachable nodes and payloads
through the existing constant-stack Arc worklist.

`insert(mut ids,m,k,v)` and `remove(mut ids,m,k)` path-copy, split, merge or
redistribute O(log n) nodes, at most 32 holders per node. `ids` is the writer's
last issued u64, shared across all branches. Every new node receives the next
id after its children; an exhausted counter raises checked Overflow. Capacity
preserves m; insertion returns its taken key/value. Failed attempts may consume
ids, which must never be reused. These APIs continue to borrow m.

`insert_owned(mut ids,take m,take k,take v)` is a separate consuming update.
At a unique Arc, it transfers the owned node through `try_unwrap`, edits its
lists, then publishes it with a new id through `arc.new`; unchanged child
holders move without increments. Shared nodes use the ordinary path-copy.
Unwrap/new still frees and allocates one wrapper per visited unique node.
Capacity consumes the draft m and returns k/v; keep an independent snapshot
for rollback. Every held version and its node ids stay unchanged.

`range(m,take lower,take upper)` accepts `Unbounded`, `Included(k)` or
`Excluded(k)` endpoints and returns an owned, sendable `Cursor`. It reserves one
Arc frame per level, seeks recursively in O(log n), and pins the version from
which it was made. `next(mut cursor)` returns `Result[Option[(K,V)],Capacity]`.
It copies just that row, and Capacity leaves that row unconsumed. Traversing k
rows costs O(k + log n) node work and the cost of copying their payloads.
Reversed bounds yield an empty cursor. A cursor can outlive its originating map
and move to another task. User cursors are read with `while let`, not `for`.

Checkpoint writers use `root` for an Arc holder and `node_id`, `node_level`,
`node_count`, `node_len`, `node_key`, `node_value`, `node_child` for individual
nodes. Node level is zero at leaves. Leaf count is the number of key/value
pairs; branch count is the number of children and their minimum keys. Keys,
values and children outside their count return None. Branches have no values;
leaves have no children. Node fields are stamp:u64, depth:u64, size:u64 and
three 24-byte List headers. Logical node charge is 104 bytes plus the list
capacities times their element sizes: K/V at leaves, K/Arc at branches, plus
owned payload allocations. Full u64/u64 leaves charge 616 bytes. Consuming
updates reserve spare leaf slots explicitly; fan-out by count stays 32.

A checkpoint cutoff can skip a whole subtree whose id is already persisted.
`leaf(id,take keys,take values)` and `branch(id,take low_keys,take children)`
construct decoded nodes with recorded ids, without rechecking tree invariants.
The decoder must provide the documented shape. On Capacity they return their
input lists. `from_root(take optional_root)` publishes them in O(1). Loading is
eager and bottom-up, O(n) nodes plus payload bytes. There are no disk, codec or
unloaded-child operations in omap.

`from_sorted(mut ids,keys,values)` builds bottom-up in O(n) payload work without
path copies. Its borrowed input lists stay unchanged on Capacity. Like the
node constructors, callers supply the invariant: strictly increasing keys
and equally many values. It balances the last groups to retain minimum
occupancy 16, stamps children before parents, and preserves the same node views.

## Tally

`std.tally.Tally[T: send]` counts and folds across tasks with no shared state; it is
the library form that takes the place of atomics for counting (atomics-moonshot-1009,
section 3.1; Java's LongAdder, OTP `counters` and Linux per-CPU counters are its
prior art). `new(n, start, combine)` opens one channel of max(n, 1) items, admitted
at creation, and reserves n slots; `start: fn() -> T` makes a fresh partial and
`combine: fn(take T, take T) -> T` folds partials, both cold. `handle(mut t)` issues a
`send` resource `Handle[T]` with the next index (0, 1, ... in the owner's program
order) and the partial `start()`; after n handles it returns `Error(Exhausted)`, and a
refused handle block returns `Error(Capacity(c))`, leaving the tally unchanged either
way. `index(h)` reads that index, which with a local sequence makes a deterministic id
with no event.

`update(mut h, f: scoped fn(mut T) -> R) -> R` applies f to the handle's own partial
in place, the `map.update` shape: f is given at the call site, so the engines inline
it into the caller's loop, and the partial is a plain T that is never empty, so there
is no tag test. A counter is `update(mut h, fn(mut n: u64) { n += 1 })`, a float sum
`fn(mut s: f64) { s += x }`, a concatenation `fn(mut s: String) { result.unwrap(string.push(mut s, piece)) }`.
`update` is the only way to change a partial. It is neither an event nor a
cancellation point and touches nothing another task can see. A handle's cleanup sends
`(index, partial)` once, so a handle dropped by a failing or cancelled task delivers
its partial (FAIL-8), as far as an `update` that failed had changed it.
`finish(take t) -> T waits` drops the owner's sender, receives until every handle is
cleaned and folds `start()` then the partials in handle order. The partition of
updates among handles and their order within each are fixed by the programs, so the
result is the same under every schedule, also for a combine that is neither
associative nor commutative (a float sum, a concatenation). `finish` waits for every
issued handle: a handle moved into a long-lived child delays it until that child ends.

`flush(mut h) waits` sends the partial so far and starts a fresh `start()`; it is an
event, and one handle's parts fold into its slot in send order. `read(mut t)` (T:
plain) drains what has arrived with `try_receive` and returns `start()` folded with
the slots so far; its value depends on the schedule. Each handle's cleanup always fits
the queue; a flush or a cleanup waits only when flushes have filled it while the owner
is not reading or finishing, so an owner that joins a flushing task before reading can
wait forever.
The scripted-schedule laws are in `tests/tally.hirsel` (`// schedule: seeds N`, run
by `core/tools/std_schedule.c`); the evaluator has no tasks, so it runs only the
single-task laws.
