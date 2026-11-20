# String

[API overview](../API.md). [Operations and contracts](../string.hirsel).

This module extends core's UTF-8 String operations. A String owns a Bytes
block; literals use static storage and copy on their first growth. Positions are
byte offsets, and equality, ordering and hashing use bytes without normalization.
Cutting helpers return anchored views without allocation and freeze their source.
Allocation and growth use the underlying list rules. Copying is explicit through
`string.from` or clone.

`string.find` returns the first matching byte offset at or after the given start.
It allocates nothing and takes O(remaining bytes + pattern bytes) in the worst case.
