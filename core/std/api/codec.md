# Codecs

[API overview](../API.md).

`std.codec.Codec[T]` is SEM-10's codec: a plain record of an encoder, a decoder, a schema
writer and LEAST, the fewest bytes any T encodes to. `of[T]()` is the only way a program
names one. The checker resolves it per instance:

| T | `of[T]()` is |
|---|---|
| a scalar, `()`, String, DecodeError | the built-in `of_u64()` and the like |
| List[E], Option[E], Map[K, V], Result[T, E]; Bytes is List[u8] | `of_list[E]()`, `of_option[E]()`, `of_map[K, V]()`, `of_result[T, E]()` |
| a nominal type declared `codec` | the codec its derive generates (`via R` included) |
| a tuple, or a type holding one | the gap "the codec of this type is not built" |

In a generic body `of[T]()` resolves when T is substituted, so a codec bound needs no
dictionary. `of` at a type without the codec bound is LANG-12 at the call.

**Whole values.** `encode(c, x, mut out)` appends x's bytes; on Capacity, out holds a
prefix. `decode(c, b)` and `decode_at(c, b, start)` read exactly the bytes encode writes:
`Ok(Error(e))` for bytes no value encodes to, `Error(c)` for a refused admission, with
nothing admitted kept either way. `schema(c)` is the bytes a store keeps beside the data;
`fingerprint(c)` is its CRC-64-AVRO. `resolve(c, writer)` resolves an older writer's
schema against c (defaulted fields, tombstones and removed variants) into a `Reader[T]`,
and `read`/`read_at` decode under it. `key_make`, `encode_key` and `decode_key` are the
order-preserving key format for u8..i64, bool, Bytes, String and tuples of them.

**Format.** Integers at their width, a float as its IEEE bits and a char as a u32, all
little-endian; bool one byte; `()` nothing. String, Bytes, List and Map: a u32 count, then the
bytes, the elements, or key and value per entry in insertion order. Option, Result and a
sum: a u32 tag (None 0, Some 1; Ok 0, Error 1; a sum's declared index), then the payload.
A record or tuple: its fields in order. LEAST is 4 for String, Bytes, List and Map and 4 for
Option and Result. A value schema is version 03; a key schema is version 04. Definitions, fields,
variants, tuple arities, type arguments, definition indices and UTF-8 names use u32
counts or lengths, all four bytes little-endian. The root type uses opcodes 0-19;
`codec.hirsel` states it in full. Counts above u32::MAX return
`Capacity(requested: count, available: 4294967295)` before writing the count.
There is no reader for the earlier widths.

**Schema consumers.** `describe(c)` retains the root, nominal definitions, and
pre-encoded reader defaults. The `schema_root`, `schema_definition`,
`schema_definitions`, `schema_word`, `schema_next` and `schema_default`
operations walk that description. [Typed JSON](json.md#json-for-codec-types)
uses it to prepare its format and passes assembled bytes to `decode`.

**Structural codecs.** Each is `put_list`, `take_list` and `schema_list` (and the Map,
Option and Result twins) over `of()` of its arguments, which is the code a derived field
of that type runs. So `of[List[E]]` and a derived `List[E]` field have equal bytes, schema,
fingerprint and LEAST (law `structural_of`), and `type T: codec via List[E]` derives
(law `structural_via`). On `core/twins/codec` (100,000 rows `{u64, String, i64}`, LLVM
release, CPU 16), `of[List[Account]]` and a derived one-field record cost the same:
14.90M instructions an encode pass (Rust 5.80M, 2.57x, medians of five). The initial
u32 decode sample was 66.74M (Rust 55.74M, 1.20x); this encode-only fix leaves decode
unchanged. See
[the twin measurement](../../twins/codec/MEASURE.md) for the before/after results.

**Writing a codec by hand.** `make(e, d, w, n)` is what the derive calls; `put_X`/`take_X`,
`put_with`/`take_with`, `take_via`, and `define`, `record`, `sum`, `via`, `field`,
`removed_field`, `variant`, `removed_variant` and `close` for the schema are its protocol.
`need` checks that bytes remain, `take_count` bounds a count by the bytes left, `room`
reserves exactly n elements, and `full` turns an admission refusal into a `Stop`.
A hand codec is for twins and tests; a type gets its codec by declaring `codec`.
