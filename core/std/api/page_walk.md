# page_walk

A sorted walk over a resident [page](page.md) tree that can take a complete
remaining subtree at once. `begin` starts at the smallest key and `seek` at an
inclusive lower bound, keeping whole subtrees after the seek path. While the
walk stands on a whole subtree, `subtree`, `subtree_high` and `subtree_charge`
describe it, `expand` descends into it and `next` passes over it; on a row,
`key`, `value`, `blob_stamp` and `owned_value` read it and `next` advances.
`collection` reads the child a collection-map row names. Every page the walk
reaches must be resident: a Cold child fails.

`diff_position` locates the next differing row of two walks before an
optional exclusive upper bound. It skips whole subtrees with matching stamps
and skips equal rows within leaves. Collection-map rows still require the
caller's descriptor comparison.
