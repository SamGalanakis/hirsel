# List

[API overview](../API.md). [Operations and contracts](../list.hirsel).

This module extends core's List operations; Bytes is List[u8], so the helpers
also apply to byte lists. A list owns a 24-byte header and a capacity-sized block
charged to the limit where it was made. Growth reserves max(needed, 4, 2 * capacity)
before refunding the old block. Removal does not shrink or allocate.
Returned element views anchor to and freeze the list. Bounds checks and refused
growth precede mutation as stated in each source contract.

A refused growth raises `Capacity` (MEM-21); the element passed to `list.push`
is cleaned with the unwound frame. `list.try_reserve` returns the refusal as a
value for inline recovery. Plain elements may be passed as `x` without `take`.

`list.find_byte` returns the first absolute byte index in a checked range.
`list.find_byte2`, `list.find_byte3` and `list.find_byte4` accept several byte values.
`list.starts_with` compares the list with a prefix of the same element type.
Both allocate nothing.

`list.sort` sorts in place without preserving the order of equal keys. It runs
in O(len) on nondecreasing or strictly descending input and O(len log len) in
the worst case. A failing comparator or one that does not give a total order
leaves every original element in the list.
