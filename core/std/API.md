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
std copies or re-exports. `string.make_ascii_upper` and `string.make_ascii_lower` are core too, written over the carrier without a second UTF-8 pass; std no longer defines them, and callers keep the same names. Modules that are std's alone (order, set, pool, dir,
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
| list | yes | 15 | first, get_or, last, remove, insert, swap_remove, truncate, reverse, sort, sum_bytes, contains, binary_search, dedup, extend, binary_search_by |
| map | yes | 3 | contains, get_or, get_or_insert |
| order |  | 5 | compare, min, max, reverse, then |
| pool |  | 11 | new, with_capacity, len, insert, get, get_or, contains, update, remove, clear, next |
| result | yes | 2 | map_err, capacity_only |
| set |  | 6 | new, len, contains, insert, remove, entries |
| string | yes | 15 | with_capacity, is_boundary, push_i64, push_uint, repeat, clear, find, starts_with, ends_with, split_once, next_split, next_line, strip_prefix, strip_suffix, trim_ascii |
| **total** | **7 of 16** | **106** | |

**Known cost.** float's push_fixed64 and its push_ascii helper build their digits as Bytes
and pass them through `string.from_utf8`, revalidating about 20 ASCII bytes per number that
they wrote themselves. That revalidation stays in std (no core entry point) until it shows in a
profile.
