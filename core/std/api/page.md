# page

The page and blob payload layouts, validation rules, private edits, sealing
and sorted bulk build are specified in the
[store page layout specification](../../programs/store/STORE-SPEC.md#112-pages).

`diff_leaves` compares resident leaves of a row tree starting at two slot
positions. It skips rows with equal keys and value bytes without allocating.
It returns the first differing slots and their key order, or no order when
one leaf is exhausted.
