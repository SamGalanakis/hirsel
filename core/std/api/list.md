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

`list.map` and `list.try_map` call their callbacks in list order and reserve
exactly `len(xs)` output slots first. A refused reservation raises `Capacity`
before the callback runs. `map` cannot collect views (OWN-19). `try_map` stops
at the first `Error` and returns it. A `?` within the callback exits only that
callback (LANG-18). Scoped callbacks cannot wait. Use a `for` loop for waiting
calls.

`list.retain` keeps matching elements in order without allocating. It cleans
removed elements after the last predicate call. A failing predicate leaves
every original element in the list exactly once, in unspecified order. The
predicate cannot capture the list (OWN-11).

`list.find` and `list.position` stop at the first match, returning a view or
an index respectively. Both return `None` if no element matches. The view
freezes the list (OWN-10). Use `position` to find an element for mutation
because Hirsel has no mutable views (OWN-8).

`list.any` stops at the first match and returns `false` for an empty list.
`list.all` stops at the first nonmatch and returns `true` for an empty list.
All seven helpers accept non-plain elements and pass them to callbacks by view.
