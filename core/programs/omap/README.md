# Ordered persistent map benchmark

`core/tools/omap_bench.py` builds this program and `core/twins/omap.rs` in
Cranelift development / LLVM optimized and Rust development / optimized modes.
Each workload runs in a separate process. `insert` measures 100,000 ascending
u64 inserts from empty. `lookup`, `range` and `snapshots` populate 100,000 rows
before the measured region. The operations are 100,000 permuted lookups, a
50,000-row scan, and 1,000 held snapshots with overwrites respectively.
`recover` measures the consuming update that returns the original map on
Capacity, currently a shared-path copy even with unique ownership.
`unique` uses the discard-on-Capacity consuming std.omap update and im's unique-owner fast path.
`bulk` builds from sorted input prepared before the measured region. im's
FromIterator uses repeated unique inserts; BTreeMap uses its sorted collector.
`unwrap` is a separate wrapper-cost control with 32 child Arcs.

Pass `--compiler`, `--runtime` and `--out` to `core/tools/omap_bench.py`.
`--prepare` builds the programs; `--check-refusal` checks the resource fixture;
`--measure` takes one sample, or `--samples 5` takes the requested five
independent samples for a median; `--profile` records sampled instruction IPs.
`--only-workload`, `--only-profile` and `--only-implementation` select cases.
`--reuse-rust DIR` reuses exact dependency rlibs, rebuilding only the twin.
Evidence belongs outside the repository. Each measurement tag directory must
be new; the runner never overwrites a sample. Retain censored rows.

The target opens a FIFO in its explicit Dir grant. It prints READY after
setup and waits for the harness. After READY it waits two capped CPU-quota periods outside the measured region
so setup cannot spend the operation's initial quota. It resets Linux VmHWM through
/proc/PID/clear_refs, enables perf with an acknowledged control command,
and releases the target. DONE reports the checked checksum and the target's
own monotonic operation duration. A second gate keeps maps and snapshots alive
until perf is disabled and target VmHWM is read. Setup and final destruction
are excluded from instructions and wall; peak RSS includes the resident map
at the start plus any peak during the operation. No peak is subtracted.
All samples use CPU 16, both measurement locks, tools/capped and 2 GiB.
An OOM before DONE is censored even if an outer wrapper exits zero.

The main Rust comparison uses BTreeMap clone-per-snapshot and im::OrdMap
15.1.0. In `insert`, im holds the preceding root until the update completes,
matching Hirsel's borrowed update; `unique` holds no artificial prior root.
The scalar control compares 100,000 unwrap/new edits with Rust Arc::try_unwrap
plus Arc::new, and with Arc::get_mut. These are controls, not map inserts.
Hirsel's consuming update frees and allocates the Arc wrapper at each level;
core Arc has no mutable-payload operation. The ordinary borrowed APIs and all
store-facing node/cursor signatures remain unchanged.

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

`overwrite` builds 100,000 entries outside the measured region, then replaces
each value through the owned path and drops the returned old-value holder.
It measures the success-result overhead against the old Map-only signature.
