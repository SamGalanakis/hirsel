# Hirsel standard library: API contracts

Admit a module to `std` when it meets all three conditions. It is universal across real
Hirsel services and CLIs. It has one settled, familiar shape with either an RFC or a spec
behind it or an implementation under about 300 lines. It must get a Hirsel invariant right,
such as `bound bytes` and `Capacity`, `Secret` redaction, `CAP-1` capabilities, constant-time
cryptography, or R8a deterministic simulation with `Clock` and `Pcg`. As a tie-breaker, put
it in `std` if an agent would otherwise hand-roll 80 lines of subtle, bug-prone boilerplate.

Keep modules in separate libraries when they pick an opinionated or competing style, as web
frameworks, ORMs, auth stacks, rich CLI frameworks, and caching or DataLoader policies do.
The same applies to modules that track vendors or fast-moving protocols, such as database,
cloud, and LLM SDKs, or that are heavy or specific to one domain, such as media, HTML,
YAML, XML, and domain engines.

Each module is one source file in this directory:
its types, and each function's signature, contract and body. This document holds what the files
share: how to read contracts, name modules, and run their contract tests. Every signature and every implementation lives in exactly one place, its `.hirsel`
file. The [operation index](API-INDEX.md) is generated from those files by
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
(`arc`, `char`, `check`, `clock`, `cred`, `exec`, `float`, `int`, `list`, `map`, `net`, `order`,
`result`, `sha1`, `sha256`, `sha512`, `string`, `task` — see the `Extends` column in [API-INDEX.md](API-INDEX.md))
merges into core's name, so `list.sort` and
`string.find` are written bare everywhere. Core collections use bare names: `Deque[T]`, `deque.push_back`, `Heap[T]`,
`heap.pop`, `list.swap` and `list.sort_stable`. The stable sort's scratch is specified
in [MEM-24](../../docs/spec/budgets.md#mem-24-logical-charges). A comparator failure
preserves every element in unspecified order, and Capacity leaves the list unchanged.
The Deque ring uses uninhabited slots,
and the Heap uses hole sifts over one List. Their modules are in `core/core`, with no
std copies or re-exports. `string.make_ascii_upper` and `string.make_ascii_lower` are core too, written over the carrier without a second UTF-8 pass; std no longer defines them, and callers keep the same names. Modules that are std's alone (order, omap, set, pool, tally, dir,
file) are reached from another package through the package name: `std.order.compare`,
`std.set.Set[u64]`, `std.pool.Pool[T]`, `std.file.read(d, p)`, `std.file.Error`. Variants are
written bare (`Held(f)`, `NotUtf8`).

**Authority.** The language rules live in `docs/spec/*.md` and `core/core/*.hirsel`. The core
library is source in `core/core/`; `std` restates none of it. Everything here is library code
over core, and no rule is bent to make a signature legal.

**Prior art.** Names and shapes follow Rust std (with `indexmap` and `slotmap`), Go, Zig std,
and Goose's standard library, in that order of preference, differing only where a Hirsel rule
requires it.

## Files

A module with a core module's name **extends** it (LANG-7): `list.push` is core's and `list.sort`
the seed's, read through one name. No extension declares a core member's name.

The generated [operation index](API-INDEX.md) lists every module and operation.
Module contracts live in [api/](api/), one file per module named `<module>.md`.
[Process](api/process.md) documents the core stop wait.
Add or update a module contract there; regenerate the index and its links with `python3 core/tools/std_api_index.py --write`.

## Reading a contract

The module's `.hirsel` file holds each signature and its operation contract.
`Fails:` names the failure or returned error and which inputs survive it.
`Alloc:` states the reservation and the limit charged. `View:` names the owner
that anchors a returned view. `FREEZE` means that owner cannot be mutated while
the view lives. `Prefix:` describes host effects that remain after failure or
cancellation. `Cost:` gives the work bound; `Ex:` shows a call.

`GROWTH` follows the module's allocation rule. For list-backed collections it
reserves the replacement before refunding the old block; the module contract
states the capacity rule. Module-specific layouts and algorithms live in
`api/<module>.md`, reached through the generated operation index.
