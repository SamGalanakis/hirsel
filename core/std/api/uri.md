# Uri

[API overview](../API.md). [Operations and contracts](../uri.hirsel).

`std.uri` parses, writes and resolves RFC 3986 URI references, and percent-encodes and
decodes their parts. Parsing copies nothing. A `Uri` is a set of spans into the caller's text,
using the same `Span` type as `std.http1`, and each component stays raw (still
percent-encoded) until the caller decodes the one it needs. The grammar is RFC 3986's, held
strictly. A byte the ABNF does not allow is an error at its offset, never a guess, and an
absent component is distinct from an empty one. Query strings follow HTTP B-4
([http.md](../../../docs/spec/http.md#b-4-lexical-forms-json-path-and-query), "Query strings"):
percent-decoding only, with `+` a literal plus. Operations allocate nothing except the
appenders, which grow a caller-owned `String` or `Bytes`.

`next_pair(query, at)` scans raw `Bytes` from offset zero and returns key
and value spans relative to those bytes. For a parsed `Uri`, pass a slice of
the query component. `decode_text(data, span, mut out)` percent-decodes a byte
span into UTF-8 and leaves the output unchanged on lexical errors.

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
pub type Error = Invalid(u64) | NotUtf8(u64) | Relative
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
