# s3@1

A pure Hirsel package for S3-compatible object storage, using std HTTP/TLS over
an explicit Net and WallClock. SigV4 uses std sha256/hmac; there are no runtime
dependencies or ambient credentials. The store engine stays in std@1/store.

`sign` is pure, exposes the canonical request/string-to-sign for verification,
and signs every supplied header. `canonical_query` sorts encoded key/value
pairs without dropping duplicates; `encode_path` preserves slash boundaries.
The vectors are AWS's published aws4_testsuite from botocore. Each full
signature exceeds the corpus step budget, so a bounded Rust integration test
executes their unchanged Hirsel assertions in `tests/aws-*.hir`.

`Client` is opaque: construct it through `client` or `from_grants`; its network,
credentials, bucket and restrictions cannot be read or replaced by callers.
`client` binds explicit Config/Credentials/Net/WallClock values, narrowing Net to
one endpoint host/port (TLS-only for HTTPS). Missing endpoint authority is `Grant`.
`from_grants` loads the `HIRSEL_S3_<NAME>_*` Env snapshots expanded from a RunConfig
`object_stores` entry, reads a credentials JSON file through its granted Dir,
and obtains the explicitly granted network and wall clock. Credentials JSON:
`{"access_key":"...","secret_key":"...","session_token":"..."}`; the token
may be omitted. Config carries the endpoint origin, bucket, signing region,
object prefix and maximum object size. HTTP redirects and
response decompression are disabled for signed requests.

`read_only(c) -> Client` copies a facet that refuses PUT and DELETE with `Denied`.
`narrow_prefix(c, prefix) -> Client` intersects the current facet with a literal
S3 key prefix relative to Config.prefix. It keeps key names unchanged; use `"tenant/"`
for a slash boundary. Repeated narrowing cannot widen authority; disjoint prefixes
deny all keys. GET, range GET, PUT, DELETE and LIST reject keys or requested list
prefixes outside the facet before reading the clock, signing or networking. A LIST
request must itself be inside the facet (listing `""` does not silently broaden it).

```hirsel
let client = s3.from_grants(io, env, "data")?
let mut reader = s3.narrow_prefix(s3.read_only(client), "tenant/")
let value = s3.get(mut reader, "tenant/item")?
// s3.put(mut reader, "tenant/item", value, s3.Create()) returns Error(Denied()).
// s3.get(mut reader, "other/item") returns Error(Denied()).
```

`get`, `get_range`, `put(Create | Match(etag))`, `list_objects` and `delete`
implement GET, validated Range GET, conditional PUT, paginated ListObjectsV2
and idempotent DELETE. Failed requests are not retried. LIST requests URL
encoding, decodes keys, rejects keys outside the prefix, and returns sorted
unique keys. One listing reads at most `max_list_pages` pages and `max_list_keys`
keys and refuses with `ListLimit()` past either, whatever the server sends. No object exceeds `max_object_bytes`, so there is no multipart
upload.

A store keeps its history behind an authority (std@1/store over
`authority.objects@1`); this package is not a store backend. A store over an
S3 bucket grants that authority the host provider `objects.s3@1`
(`crates/hirsel-object-storage/src/s3`), which signs and sends its own requests
and refuses at open a server that ignores `If-None-Match: *`.

Protocol references read from shallow clones (no server code copied):

- botocore `4d9f375e71c1a97693d961cdefb1827bdaaccfb9`,
  `botocore/auth.py:273` (encoded query-pair sort), `:304` (canonical headers),
  `:373` (canonical request), `:408` (string-to-sign), `:420` (HMAC derivation),
  `:541` (S3 path preservation); the signer ports this structure.

- Garage `ef6a03a4215e9a545945e6e34b43a185dd7b718a`,
  `src/api/common/signature/payload.rs:307` (canonical path/query/headers),
  `src/api/s3/get.rs:817` (GET preconditions), `src/api/s3/list.rs` (LIST v2).
- RustFS `f4f1743d04045528e2c97897febd87c7e26838d4`,
  `crates/e2e_test/src/conditional_write_policy_test.rs:163` (conditional create),
  `rustfs/src/server/layer.rs:1101` (PreconditionFailed responses).
- arrow-rs object_store `279572ea60f3a7a6e5237e066a6f4a35eee611e0`,
  `src/prefix.rs:34,108,129,154` (private wrapper and centralized key mapping);
  facets retain original key names and intersect restrictions instead of rebasing,
  so handing out a facet cannot expand the original namespace. Literal prefixes
  preserve S3 key semantics instead of filesystem path normalization.
  `src/aws/mod.rs:255` (refuse unsupported conditional PUT; map 304/412 to Exists).
- SlateDB `a5c4fd22b36c836a89d8c4f27bf0b7b045e62e32`,
  `slatedb/src/wal/slatedb/store.rs:225` (create-only log objects),
  `slatedb/src/retrying_object_store.rs:290` (verify uncertain completion).

MinIO is not used: its upstream README, “Source-Only Distribution”, states that
community precompiled releases are no longer provided and legacy binaries
receive no updates. Real-server validation uses Garage or RustFS in scratch.

The `e2e/read-only.hir.json` example delegates a `public/` reader.
The standalone `e2e/hirsel.json` program loads a named `data` object store from
`--grants`, reports one conditional create's elapsed nanoseconds, reads the
value back, checks that a second create is refused with `Exists()` and deletes
the key. Grant stdout and a read-only credentials
Dir alongside the `object_stores` declaration (see conformance/old-spec/determinism.md).
Server contract probes against scratch release binaries found RustFS 1.0.3 returns
200 then 412 for competing create-only PUTs and retains the first bytes. Garage
2.4.1 returns 200 twice and replaces the bytes, so it cannot serve a store.

Differences from botocore are explicit Hirsel inputs in place of ambient SDK
credentials, clock and request state; the caller supplies only stable headers,
which are all signed. Bounded byte payloads always use their SHA-256, so there
is no streaming checksum or unsigned-payload branch. HTTP effects use Net and
WallClock grants and never automatically retry, keeping authority and replay
visible to the caller. LIST needs only a bounded scan of the URL-encoded S3 fields
rather than the SDK's general XML model decoder; malformed required fields refuse.
