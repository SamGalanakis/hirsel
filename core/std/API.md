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
| dir |  | 3 | create_dir, remove_all, create_temp |
| file |  | 10 | open, read_into, write_all, close, read, read_string, write, remove, try_lock, unlock |
| float | yes | 7 | parse64, parse32, is_finite, push_f64, push_fixed64, shortest64, exp |
| int | yes | 5 | parse_uint, parse_int, is_ascii_digit, is_ascii_alpha, to_ascii_lower |
| io |  | 7 | lines, prefix, read_line, out, pending, flush, write |
| json |  | 6 | size, parse, push_json, index, get, shown |
| list | yes | 16 | first, get_or, last, remove, insert, swap_remove, truncate, reverse, sort, sum_bytes, contains, binary_search, dedup, push_copy, extend, binary_search_by |
| map | yes | 3 | contains, get_or, get_or_insert |
| omap |  | 21 | new, snapshot, len, root, from_root, node_id, node_level, node_count, node_len, node_key, node_value, node_child, leaf, branch, get, insert, remove, first, last, range, next |
| order |  | 5 | compare, min, max, reverse, then |
| pool |  | 11 | new, with_capacity, len, insert, get, get_or, contains, update, remove, clear, next |
| result | yes | 2 | map_err, capacity_only |
| set |  | 6 | new, len, contains, insert, remove, entries |
| string | yes | 15 | with_capacity, is_boundary, push_i64, push_uint, repeat, clear, find, starts_with, ends_with, split_once, next_split, next_line, strip_prefix, strip_suffix, trim_ascii |
| tally |  | 7 | new, handle, index, add, flush, read, finish |
| **total** | **7 of 18** | **135** | |

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
Keys and values have separate immutable Arc owners, so path copies share both
payloads and untouched subtrees. Resources fail the ordinary `shareable` bound.
Versions are sendable; last-holder release frees unreachable nodes and payloads
through the existing constant-stack Arc worklist.

`insert(mut ids,m,k,v)` and `remove(mut ids,m,k)` path-copy, split, merge or
redistribute O(log n) nodes, at most 32 holders per node. `ids` is the writer's
last issued u64, shared across all branches. Every new node receives the next
id after its children; an exhausted counter raises checked Overflow. Capacity
preserves m; insertion returns its taken key/value. Failed attempts may consume
ids, which must never be reused. There is no unique-owner mutation fast path:
these APIs borrow m, and a held checkpoint's node identities must stay fixed.

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
three 24-byte List headers. A full node's logical charge is 616 bytes excluding
separately counted key/value payloads; exact node lists never grow implicitly.

A checkpoint cutoff can skip a whole subtree whose id is already persisted.
`leaf(id,take keys,take values)` and `branch(id,take low_keys,take children)`
construct decoded nodes with recorded ids, without rechecking tree invariants.
The decoder must provide the documented shape. On Capacity they return their
input lists. `from_root(take optional_root)` publishes them in O(1). Loading is
eager and bottom-up, O(n) nodes plus payload bytes. There are no disk, codec or
unloaded-child operations in omap.

## Tally

`std.tally.Tally[T: send]` counts and folds across tasks with no shared state; it is
the library form that takes the place of atomics for counting (atomics-moonshot-1009,
section 3.1; Java's LongAdder, OTP `counters` and Linux per-CPU counters are its
prior art). `new(n, take zero, combine)` opens one channel of max(n, 1) items, admitted
at creation, and reserves n slots. `handle(mut t)` issues a `send` resource
`Handle[T]` with the next index (0, 1, ... in the owner's program order); after n
handles it returns `Error(Exhausted)`, and a refused handle block returns
`Error(Capacity(c))`, leaving the tally unchanged either way. `index(h)` reads that
index, which with a local sequence makes a deterministic id with no event.

`add(mut h, take x)` folds x into the handle's own partial (x itself when the partial
is empty, else `combine(partial, x)`). It is neither an event nor a cancellation point
and touches nothing another task can see. A handle's cleanup sends `(index, partial)`
once, so a handle dropped by a failing or cancelled task delivers its partial (FAIL-8).
Only a combine that fails inside `add` loses that handle's partial, with the task.
`finish(take t) -> T waits` drops the owner's sender, receives until every handle is
cleaned and folds `zero` then the partials in handle order. The partition of adds among
handles and the order of adds within each are fixed by the programs, so the result is
the same under every schedule, also for a combine that is neither associative nor
commutative (a float sum, a concatenation). `finish` waits for every issued handle: a
handle moved into a long-lived child delays it until that child ends.

`flush(mut h) waits` sends the partial so far and empties it; it is an event, and one
handle's parts fold into its slot in send order. `read(mut t)` (T: plain) drains what
has arrived with `try_receive` and returns zero folded with the slots so far; its value
depends on the schedule. Each handle's cleanup always fits the queue; a flush or a
cleanup waits only when flushes have filled it while the owner is not reading or
finishing, so an owner that joins a flushing task before reading can wait forever.
The scripted-schedule laws are in `tests/tally.hirsel` (`// schedule: seeds N`, run
by `core/tools/std_schedule.c`); the evaluator has no tasks, so it runs only the
single-task laws.
