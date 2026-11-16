# Map

[API overview](../API.md). [Operations and contracts](../map.hirsel).

This module extends core's Map operations with membership and plain-value
helpers. The insertion-ordered representation has one capacity-sized storage
block and an index with twice the entry capacity. Growth doubles from four entries.
The per-run hash seed is not exposed. Iteration returns views and freezes the map.
`map.update` lends a present value after one lookup; `map.set` replaces the value
without allocating or moving its key. See also [Set](set.md).
