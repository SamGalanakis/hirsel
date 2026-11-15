# Concurrent registrations

Add ordinary test modules to `modules.rs` beside the harness's `main.rs`.
Each line is exactly `mod name;`. The harness includes the declarations at its
crate root, so existing module paths, test names, guard setup and Buck2 test
shards stay the same. Keep attributes and `#[path]` support modules in `main.rs`;
they are not independent lines. Both Cargo and Buck2 compile the include, and
`tools/buck2/fmt.py` checks each registered module as a rustfmt root, without
requiring independent declarations to be reordered.

The `hirsel` merge driver in `tools/buck2/merging.py` combines both these lists
and `tools/known-failures.tsv` as whole-line sets. For base B and sides O/T the
result is `(B ∩ O ∩ T) ∪ (O − B) ∪ (T − B)`: a baseline row survives only if
both sides keep it, and either side's new rows survive. Thus deleting resolved
failure R while another lane adds S yields S. Git's built-in `merge=union`
would bring R back, silently hiding a fixed test. This driver protects ordinary
merges and rebases as well as landing, which is why it is preferable to a
push-only ledger check.

Bootstrap (`python3 tools/buck2/prepare.py`) and `tools/land` install the driver
in the clone's Git config. To configure it before a manual merge or rebase
without preparing build tools, run:

```sh
git config merge.hirsel.name 'files many lanes add to (tools/buck2/merging.py)'
git config merge.hirsel.driver 'python3 tools/buck2/merging.py %O %A %B %P'
```

Custom drivers require this local configuration; `.gitattributes` alone does
not install them. The two former union patterns both use `merge=hirsel`:
`crates/*/tests/*/modules.rs` and `tools/known-failures.tsv`. The set operation
reuses the removal rule from `sorted_stretch`
(`tools/buck2/merging.py` at `37dae771`, lines 193–194), over the entire file so Git's conflict-hunk boundaries cannot change its result.
It retains ours' line order followed by new lines from theirs, removes identical
duplicates and writes final newlines. Declaration and ledger row order has no
semantics. After each ticket rebase, `tools/land` commits one row per ledger ID: the latest date wins, with ties preferring the ticket's added row; unrelated rows keep their order.
Before that, it drops each ID deleted since the merge base by the ticket, or by main when the ticket left the ID's rows unchanged; a deletion beats a concurrent edit on the other side.
`tools/check-hot-files`, run by `tools/land` on every rebased tree including
push-only retries, rejects duplicate declarations and duplicate ledger IDs.
`tools/check-hot-files` also checks each ledger row's four fields and date. Do not add
comments, attributes or blank lines to the declaration lists.

Publish benchmark measurements, raw sessions, profiles and reproduction scripts
under `/workspace/code/hirsel-briefs/<lane>/`. LangArena and the tasks app default
to external session directories. Keep only a short result row or archive link in
an existing results file; do not add per-lane result files to the public repo.
Never union-merge Markdown tables: adjacent rows can have different headings,
units, sessions and interpretation.

BUCK inventories and related build inputs are already ignored and derived by
`tools/buck2/sync.py --if-stale`. Do not commit them. Landing can replay legacy
commits with conflicts in generated files: build inputs are untracked and
regenerated before checks. Source conflicts still abort the rebase. No lane commits a generated file.

## Verification

`tools/tests/hot_files.py` creates scratch branches and checks add/add,
delete/add and delete/delete for both independent-line patterns, through real
merges and rebases in both orders, with system and global Git configuration
disabled. `tools/tests/merging.py` exhausts all eight base/ours/theirs row
memberships and checks line order and final newlines. `tools/tests/land.py`
covers replaying generated-file conflicts, regenerating an ignored build
inventory, refusing source conflicts, moved-main retries and failed checks.
