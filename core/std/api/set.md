# Set

[API overview](../API.md). [Operations and contracts](../set.hirsel).

Set[K] owns a Map[K, ()] and preserves insertion order. Storage charges are
the map's with a zero-sized value. Iteration uses the map view returned by
`entries`; Set itself is not a built-in loop source. Membership, insertion and
removal retain the map's key, growth and failure rules. See [Map](map.md).
