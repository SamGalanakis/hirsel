# List

[API overview](../API.md). [Operations and contracts](../list.hirsel).

This module extends core's List operations; Bytes is List[u8], so the helpers
also apply to byte lists. A list owns a 24-byte header and a capacity-sized block
charged to the limit where it was made. Growth reserves max(needed, 4, 2 * capacity)
before refunding the old block. Removal does not shrink or allocate.
Returned element views anchor to and freeze the list. Bounds checks and Capacity
outcomes precede mutation as stated in each source contract.

`list.push` returns `Result[(), (Capacity, T)]`, giving the element back on a
refused growth. In a function returning `Result[_, Capacity]`, use
`result.capacity_only(list.push(mut xs, take x))?` to clean the returned element
and propagate only Capacity. Plain elements may be passed as `x` without `take`.

`list.find_byte` returns the first absolute byte index in a checked range.
`list.find_byte2`, `list.find_byte3` and `list.find_byte4` accept several byte values.
`list.starts_with` compares the list with a prefix of the same element type.
Both allocate nothing.
