# Uuid

[API overview](../API.md). [Operations and contracts](../uuid.hirsel).

`std.uuid` handles RFC 9562 UUIDs. It parses and writes the text form and builds versions 4,
5 and 7. Versions 4 and 7 take random bytes from the caller, and version 7 also takes a
millisecond timestamp. The module never draws
randomness or reads a clock. The caller fills 16 or 10 bytes from `host.random` (unguessable)
or from a `std.rand` stream (repeatable, and therefore **predictable**), and passes a
millisecond time from `host.wall_ms` or `CallInfo.at`. An identifier that must not be guessed,
such as a bearer token, a capability URL or a session id, has to come from `host.random`. A
seeded stream gives the same identifiers to anyone who knows the seed. `push` appends 36 bytes
to a caller-owned String, and `to_string` returns a new String with the same lowercase
hyphenated text.

Version 5 takes a namespace UUID and canonical name bytes. It hashes the namespace in
network order followed by the name with SHA-1, then sets the RFC version and variant bits.
RFC 9562 defines `NAMESPACE_DNS`, `NAMESPACE_URL`, `NAMESPACE_OID` and `NAMESPACE_X500`.
The caller chooses the name encoding for the namespace. Names above 1 MiB return
`NameTooLong`.

## Types

```
pub type Uuid: key + ordered { hi: u64, lo: u64 }
pub type Variant: key = Ncs | Rfc | Microsoft | Future
pub type Error: key = Invalid(u64)
```

- `Uuid` holds the 128 bits in RFC 9562 network order. Byte 0 of the RFC layout is the top
  byte of `hi`, and byte 15 is the bottom byte of `lo`. Every 128-bit value is a UUID (Nil and
  Max included), so the record has no invariant and its fields are public.
- The derived order compares `hi` and then `lo`, which is RFC 9562 §6.11's byte order.
  Version 7 UUIDs from different milliseconds therefore sort by time.
- `Invalid(i)` gives the byte offset of the first input byte that does not fit the grammar.
  If all bytes in the prefix fit, the offset is the input length for a short input or 36
  for a complete UUID followed by extra bytes.

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

v1, v3, v6 and v8 constructors, MAC-address nodes, Nil/Max constants (`Uuid(hi: 0, lo: 0)`
is the literal), braced/URN/simple text, and monotonic counters.
