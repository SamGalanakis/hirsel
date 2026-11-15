# Ordered persistent map benchmark

`core/tools/omap_bench.py` builds this program and `core/twins/omap.rs` in
Cranelift development / LLVM optimized and Rust development / optimized modes.
Each program runs in a separate process. Every workload first inserts 100,000
ascending u64 keys, with value key + 1. The lookup mode adds 100,000 permuted
lookups, range adds a 50,000-row scan, and snapshots holds 1,000 versions while
overwriting 1,000 distinct rows. Checksums match in every implementation.

Pass `--compiler`, `--runtime` and `--out` to the script. `--prepare` builds the
programs; `--check-refusal` checks the actual resource-valued std.omap fixture;
`--measure` takes one instructions:u / target peak RSS sample per workload and
profile under tools/capped, CPU 16, both shared measurement locks, and 2 GiB.
Compilation and the counter process's RSS are excluded. Retain incomplete/OOM
rows as censored; a partial BTreeMap snapshot run is never a full ratio.

The Rust comparison uses std::collections::BTreeMap with clone-per-snapshot and
im::OrdMap 15.1.0. For im, each ordinary insertion holds the preceding root until
the update completes, matching Hirsel's persistent update contract. The
im-unique insert mode measures im's unique-owner fast path separately. Hirsel
always path-copies: its update borrows the input version, and public Arc has no
mutable-payload operation. try_unwrap consumes the Arc and requires a new node
allocation to publish the owned payload again.

The script compiles unmodified upstream registry sources directly with rustc,
without changing Cargo flags or budgets. `--registry` can name a vendored copy
of the following exact crates, preserving their original license files:

| Crate | Version | License |
|---|---|---|
| im | 15.1.0 | MPL-2.0-or-later |
| sized-chunks | 0.6.5 | MPL-2.0-or-later |
| bitmaps | 2.1.0 | MPL-2.0-or-later |
| typenum | 1.20.1 | MIT OR Apache-2.0 |
| rand_core | 0.6.4 | MIT OR Apache-2.0 |
| rand_xoshiro | 0.6.0 | MIT OR Apache-2.0 |

im's stable build.rs selects `threadsafe`, with no specialization or optional
features; bitmaps and sized-chunks enable their default std feature. These are
benchmark dependencies only, outside the shipped compiler/runtime link graph.
No third-party source is copied into the Hirsel library.
