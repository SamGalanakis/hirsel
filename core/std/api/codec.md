# Codecs

[API overview](../API.md).

`std.codec.Codec[T]` is Hirsel's binary codec: a plain record of an encoder, a decoder, a schema
writer (each a described function, `StaticFn`), a lower bound on the encoded length, and whether its encoding set is empty.
`revision(c)` and `key_revision(k)` return the three parts'
[code revisions](../../../docs/spec/programs.md#prog-9-code-descriptors), 96 bytes,
which change with any default literal or `via` body decoding reads. `of[T]()` is the only way a program
names one. The checker resolves it per instance:

| T | `of[T]()` is |
|---|---|
| a scalar, `()`, Never, String | the built-in `of_u64()` and the like |
| List[E], Option[E], Map[K, V], Result[T, E]; Bytes is List[u8] | `of_list[E]()`, `of_option[E]()`, `of_map[K, V]()`, `of_result[T, E]()` |
| a nominal type declared `codec` | the codec its derive generates (`via R` included) |
| a tuple of codec components | a derived codec that reads and writes each component in order |

In a generic body `of[T]()` resolves when T is substituted, so a codec bound needs no
dictionary. Calling `of` at a type without the `codec` bound is refused at the call site.

**Whole values.** `encode(c, x, mut out) -> Result[(), EncodeError]` appends x's
bytes. A refused allocation raises Capacity and leaves the completed prefix in
out. `decode` and `decode_at` return `Result[T, DecodeError]`. A decode error or an
allocation refusal releases all temporary storage. `schema(c)` returns the
bytes a store keeps beside the data, and `fingerprint(c)` returns its
CRC-64-AVRO. Both return `EncodeError` for an unrepresentable count. `fingerprint_of(b)` folds the bytes through
Avro's 256-entry Rabin table and allocates nothing. `resolve(c, writer)` resolves
an older writer's schema against c (defaulted fields, tombstones and removed variants) into a `Reader[T]`,
and `read` and `read_at` decode under it. Resolution and both reads return
`DecodeError` for data or schema errors. `key_make`, `encode_key` and `decode_key`
are the hand-written key codec protocol. `encode_key` returns unit and `decode_key` returns
`Result[T, DecodeError]`.

**Ordered keys (`key_of`).** `key_of[K]()` is how a program names an
ordered key codec, and the checker resolves it per instance, as it does `of`. Its
admitted set is its own; it is not the `key` (equality) bound, which `List`, `Option`
and `Arc` also satisfy:

| K | `key_of[K]()` |
|---|---|
| u8..u64, i8..i64 | big-endian at the width; signed with the sign bit flipped |
| bool | one byte, 0 or 1 |
| String, Bytes | the bytes, each 0x00 written 0x00 0xff, then 0x00 0x00 |
| `()` | nothing (so `Table[(), V]` has one row) |
| a tuple of admitted types | the components' bytes concatenated |
| a nominal record declared `key` whose fields are admitted | its fields in declaration order, as a tuple |
| `Desc[T]`, T admitted | T's key bytes with every byte inverted (`b ^ 0xff`) |

Any other K is refused at the call (`key_of` requires a type in the admitted set). A record with
a defaulted or removed field is refused at the call: a key has no reader, so any edit
to a key type is a `migrate_table` rewrite. The format is prefix-free (no key's bytes
are a proper prefix of another's), so bytewise order is value order, a tuple's order is
lexicographic, and inversion reverses the order. `Desc[T]` is `ordered` in reverse, so
the program's `<` agrees with the store's order (law `key_order`: for random values of
each admitted shape, `a < b` iff `encode(a) < encode(b)` bytewise, with the language's
`ordered` comparison as the oracle). `Desc(value: x)` wraps and `.value` unwraps. In
a value codec, `Desc[T]` is written as T.

`key_prefix.matches(full, leading)` checks ordered key codecs for prefix reads. It compares
complete leading components through tuple and record nesting, which adds no bytes.
A `Desc` component matches as a whole because it inverts its payload's bytes.

**Format.** Integers at their width, a float as its IEEE bits and a char as a u32, all
little-endian; bool one byte; `()` nothing. String, Bytes, List and Map: a u32 count, then the
bytes, the elements, or key and value per entry in insertion order. Option, Result and a
sum: a u32 tag (None 0, Some 1; Ok 0, Error 1; a sum's declared index), then the payload.
A record or tuple: its fields in order. LEAST is 4 for String, Bytes, List and Map and 4 for
Option and Result. A value schema is version 03; a key schema is version 04. Definitions, fields,
variants, tuple arities, type arguments, definition indices and UTF-8 names use u32
counts or lengths, all four bytes little-endian. The root type uses opcodes 0-21.
Opcode 20 is Never, which has no valid encoding. Opcode 21, in key schemas only, is
`Desc` followed by its component type (a key schema without `Desc` has the same
bytes and fingerprint as before, so the version stays 04). A tag whose payload requires
a Never fails with Invalid at the start of the tag.
The full format is in `codec.hirsel`. Counts above u32::MAX return
`Error(CountOverflow(count))` before writing the count.
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

**Writing a codec by hand.** `make(code.describe(e), code.describe(d), code.describe(w), n)` supplies the parts of a hand-written codec, and `key_make(code.describe(e), code.describe(d), code.describe(w))` those of a key codec. `put_X`/`take_X`,
`put_with`/`take_with`, `take_via`, and `define`, `record`, `sum`, `via`, `field`,
`removed_field`, `variant`, `removed_variant` and `close` for the schema are its protocol.
`need` checks that bytes remain, `take_count` bounds a count by the bytes left, `room`
returns an empty list with room for exactly n elements. Fixed-width scalar
writers return unit. The encoder and schema-writer callbacks return
`Result[(), EncodeError]`, and the decoder callback returns
`Result[T, DecodeError]`.
A hand codec is for twins and tests; a type gets its codec by declaring `codec`.
