# Json

[API overview](../API.md). [Operations and contracts](../json.hirsel).

`std.json` is the matched json workload's library: the compact document
(`Json`, `Member`), its exact sizing pass (`size`), `parse` in one reservation, and the object
key index (`ObjectIndex`: `index`, `get`, `shown`). Walking and positional access use core's
`match`, `seq.len` and `indexed.get`. A key or a Str is read through `text.as_string`.

Use `decode` to read one complete value into an owned type. Compose
`record_of` for object members, `list_of` for arrays, and `read_f64` for
numbers. A record reader receives decoded keys in their input order and
chooses which fields to keep. It calls `skip_value` for unknown fields to
check their syntax without building a DOM.

`size` and `parse` allow up to 128 nested arrays and objects, counting the
root container as one. `skip_value` applies the same limit to the value it
skips. A 129th container returns `Error(Depth)`. The sizing pass checks the
limit before `parse` admits or writes the document. The threshold follows
[serde_json's default recursion limit](https://github.com/serde-rs/json/blob/f51fa15a7015c7d02817a46d2db44c899a0016e8/src/de.rs#L63).
