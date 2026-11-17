# Uri

[API overview](../API.md). Specified ahead of its module: the build lane moves each operation
block below into `core/std/uri.hirsel` as that function's `///` contract and cuts this page
down to the module prose.

`std.uri` parses, writes and resolves RFC 3986 URI references, and percent-encodes and
decodes their parts. Parsing copies nothing. A `Uri` is a set of spans into the caller's text,
using the same `Span` type as `std.http1`, and each component stays raw (still
percent-encoded) until the caller decodes the one it needs. The grammar is RFC 3986's, held
strictly. A byte the ABNF does not allow is an error at its offset, never a guess, and an
absent component is distinct from an empty one. Query strings follow HTTP B-4
([http.md](../../../docs/spec/http.md#b-4-lexical-forms-json-path-and-query), "Query strings"):
percent-decoding only, with `+` a literal plus. Operations allocate nothing except the
appenders, which grow a caller-owned `String` or `Bytes`.

## Types

```
pub type Host: key = Name(http1.Span) | Ip6(http1.Span)
pub type Uri: key {
    scheme: Option[http1.Span],
    userinfo: Option[http1.Span],
    host: Option[Host],
    port: Option[http1.Span],
    path: http1.Span,
    query: Option[http1.Span],
    fragment: Option[http1.Span],
}
pub type Set: key = Segment | Path | Query | QueryPart | UserInfo
pub type Error: key = Invalid(u64) | NotUtf8(u64) | Relative | Capacity(Capacity)
```

- Spans exclude the delimiters: `scheme` stops before `:`, `query` starts after `?`, and
  `fragment` starts after `#`. `Ip6` spans the address inside `[` and `]`.
- `host` is `Some` exactly when the reference has an authority (`//`). `file:///x` has
  `host = Some(Name(empty))`. A reg-name host is returned raw. It may be an IPv4 literal, a
  DNS name or percent-encoded text. Deciding which, and refusing ambiguous numeric hosts, is
  the transport's job, as it is today for network grants.
- `port` is the raw digits, possibly empty, as RFC 3986 §3.2.3 allows. `port_number` reads
  it.
- `path` is always present and may be empty.
- `Invalid(i)` is the byte offset of the first byte the grammar refuses. `NotUtf8(i)` is the
  offset, in the text, of the escape or byte where decoded text stops being UTF-8.
  `Relative` means a base for `resolve` has no scheme.

## Grammar

RFC 3986 §3 and §4.1 `URI-reference`, ASCII only:

- `scheme` is `ALPHA *( ALPHA / DIGIT / "+" / "-" / "." )`. It is recognized only before the
  first `/`, `?` or `#`, so `a/b:c` is a relative path.
- Each component accepts exactly its ABNF set (`pchar`, `query`, `fragment`, `userinfo`,
  `reg-name`). Any other byte is `Invalid` at its offset. That includes a space, `"`, `<`,
  `>`, `\`, `^`, `` ` ``, `{`, `|`, `}`, control bytes and every non-ASCII byte. A `%` must
  start exactly `%HH` with two hex digits, in either case.
- `userinfo` ends at the first `@` of the authority. A second `@` is `Invalid`.
- `IP-literal` is `[` IPv6address `]` in all nine RFC 3986 §3.2.2 forms, including an
  embedded dotted IPv4 tail. IPvFuture (`[v1.x]`) and RFC 6874 zone ids (`%25`) are
  `Invalid`.
- `port` is `*DIGIT`, and its value must be at most 65535 (`Invalid` at its first digit
  otherwise). Leading zeros are kept in the span.
- A relative reference whose first path segment holds a `:` is `Invalid` at that `:`
  (RFC 3986 §4.2), because it would read back as a scheme.

## Operations

`parse(text: String) -> Result[Uri, Error]`
: Parses `text` as a `URI-reference`, which may be absolute or relative. Fails: `Invalid(i)`.
  Alloc: nothing. Cost: O(len). Ex: `let u = std.uri.parse(s)?`

`push(mut out: String, text: String, u: Uri) -> Result[(), Capacity]`
: Recomposes `u` per RFC 3986 §5.3, copying each component's raw bytes from `text` with its
  delimiters. For every `u` that `parse(text)` returned, this appends exactly `text`.
  Atomic: it reserves the exact length first. Alloc: GROWTH of `out`.

`port_number(text: String, u: Uri) -> Option[u16]`
: The port's value, or `None` when the port is absent or empty.

`resolve(base_text: String, base: Uri, ref_text: String, r: Uri, mut out: String) -> Result[Uri, Error]`
: RFC 3986 §5.2 strict resolution of `r` against `base`, with §5.2.4 dot-segment removal.
  It appends the target to `out` and returns the target's `Uri`, whose spans index `out`.
  A reference with a scheme keeps its own components, but its path still has its dot
  segments removed (§5.2.2), so `http://a/b/../c` resolves to `http://a/c`, and `http:g`
  against an `http` base stays `http:g` (strict). Fails: `Relative` when `base` has no
  scheme, even when `r` has one: a base without a scheme is a caller error, whatever the
  reference. Also `Capacity(c)`. Reserves
  `len(base_text) + len(ref_text) + 1` first, which bounds every target, so on failure `out`
  is unchanged. Alloc: GROWTH of `out`. Cost: O(len base + len ref).
  Ex: `let t = std.uri.resolve(b, bu, s, su, mut out)?`

`decode(text: String, s: http1.Span, mut out: Bytes) -> Result[(), Error]`
: Appends the bytes of `text[s]` with each `%HH` replaced by its byte. Every other byte is
  copied, `+` included. Fails: `Invalid(i)` for a `%` that is not followed by two hex
  digits, or `Capacity(c)`. Atomic. Alloc: GROWTH of `out`.

`decode_text(text: String, s: http1.Span, mut out: String) -> Result[(), Error]`
: `decode` into a String. The result must be UTF-8, or it fails `NotUtf8(i)` at the offset
  in `text` of the escape that begins the bad sequence. Atomic. This is the decoder for
  path and query values (B-4: strings decoded from a path or query are UTF-8).

`encode(mut out: String, raw: Bytes, set: Set) -> Result[(), Capacity]`
: Appends `raw` with every byte outside `set` written as `%HH` in upper case (RFC 3986 §2.1).
  `ALPHA`, `DIGIT`, `-`, `.`, `_` and `~` are never escaped, and `%` always is. The sets
  allow, beyond those:
  - `Segment`: `sub-delims`, `:` and `@` (one path segment: `pchar`).
  - `Path`: `Segment` and `/`.
  - `Query`: `Path` and `?`. This is RFC 3986's `query` and `fragment` set, so a fragment
    uses it too.
  - `QueryPart`: `Query` without `&`, `=` and `+`, for one key or value of a
    `key=value&..` query. `+` is escaped so that a form-decoding reader does not read it as a
    space.
  - `UserInfo`: `sub-delims` and `:`.
  Atomic: it counts first and reserves the exact length. `decode(encode(b)) == b` for every
  byte string and every set.

`next_pair(text: String, query: http1.Span, at: u64) -> Option[(http1.Span, http1.Span, u64)]`
: Walks a `key=value&..` query from `at` (start with `query.start`). It returns the raw key
  span, the raw value span and the next `at`, or `None` at `query.end`. Fields are separated
  by `&` only, so `;` is an ordinary byte. Empty fields (`a&&b`) are skipped. A field with
  no `=` has an empty value span at its end, so `flag` gives key `flag` and value `""`. The
  key ends at the first `=`, so the value may hold `=`. Keys and values stay encoded. Decode
  each with `decode_text`.
  Ex: `let mut at = q.start; while let Some((k, v, n)) = std.uri.next_pair(s, q, at) { ..; at = n }`

## Decisions

- **Spans into the caller's text, ADAPTED from Zig `std.Uri`** (zig bff490a2
  `lib/std/Uri.zig`). Zig's `Component` keeps each part raw or percent-encoded and decodes on
  demand, and its parser is a single pass. Both are kept. Zig is lenient where RFC 3986 is
  not: `fmt.parseInt` accepts signs and `_` in ports and escapes, an unescaped `@` is
  allowed in userinfo, and malformed escapes stay literal. Those are REPLACED by strict ABNF
  checks, because two parsers that disagree on one URL are a request-smuggling and
  SSRF-bypass class.
- **Go `net/url` NOT TAKEN for parsing** (go 46b62495 `src/net/url/url.go`). Go decodes
  eagerly into `Path` and keeps `RawPath` only sometimes, accepts many bytes RFC 3986
  refuses, and accepts RFC 6874 zones. Go's `ResolveReference` (`url.go:1132`) is RFC 5.2
  and its tests are an oracle. Its `ParseQuery` (`url.go:962-1020`) decodes `+` as a space
  and refuses `;`. Here `+` stays literal (B-4) and `;` is literal, as WHATWG
  `URLSearchParams` treats it. The rest of Go's field rules are ADOPTED: split on `&`, skip
  empty fields, and treat a missing `=` as an empty value.
- **WHATWG URL NOT TAKEN** (rust `url` 2.5.8 implements it). It repairs input (backslashes,
  tabs, missing slashes), normalizes per scheme, and runs IDNA. That makes it a browser's
  parser, not a protocol's. A service that must accept what a browser address bar accepts
  needs that, and it is a separate library.
- **Encode sets by component, ADOPTED from rust `percent-encoding`** (2.3.2 `src/lib.rs`,
  `AsciiSet`). The sets here are named fixed values, not ones the caller builds.
- **Goose** has no URI module. The caller-owned-output convention is followed (ADOPTED).
- **Spans in the public API, extending HTTP-6.** HTTP-6 keeps `http1.Span` offsets inside
  std for the handler-facing `Request`, which is anchored to one buffer and lives for one
  call. A parsed `Uri` is different. `resolve` writes into `out` and returns spans of it
  while `out` is still `mut`, and a caller keeps a `Uri` in records and maps next to its
  text. Under OWN-19, a view would freeze its anchor and could not be a record field of a
  stable type or a collection element. So `Uri` holds plain spans, and `decode_text` or
  `string.slice` turns one into text. This rule does not change HTTP-6: handlers still see
  views.
- **Extends HTTP B-4** without changing it. B-4 rules how a route's query decodes, and
  `next_pair` with `decode_text` is that rule as a library. `http1` keeps its query raw and
  routing never reads it (http.md HTTP-4).

## Left out

RFC 3986 §6 normalization (case, percent-encoding and path normalization; resolution already
removes dot segments), IRIs and IDNA, WHATWG URL parsing, `application/x-www-form-urlencoded`
`+`-as-space decoding, the `authority-form` and `asterisk-form` request targets (`http1`
parses those), host lowercasing, IPv4 and DNS validation of reg-names, IPvFuture, zone ids, and a query map type
(`next_pair` into a `Map` is three lines).

## Laws (planned for the build lane, `core/std/tests/uri.hirsel`)

- **U1 RFC resolution.** RFC 3986 §5.4.1's 23 normal and §5.4.2's 19 abnormal examples
  against base `http://a/b/c/d;p?q` give the RFC's targets byte for byte, with `http:g`
  strict. Plus two hand-derived rows from §5.2.2: `http://a/b/../c` gives `http://a/c`, and
  `HTTP:./g` gives `HTTP:g`. Oracle: the RFC text. Bug caught: a reference with a scheme
  copied without dot removal (no §5.4 row tests it), or merging with the base's last segment kept
  (`../g` going to `http://a/b/c/g`), or `g;x=1/../y` treating `;` as a separator.
- **U2 Round trip and presence.** For every reference U1, U5 and U7 accept, and for Go
  `urltests` (`url_test.go:30`) inputs that are strict RFC 3986, `push` gives the input back.
  `http://h`, `http://h?`, `http://h#`, `http://h:`, `http:` and `//h` give different
  presence for query, fragment, port and host. Oracle: the invariant and hand derivation.
  Bug caught: an empty query folded into `None`, so `push` drops the `?`.
- **U3 Strict rejection.** Each of about 25 inputs fails `Invalid(i)` at its hand-derived
  offset: a space, `%4`, `%G0`, `http://h:65536`, `http://h:+80`, `http://a@b@c`,
  `http://[fe80::1%25en0]`, `http://[v1.x]`, `http://[::1`, `1a:b` (as a relative first
  segment `./1a:b` is fine), `é`, `\`, `{` and a NUL. Oracle: RFC 3986 ABNF, by hand.
  Bug caught: Zig's leniencies (a signed port, a literal `%`), or the `@` split at the last
  `@`.
- **U4 Encode sets.** For each `Set` and all 256 byte values, `encode` of the one byte
  equals Python `urllib.parse.quote(bytes([b]), safe=S)` with `S` that set's extra
  characters (`quote` already leaves `ALPHA DIGIT - . _ ~` alone and writes upper-case hex).
  `decode` of it gives the byte back. Produced by `core/tools/uri_reference.py`. Oracle:
  CPython's `urllib.parse`. Bug caught: `QueryPart` leaving `+` or `&` unescaped, or `~`
  escaped (the RFC 2396 rule).
- **U5 Decode.** `a+b` decodes to `a+b`, `a%2Bb` to `a+b`, `%e2%82%ac` (lower case) to `€`.
  `%FF` fails `NotUtf8` at its `%` through `decode_text` and decodes to byte 255 through
  `decode`. Oracle: B-4 and hand derivation. Bug caught: `+` decoded as a space.
- **U6 Query pairs.** Go `parseTests` (`url_test.go:1521`), used by `TestParseQuery`, with
  `next_pair` and `decode_text`. Rows with `+` get B-4's literal plus, and rows with `;`
  (errors in Go) get `;` as data. Each changed row carries a comment naming the deviation.
  Plus `a=1=2` gives value `1=2`, and `&&` gives no pairs. Oracle: Go's table as adapted.
  Bug caught: splitting the value at its second `=`, or yielding an empty pair for `&&`.
- **U7 IPv6.** The nine RFC 3986 §3.2.2 `IPv6address` forms (`::`, `::1`, `1::`, eight
  groups, `::ffff:192.0.2.1`, ...) parse. `1:2:3:4:5:6:7:8:9`, `1::2::3`, `::ffff:256.0.0.1`,
  `12345::` and `[::1]x` fail at hand-derived offsets. Oracle: the ABNF. Bug caught: a `::`
  counted twice, or an IPv4 tail accepted with a 3-digit octet over 255.
- **U8 Resolve bounds.** Resolving every U1 reference into an `out` that already holds 3
  bytes gives spans offset by 3. With an allocator one byte short of the reserve, `out` is
  unchanged. Oracle: the contract. Bug caught: spans relative to the target's start rather
  than to `out`.

Prior vectors: `771885fb^:stdlib/std@1/uri.hir` and
`c5690b37^:conformance/SEM/SEM-3-uri-zig-{01..22}-*-run.hir`,
`SEM-3-uri-rfc3986-{normal,abnormal}-run.hir`, `SEM-3-uri-strict-numeric-run.hir` and
`SEM-3-uri-query-pairs-run.hir`. These follow Zig's test groups (`Uri.zig:466-866`). The old
module kept Zig's empty-path-to-`/` rendering and literal malformed escapes. Here those rows
change to the round-trip and strict-escape answers above.
