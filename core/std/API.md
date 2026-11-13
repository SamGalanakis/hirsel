# Hirsel standard library seed: API contracts

The seed holds the library contracts that the nine M1 workloads (`NINE.md`) need, plus the few
operations that complete their families. Each module is one source file in this directory:
its types, and each function's signature, contract and body. This document holds what the files
share: how to read a contract, the design of each family, and the spec gaps that still shape a
signature. Every signature and every implementation lives in exactly one place, its `.hirsel`
file. The operation index at the end is generated from those files.

**Core declarations and library code.** `core/core/*.hirsel` is the core library. It holds
declarations only: a function there has no body, because each engine provides it. `core/std/*.hirsel`
is library code. Every function there has a body written in Hirsel over `core/core`, and nothing
else, so you can tell core from library by the directory, and in a file by whether the `fn` has a
body. A `pub fn` is the API. A plain `fn` or `const` is an implementation detail of its module.
`core/std/tests/<module>.hirsel` holds the contract tests: small functions, each preceded by
`// expect: run f() = R`, with the expected values derived by hand. To see what the
front-end checker does with each body and test (the counts, then a table per function and per
contract-test run), run
`cargo test -p hirsel-core-front std_seed_check_report -- --ignored --nocapture`. It prints to the
test log and writes nothing to the tree; the report depends on the checker, so it is not tracked.

**Naming std from another package.** There are no imports (LANG-7). A module that extends core
(list, string, char, int, float, map, result) merges into core's name, so `list.sort` and
`string.find` are written bare everywhere. The deque and the heap are core modules (`Deque[T]`, `deque.push_back`, `Heap[T]`, `heap.pop`; docs/spec/core.md), not std: `std.deque` and `std.heap` are deleted, not re-exported, and `list.swap` and `list.sort_stable` are core. `string.make_ascii_upper` and `string.make_ascii_lower` are core too, written over the carrier without a second UTF-8 pass; std no longer defines them, and callers keep the same names. Modules that are std's alone (order, set, pool, dir,
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
| list | yes | 10 | first, last, remove, insert, swap_remove, swap, truncate, reverse, sort, sum_bytes |
| string | yes | 12 | with_capacity, is_boundary, push_i64, push_uint, clear, find, starts_with, ends_with, split_once, strip_prefix, strip_suffix, trim_ascii |
| char | yes | 3 | utf8_len, to_ascii_upper, digit |
| int | yes | 2 | parse_uint, parse_int |
| float | yes | 3 | parse64, parse32, is_finite |
| order |  | 3 | compare, min, max |
| map | yes | 2 | contains, get_or |
| set |  | 5 | new, len, contains, insert, remove |
| pool |  | 9 | new, with_capacity, len, insert, get, get_or, contains, update, remove |
| result | yes | 2 | map_err, capacity_only |
| dir |  | 3 | create_dir, remove_all, create_temp |
| file |  | 10 | open, read_into, write_all, close, read, read_string, write, remove, try_lock, unlock |
| **total** | **7 of 12** | **65** | |

**Known cost.** float's push_fixed64 and its push_ascii helper build their digits as Bytes
and pass them through `string.from_utf8`, revalidating about 20 ASCII bytes per number that
they wrote themselves. That revalidation stays in std (no core entry point) until it shows in a
profile.
