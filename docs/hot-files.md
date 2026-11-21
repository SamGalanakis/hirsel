# Concurrent registrations

Add ordinary test modules to `modules.rs` beside the harness's `main.rs`.
Each line is exactly `mod name;`. The harness includes the declarations at its
crate root, so existing module paths, test names, guard setup and Buck2 test
shards stay the same. Keep attributes and `#[path]` support modules in `main.rs`;
they are not independent lines. Both Cargo and Buck2 compile the include, and
`tools/buck2/fmt.py` checks each registered module as a rustfmt root, without
requiring independent declarations to be reordered.

The `hirsel` merge driver in `tools/buck2/merging.py` handles these lists during
Git merges and rebases. Either side can remove a baseline declaration, and both
sides' new declarations survive. The driver writes declarations in the current
side's order, followed by new lines from the other side.

Bootstrap (`python3 tools/buck2/prepare.py`) configures the driver. To install
it before a merge or rebase without preparing build tools, run:

```sh
git config merge.hirsel.name 'files many lanes add to (tools/buck2/merging.py)'
git config merge.hirsel.driver 'python3 tools/buck2/merging.py %O %A %B %P'
```

Run `tools/check-hot-files` to check that each registry contains only unique
module declarations and that its harness includes it.

Keep benchmark measurements, raw sessions, profiles and reproduction scripts
in the lane's evidence directory outside the checkout. LangArena and the tasks
app default to external session directories. Keep only a short result row or archive link in
an existing results file; do not add per-lane result files to the public repo.
Never union-merge Markdown tables: adjacent rows can have different headings,
units, sessions and interpretation.

BUCK inventories and related build inputs are ignored. Regenerate them with
`python3 tools/buck2/sync.py --if-stale` after a rebase.

## Verification

`tools/tests/hot_files.py` checks concurrent additions and removals through
real Git merges and rebases in both orders. `tools/tests/merging.py` checks
every base, current and incoming row membership, plus line order and final
newlines.
