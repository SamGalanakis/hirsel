# Uuid

[API overview](../API.md). Specified ahead of its module: the build lane moves each operation
block below into `core/std/uuid.hirsel` as that function's `///` contract and cuts this page
down to the module prose.

`std.uuid` handles RFC 9562 UUIDs. It parses and writes the text form, and builds versions 4
and 7 from bytes and a time the caller supplies. The module has no capability: it never draws
randomness or reads a clock. The caller fills 16 or 10 bytes from `host.random` (unguessable)
or from a `std.rand` stream (repeatable, and therefore **predictable**), and passes a
millisecond time from `host.wall_ms` or `CallInfo.at`. An identifier that must not be guessed,
such as a bearer token, a capability URL or a session id, has to come from `host.random`. A
seeded stream gives the same identifiers to anyone who knows the seed. Operations allocate
nothing except `push`, which appends 36 bytes to a caller-owned String.

## Types

```
pub type Uuid: key { hi: u64, lo: u64 }
pub type Variant: key = Ncs | Rfc | Microsoft | Future
pub type Error: key = Invalid(u64)
```

- `Uuid` holds the 128 bits in RFC 9562 network order. Byte 0 of the RFC layout is the top
  byte of `hi`, and byte 15 is the bottom byte of `lo`. Every 128-bit value is a UUID (Nil and
  Max included), so the record has no invariant and its fields are public.
- The derived `key` order compares `hi` and then `lo`, which is RFC 9562 §6.11's byte order.
  Version 7 UUIDs from different milliseconds therefore sort by time.
- `Invalid(i)` gives the byte offset of the first input byte that does not fit the grammar.
  For input of the wrong length, it is the offset at which the input stops fitting: the
  length itself when the input is too short, or 36 when it is too long.

## Operations

`parse(s: String) -> Result[Uuid, Error]`
: Accepts exactly the RFC 9562 §4 form `8-4-4-4-12`: 32 hex digits in either case, with
  hyphens at offsets 8, 13, 18 and 23. Fails: `Invalid(i)` at the first wrong byte (a non-hex
  digit, a misplaced or missing hyphen, or a wrong length as above). Alloc: nothing. Cost: O(36).
  Ex: `let id = std.uuid.parse(text)?`

`push(mut out: String, u: Uuid) -> Result[(), Capacity]`
: Appends the lowercase `8-4-4-4-12` text (RFC 9562 §4: output is lowercase). Fails:
  `Error(c)`. Reserves 36 bytes first, so on failure `out` is unchanged. Alloc: GROWTH of
  `out`. Cost: O(36). Ex: `std.uuid.push(mut line, id)?`

`from_bytes(b: Bytes, at: u64) -> Uuid`
: The UUID whose 16 bytes are `b[at .. at+16]`, in network order. Fails: Bounds (failure)
  when `at + 16` exceeds `list.len(b)`, as `list.slice`. Alloc: nothing.

`push_bytes(mut out: Bytes, u: Uuid) -> Result[(), Capacity]`
: Appends the 16 bytes in network order. Reserves first, so a failure leaves `out`
  unchanged. Alloc: GROWTH of `out`.

`version(u: Uuid) -> u8`
: The top four bits of byte 6, for any variant.

`variant(u: Uuid) -> Variant`
: The top bits of byte 8: `0xxx` Ncs, `10xx` Rfc, `110x` Microsoft, `111x` Future.

`v4(b: Bytes, at: u64) -> Uuid`
: RFC 9562 §5.4. Takes the 16 bytes at `at`, sets the version field (byte 6 high nibble) to
  4 and the variant field (byte 8 top two bits) to `10`. The other 122 bits come from `b`
  unchanged. Fails: Bounds as `from_bytes`.
  Ex: `let mut b: Bytes = list.with_capacity(16)?; host.random(r, mut b); let id = std.uuid.v4(b, 0)`

`v7(unix_ms: u64, b: Bytes, at: u64) -> Option[Uuid]`
: RFC 9562 §5.7. `unix_ms` fills the top 48 bits (bytes 0-5). The 10 bytes at `at` fill
  bytes 6-15. The version nibble (7) replaces the top four bits of byte 6, and the variant
  `10` replaces the top two bits of byte 8. So 74 of the 80 supplied bits are kept, which
  is uuid-rs `Builder::from_unix_timestamp_millis`'s layout. `None` when `unix_ms >= 2^48`
  (beyond year 10889). Fails: Bounds when `at + 10` exceeds `list.len(b)`.
  Ex: `std.uuid.v7(host.wall_ms(c), b, 0)`

`unix_ms(u: Uuid) -> Option[u64]`
: The top 48 bits when `version(u) == 7` and the variant is Rfc. Otherwise `None`.

## Decisions

- **One accepted text form.** Go `uuid.Parse` (go 46b62495 `src/uuid/uuid.go:44-60,127-160`)
  and uuid-rs `Uuid::parse_str` (uuid 1.24.1 `src/parser.rs`) also accept braces,
  `urn:uuid:` and 32 bare hex digits. These are NOT TAKEN. A service boundary that accepts
  four spellings of one value has four cache keys for it, and RFC 9562 §4 defines the
  hyphenated form as the text format. A caller that must accept URNs can strip the prefix
  with `string.strip_prefix` first.
- **Bytes and time come from the caller.** Go `NewV4`/`NewV7` read `crypto/rand` and the
  wall clock. Go also keeps a process-global, mutex-guarded "last timestamp" so that v7
  values stay ordered within a millisecond (`uuid.go:202-258`). That design is REPLACED by
  the explicit-input shape of uuid-rs `Builder::from_random_bytes` and
  `from_unix_timestamp_millis` (`src/builder.rs:534,591`). Hirsel has no ambient clock or
  entropy, and hidden global state would break replay.
- **Version 7 is not ordered within one millisecond.** Two v7 UUIDs from the same
  millisecond compare by their random bits. RFC 9562 §6.2 makes counters optional. The
  module keeps no state between calls, so it implements none. A caller that needs strict
  order within a store uses the store's own ids, or increments a counter of its own in the
  bytes it passes.
- **Goose** has no UUID module. The design follows Goose's convention that the caller owns
  the destination (`docs/design/stdlib_design.md` §1 "Make costs explicit") (ADOPTED).

## Left out

v1, v3, v5, v6 and v8 constructors, MAC-address nodes, Nil/Max constants (`Uuid(hi: 0, lo: 0)`
is the literal), braced/URN/simple text, and monotonic counters.

## Laws (planned for the build lane, `core/std/tests/uuid.hirsel`)

Each law names the bug it catches and its oracle. Copy expected values from the cited source
text, never from this module's output.

- **U1 RFC vectors.** RFC 9562 Appendix A.3 (v4) and A.6 (v7): `parse` of each text gives the
  bytes in the appendix's field table. `push` gives the text back in lowercase. `v7` built from
  the A.6 timestamp and its rand bits reproduces the text. Oracle: the RFC text. Bug caught:
  byte order swapped within `hi` and `lo` (little-endian packing), or the version written
  into byte 7.
- **U2 Version and variant bits.** `v4` of 16 `0xff` bytes is
  `ffffffff-ffff-4fff-bfff-ffffffffffff`, and of 16 zero bytes is
  `00000000-0000-4000-8000-000000000000`. `v7(0, ten 0xff bytes)` is
  `00000000-0000-7fff-bfff-ffffffffffff`. Oracle: hand-derived from RFC 9562 §5.4/§5.7 and
  cross-checked with Python `uuid.UUID(bytes=..)`, which sets the version and variant the
  same way. Bug caught: the variant mask clears three bits instead of two, or the version
  ORs into a byte without clearing the old nibble first.
- **U3 Parse rejection offsets.** Go `TestParseErrors` (uuid_test.go:175) inputs that this
  contract also refuses, plus the forms it alone refuses: braces at 0, `urn:` at 0, 32 bare
  digits at 8 (the first missing hyphen), `g` at its offset, a 35-byte input at 35, and a
  37-byte input at 36. Oracle: hand-derived from the grammar. Bug caught: accepting a hyphen
  in place of a hex digit, or reading past the end on short input.
- **U4 Order.** Sorting v7 UUIDs from times 1, 2 and 2^47 with random bytes chosen against
  the time order gives time order. Oracle: RFC 9562 §6.11. Bug caught: the derived key
  compares `lo` before `hi`.
- **U5 unix_ms round trip.** `unix_ms(v7(t, b, 0)) == Some(t)` for t = 0, 1645557742000
  (A.6) and 2^48-1. `v7(2^48, ..)` is `None`. `unix_ms` of a v4 is `None`. Oracle: the
  invariant. Bug caught: a 47-bit mask or an off-by-one at 2^48.

Prior vectors: `771885fb^:stdlib/std@1/uuid.hir` and
`c5690b37^:conformance/SEM/SEM-3-uuid-{layout,random,rfc-extras}-run.hir`. Where the old
contract differs (it accepted four spellings), keep only the rows this page also accepts. The
other rows become U3 rejection rows.
