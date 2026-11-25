# Ids

[API overview](../API.md). [Operations and contracts](../ids.hirsel).

Ids is a u32 index allocator. It owns only a LIFO free list, the next fresh
index, and one liveness bit per issued index; element storage stays in the
caller's List, so callers keep direct places such as `nodes[i].left`. `take_index`
(the operation is not named `take`, which LANG-2 reserves) returns a recycled
index before a fresh one, and `give` returns a live index to the free list.
Giving an index that is not live fails, which catches a double free.

`place` and `try_place` combine index allocation with row placement: a fresh
index pushes its row and a recycled index overwrites its row. The row storage is
the dense List for that allocator, so its length is `limit` before the call. A
refused growth is prepared before either side changes; in particular a refused
`try_place` leaves `limit` and the List length unchanged. Index 2^32 - 1 is
never issued and remains available as a caller sentinel.
