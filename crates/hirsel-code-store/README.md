# hirsel-code-store

Content-addressed code objects keyed by LANG-6 definition hash, with atomic
`add(bundle) -> Ok | Missing(hashes)`, fetch by hash and an optional signature policy.

**Code objects** (`object.rs`). A definition's object is its hash preimage:
its recursive group's content texts (`hirsel-proof-export` content mode, every
outside reference written as its hash), each member's group references and the
member it is, as `hirsel.code.object.v1` codec text. Decoding requires
canonical form, counts each member's group markers against its references,
recomputes the group's hashes with `group_hashes` and refuses an object whose
member hash differs from the claimed key (`Tampered`). Dependencies are the
`(Ref "<hash>")` nodes parsed from the texts, so nothing in an object is
trusted for what it claims. An object holds its whole group, so `group()`
yields every member's object; a store holds groups whole.



**Signatures** (`sign.rs`). `sign.none@1` (default) checks nothing beyond the
hash. `sign.ed25519@1` with a sorted, unique, non-empty key list requires each
object to carry an Ed25519 signature by a listed key over
`hirsel.code.signature.v1\0 || hash`, from the bundle or already stored; the
hash binds the whole group. `Signer` signs (from a seed or PKCS#8), via
`ring`, which the workspace already uses through rustls.



**Configuration** (`config.rs`). Code storage provider entries:

```json
{"provider": "code.none@1"}
{"provider": "code.dir@1", "path": "/srv/hirsel/code",
 "signatures": {"provider": "sign.ed25519@1", "keys": ["<64 hex digits>"]}}
{"provider": "code.object@1", "objects": {"provider": "objects.dir@1", "path": "/srv/objects"}}
{"provider": "code.sim@1", "seed": 7, "fail_one_in": 8}
```

`code.none@1` (default) holds only the configured revision and refuses adds.
`code.dir@1` stores under a directory, `code.object@1` over any object storage
entry (`hirsel_object_storage::open`), and `code.sim@1` in memory with seeded
faults. Each but `code.none@1` takes an optional `signatures` entry (default
`sign.none@1`). No choice changes what code means: a store changes only which
code a consumer can find.

Dependencies: `hirsel-codec`, `hirsel-object-storage`, `hirsel-proof-export`,
`hirsel-syntax`, `ring`, `serde_json`.
