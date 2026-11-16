# Json

[API overview](../API.md). [Operations and contracts](../json.hirsel).

`std.json` is the matched json workload's library: the compact document
(`Json`, `Member`), its exact sizing pass (`size`), `parse` in one reservation, and the object
key index (`ObjectIndex`: `index`, `get`, `shown`). Walking and positional access use core's
`match`, `seq.len` and `indexed.get`. A key or a Str is read through `text.as_string`.
