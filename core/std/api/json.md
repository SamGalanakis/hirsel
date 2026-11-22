# Json

[API overview](../API.md). [Operations and contracts](../json.hirsel).

`std.json` is the matched json workload's library: the compact document
(`Json`, `Member`), its exact sizing pass (`size`), `parse` in one reservation, and the object
key index (`ObjectIndex`: `index`, `get`, `shown`). Walking and positional access use core's
`match`, `seq.len` and `indexed.get`. A key or a Str is read through `text.as_string`.

`has_duplicates` checks the index for repeated decoded keys. `as_i64` and
`as_u64` read canonical decimal integer nodes in range. `as_f64` reads a
numeric node as a finite binary64. `pointer` resolves an RFC 6901 JSON Pointer
to a view of the selected node. The empty pointer selects the root. Invalid
pointers, missing paths and ambiguous duplicate keys return None.

## JSON for codec types

Use `json.typed()` with a result type of `Result[json.Format[T], json.TypedError]`
to prepare a format once. T must have a codec. Preparation refuses tuples,
options whose element already admits `null`, lists with zero-byte elements,
maps with zero-byte entries, and schema cycles that can recur without consuming
a codec byte. A schema refusal includes the relevant codec opcode.

`decode_typed(format, take input, byte_limit)` reads one UTF-8 JSON value.
The byte bound covers decoding scratch and allocations made by T's decoder.
Record members may arrive in any order. Absent fields use their declared
defaults, or None for optional fields. The decoder refuses duplicate and
unknown members. The codec's decoder still enforces `via` validation and
duplicate map keys. An input refusal gives a byte offset and an RFC 6901
JSON Pointer. The empty path denotes the root. `Capacity` reports refusal to
enter the byte bound. A refused allocation inside the bound raises Capacity.
`LengthOverflow` reports a count that the codec cannot represent.

`from_string(format, input, byte_limit)` applies the same bound to a borrowed
String.

`encode_typed` writes T's codec bytes into reusable caller-owned scratch.
It measures the JSON, calls the supplied length callback, then streams the JSON
bytes to the sink. For a value already encoded by the codec, use `typed_length`
and `write_typed`. Both passes use a fixed 64-entry traversal stack and allocate
no heap storage. A sink refusal can leave a prefix. The length pass refuses
nesting over 64 before any output is sent.

`push_typed(format, value, mut out, mut scratch)` measures the value and
reserves room before appending JSON to Bytes. It returns the appended byte
count. A refused measurement or reservation leaves the output bytes intact.
`to_string(format, value)` returns a new String containing the same JSON.

The `typed()` format follows [HTTP B-4 and B-5](../../../docs/spec/http.md#b-4-lexical-forms-json-path-and-query).
In particular, 64-bit integers are strings, Bytes use padded base64, and maps
are arrays of pairs in insertion order. Records are written in declaration
order. The writer uses `\u00XX` for control characters and shortest round-trip
decimal digits at the original float width. It preserves negative zero.
Named nonfinite values are JSON strings.

Use `json.interop()` to prepare the external-API profile for T. It uses the
same 64-container limit, including containers in ignored record members.
Records skip unknown members and reject duplicate names, including escaped
spellings and duplicate names inside skipped values. Result member names and
sum variant names remain strict. Records omit fields whose value is None.

`i64` and `u64` use bare exact decimals. `Map[String, V]` uses a JSON object,
preserves insertion order on write and wire order on read, and rejects
duplicate names. Floats must be finite. Measurement returns
`TypedWriteError.NonFinite` before emitting any output.

## Documents, callback readers and writers

Use `decode` to read one complete value into an owned type. Compose
`record_of` for object members and `list_of` for arrays. `peek` reports the
next `ValueKind` without advancing the cursor. `read_bool` and `read_string`
read booleans and decoded strings. `read_i64` and `read_u64` require canonical
decimal integers in range. These readers report `WrongKind` at a different
value kind, and the integer readers report `IntegerRange` on overflow.
`read_f64` reads finite floating-point numbers. `read_option` consumes null as
None and calls the supplied reader for other values. `read_raw` consumes one
value and returns its exact spelling as a view of the input, excluding
surrounding whitespace.

A record reader receives decoded keys in their input order and
chooses which fields to keep. It calls `skip_value` for unknown fields to
check their syntax without building a DOM.

`size` and `parse` allow up to 128 nested arrays and objects, counting the
root container as one. `skip_value` applies the same limit to the value it
skips. A 129th container returns `Error(Depth)`. The sizing pass checks the
limit before `parse` admits or writes the document. The threshold follows
[serde_json's default recursion limit](https://github.com/serde-rs/json/blob/f51fa15a7015c7d02817a46d2db44c899a0016e8/src/de.rs#L63).

Use `write` to serialize one value directly into a caller-owned `String`.
The scoped callback writes scalars or opens an object or array. Call
`write_member` before each object value and `element` before each array
value. Pretty mode indents two spaces for each level.

`write_u64` appends an unsigned decimal. `write_raw` checks that its input
contains one complete JSON value under the same checks as `size`, then
appends it verbatim. A validation error leaves the output intact.

Each completed append stays in the output if a later append refuses
Capacity. A quoted string appends its opening quote, each unescaped run
or escape, and its closing quote as separate requests. An object member
writes its key this way before appending the colon.
