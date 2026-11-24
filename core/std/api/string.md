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

`string.push_upper`, `string.push_lower`, `string.to_upper` and `string.to_lower` apply full Unicode 18.0.0 case mappings to each scalar.

`string.case_fold` applies Unicode 18.0.0 default full case folding to each scalar.
`string.eq_ignore_case` compares the resulting UTF-8 bytes. For example, `"Straße"`
and `"STRASSE"` compare equal, while `"I"` and `"ı"` compare unequal.
[Unicode text specification](../../../docs/spec/unicode.md).

`string.join(parts, sep)` writes the strings in order with `sep` between
adjacent parts. An empty separator concatenates the parts. An empty list
produces an empty string.

`string.replace(s, pat, replacement)` replaces every leftmost non-overlapping
match of `pat`. An empty pattern inserts `replacement` at every UTF-8 scalar
boundary, including the start and end. Both operations return an owned string
and can raise Capacity.
