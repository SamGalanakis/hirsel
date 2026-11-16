# List

[API overview](../API.md). [Operations and contracts](../list.hirsel).

This module extends core's List operations; Bytes is List[u8], so the helpers
also apply to byte lists. A list owns a 24-byte header and a capacity-sized block
charged to the limit where it was made. Growth reserves max(needed, 4, 2 * capacity)
before refunding the old block. Removal does not shrink or allocate.
Returned element views anchor to and freeze the list. Bounds checks and Capacity
outcomes precede mutation as stated in each source contract.
