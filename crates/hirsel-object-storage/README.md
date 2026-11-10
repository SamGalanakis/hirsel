# hirsel-object-storage

The object storage seam shared by host providers that keep immutable objects:
the code store and the object-store-backed storage providers. `ObjectStorage`
has `get_tagged(key)` (bytes and entity tag; `get` and `get_many` derive from
it) and `put_new(key, bytes)`, a conditional PUT (`If-None-Match: *`): it
creates the object whole when the key is absent, durably before it returns
`Created`, and otherwise returns `Exists` and leaves the object unchanged. A
failed put may or may not have created the object. `replace(key, bytes,
condition)` writes a mutable object only when it is `Absent` or `Matches` the
tag a reader saw (`If-Match`), returning the new tag or `Mismatch`. `list(prefix,
after)` returns up to `LIST_PAGE` (1000) keys in order with a `truncated` flag,
and `delete(key)` removes one. A tag is the content's SHA-256 (`content_tag`),
like an S3 ETag. Keys
are `/`-separated segments of lowercase ASCII letters, digits, `.`, `_` and
`-`, none empty or starting with `.` (`check_key`), so a key never escapes a
directory or collides with a temporary file.

Implementations:

- `DirObjects` (`objects.dir@1`): files under a directory. A put writes a
  temporary file, syncs it, hard-links it to the key (refused when the key
  exists, so creation is atomic and never replaces), removes the temporary
  name and syncs the directory. A replace holds an `flock` on `.lock`, compares
  the tag, writes and syncs a temporary file and renames it over the key.
- `MemoryObjects` (`objects.sim@1`): in memory with a bucket's
  conditional-PUT semantics. `with_faults(seed, one_in)` fails each operation
  with probability `1/one_in` from a SplitMix64 stream; a faulted put fails
  either before its write or after it (a lost reply), and a faulted list
  either fails or omits every other key. `overwrite` replaces an
  object as a hostile medium would, for tests.

`open(entry)` opens a run-configuration entry: `{"provider": "objects.dir@1",
"path": "<absolute directory>"}` or `{"provider": "objects.sim@1", "seed": n,
"fail_one_in": n}` (both default 0, never failing). Unknown members and
providers are refused. An S3-compatible client (`objects.s3@1`) implements the
same trait in its own lane.

Dependencies: std, serde_json, sha2 (tags) and libc (`flock`).
