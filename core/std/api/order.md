# Order

[API overview](../API.md). [Operations and contracts](../order.hirsel).

Comparisons use the `ordered` bound and core's Less, Equal and Greater values.
`compare`, `min`, `max`, `reverse` and `then` work over ordered scalars and
compositions. Scalar comparison is constant work; sequence comparison visits the
common prefix until the first difference or the shorter sequence's end.
