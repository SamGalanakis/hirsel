# The nine M1 workloads and the library operations they use

The source is the corpus definitions, `core/corpus/m1/<workload>/SPEC.md`, with their
`cases.json`, inputs and EXPECTED files, and `core/SPEC-COMPARISON.md`. The spec's illustrative
examples are not a source here. For each workload, the list names the library operations a
Hirsel version calls under spec revision 2. Language forms are not listed: `for`, `while let`,
`xs[i]`, `region`, `bound`, `?`, `==`, `<`, records, `match`, `replace` and resource `drop`.
Operations marked CORE are in core.md; the rest are library code in this directory. An `Nn` or
`Pn` tag means the operation needs a gap closed (SPEC-GAPS.md in the brief folder, summarised in
API.md).

What a workload does in its own code is not a library operation. `generics` writes its own
fallible generic map and its counting, because the point of that workload is to exercise
user generics. `rope` and `versions` count live chunks and nodes themselves. v1 has no
cells (CONC-12), so a shared counter like the Rust twins' cannot be written. Instead, each
counts the distinct ids reachable from its live roots into a `set.Set[u64]`. The SPEC's
oracle is defined the same way: the union of live origins or nodes.

**Verdict under revision 2.** All nine can be written. None needs an operation marked NEEDS
CORE except json's `float.parse` (N4). Three are awkward: `files` leaks its temporary
directory (N3) and relies on assumed `OpenMode` meanings (N5); `generics` must keep counts in
a side list (P5); every workload that cuts lines writes `find` plus two `slice`s instead of
`split_once` (N2).

## Shared by all nine (the driver protocol)

A run has two arguments: an input path and a variant (`twins/support.rs`). The run
configuration grants `main` a `Dir` for the case directory, the `Stdout` and the `Args`. The
driver passes the input path relative to that Dir.

- `args.get` twice (CORE), then `file.read_string`.
- Lines and fields are cut with `string.find` and `string.slice` (CORE); numbers are read with
  `int.parse` (CORE). `split_once` would read better, but a library cannot return the sub-view (N2).
- A named Capacity site is matched with `string.starts_with` and `string.slice` (CORE), then `==`
  on the variant. The program builds `Capacity(requested: r, available: a)` itself, which works
  because Capacity is a plain record. The site is logical, as in the corpus; no admission is forced.
- Output: `string.new`, `string.push`, `string.push_u64` (all CORE), then `stdout.write` (CORE).
- Checksum: `bytes.sum(string.as_bytes(out))` (`as_bytes` CORE).
- The acquire/release trace. The release lines must be printed after the result lines,
  and in the order the owners are actually cleaned. With no cells, a resource's `drop` cannot
  append to a list it does not own. Each traced resource therefore holds a `Sender[String]` to
  one trace channel. Its `drop` calls `channel.try_send`, and `main` drains the channel with
  `channel.receive` once every owner is gone. This uses `channel.new` and `channel.sender` (CORE;
  `sender` admits nothing, so a drop path never admits). The capacity is the number of trace
  events, known per case. This is the one place the nine need a channel inside a single task
  (G18, an intended gap).

## bfs

The header gives the vertex count, source and target; then come directed edges in input order. All
vertices are validated before any search allocation. The parent and queue lists have V elements each
(sites `search.parent` and `search.queue`). A vertex is marked when it is enqueued. The path is a newly
owned list (site `result.path`), and NoPath is returned for an unreachable target.

- `list.with_capacity` (CORE) for adj, parent, queue and path; `list.fill` (CORE, grows to length V)
  for the sentinel; `list.push` (CORE), including `list.push(mut adj[u], v)` on an indexed place
- `list.len`, `list.get` (CORE); `list.reverse` for the walk back from the target
- The path is printed with `string.push_u64` (CORE) and `,` separators

## files

1. Make a private temporary directory, write the source into it, open the source, create the
   destination, and take an exclusive OS lock on a third file.
2. Copy through a 4,096-byte buffer (site `copy.buffer`).
3. The error-work case writes exactly 17 bytes and then fails `Io@copy`.
4. Clean up in the order lock, destination, source. Cleanup reports are fallible: the first error is
   primary and the later ones are suppressed, in order.
5. Probe the lock with a freshly opened file, then report the destination's size and sum.

- `dir.create_temp` (`host.mkdtemp`) returns the temporary Dir. Nothing can remove it afterwards:
  `dir.remove_all` needs its name in the case Dir (N3). Each run leaks one directory.
- `file.write(tmp, "source.txt", data)` for the source.
- `file.open` with `Read` for the source, `CreateNew` for the destination (error-second hits an
  existing destination), and `Write` for the lock file. That assumes `Write` creates a missing file
  (N5); truncating a lock file is harmless. The probe opens the lock file with `Read`.
- `file.try_lock` (error-lock meets a real held lock), `file.unlock`, `file.close`
- `list.with_capacity(4096)` (CORE), `list.clear` (CORE), `file.read_into`, `file.write_all`, and
  `list.slice(buf, 0, 17)` (CORE) for the 17-byte prefix before the failing write
- `file.read`, `list.len` (CORE) and `bytes.sum` for `destination-bytes` and `sum`
- `list.push` (CORE) to collect the suppressed causes

## generics

Lines become a `List[String]`. A scoped, non-retaining callback appends `!` and returns a
new owned String (site `callback2.string`). Strings of 6 or more UTF-8 bytes are counted, and the
mapped strings are tallied in first-insertion order (site `tally2.key`). The map, mapped list
and list are released in reverse order.

- `list.with_capacity`, `list.push`, `list.len`, `list.get`, `list.set` (CORE)
- `string.copy`, `string.push`, `string.byte_len` (CORE)
- The tally is the corpus's idiomatic shape, "HashMap plus insertion-order vector": a
  `Map[String, u64]` from key to index (`map.new`, `map.get`, `map.insert`, CORE) and a
  `List[u64]` of counts bumped with `list.set`. A repeat allocates nothing. Without the side list,
  a repeat would need `map.insert` with a fresh key copy, an admission per hit (P5). The output
  walks `for (k, i) in m` (SEM-6) and reads the count at `i`.

## json

The first line is a `/`-separated path; each later line is one document. The program parses
null, booleans, finite numbers (`1e999` is rejected), strings with escapes and surrogate
pairs, arrays and objects. A duplicate key keeps its first position and takes the last value. Each
query is printed as compact JSON, and the whole document is pretty-printed with 2-space indentation
to count its bytes and sum. Capacity hits the third value in preorder of document 2.

- The parser reads bytes: `string.as_bytes` (CORE) and `xs[i]`, `list.len` (CORE). Unescaped runs
  and number lexemes are cut with `string.slice` (CORE) and kept with `string.copy` (CORE).
- Numbers: `float.parse` (NEEDS CORE, N4) then `float.is_finite`. The lexeme is kept as text, so
  no float is formatted. Only the finiteness check needs the value; a hand check of the exponent
  could replace it, but that is a JSON-number reimplementation, not a library call.
- `\uXXXX`: `int.parse_radix` (base 16), then the surrogate pair is combined in user code, then
  `char.from_u32` (CORE) and `string.push_char` (CORE).
- Tree: `List[Json]` and `Map[String, Json]`, built with `list.push` (CORE) and `map.insert`
  (CORE). The first-position/last-value rule for duplicates is exactly `map.insert`'s rule.
- Query: `map.get` (CORE), `list.get` (CORE), and `int.parse` (CORE) for an index step
- Printing: `string.next_scalar` and `char.to_u32` (CORE) to escape, `string.push_uint` (base 16,
  width 4) for `\u00XX`, plus `string.push` and `string.push_char`. `for (k, v) in m` gives
  object order (SEM-6).

## lru

There are at most 64 entries in a recency chain linked by handles.
- `P key value` prepares a payload (site `put2.payload`) before anything changes, then installs or
  replaces the entry; at capacity the tail is evicted.
- `G key` returns a copied payload and moves a hit to the front.
- `S` tries the handle of the first entry ever inserted, which must be stale after reuse.

- `pool.with_capacity` and `pool.len`; `pool.insert`, `pool.get`, `pool.update` and `pool.remove`
  (eviction). Relinking sets prev and next inside `update`. Replacing a payload lends the staged
  payload to the callback (OWN-21) and swaps it in with
  `replace(mut e.payload, replace(mut staged, string.new()))`.
- The index is a `Map[String, pool.Id[Entry]]`; head, tail and each entry's prev and next are
  `Option[pool.Id[Entry]]`. Ids are plain and checked at run time, so they are stored directly.
- The saved first Id is probed with `pool.get`, which returns `Error(Removed)` after reuse; the
  output's StaleHandle comes from that.
- `map.get`, `map.insert`, `map.remove` (CORE)
- `string.copy` (CORE) for the key, the payload and the copied hit

## pipeline

There are three tasks (source, transform, sink) and two channels holding 8 batches each. Batches are
owned and moved from task to task. Transform allocates the uppercase output (site `batch2.output`).
- Normal case: before the consumer is released, the source shows that `try_send` returns Full on q1.
  Batch release ids are printed sorted.
- Failure cases: the actual release order is printed.
- Cancellation cases: one cancels at an explicit transform check, one at the next receive, and one at
  a send blocked on a full queue.

- `channel.new` (CORE, 3 channels: q1, q2 and a 1-slot gate the transform waits on), `channel.sender`,
  `channel.send`, `channel.try_send`, `channel.receive` (CORE)
- `task.spawn` with a plain `fn(take X) -> R waits` per stage and its endpoints as `x`, `task.join`,
  `task.cancel`, `task.check_cancel` (CORE)
- `string.copy` (CORE) for the new uppercase row (site `batch2.output`), then
  `string.make_ascii_upper`
- `list.with_capacity`, `list.push` (CORE) for rows; `list.sort` for the release ids

## request-scratch

This is a finite scripted request plan. Requests are separated by blank lines. Each one starts with
`GET <route>`, followed by `Key: value` headers. There is one immutable Arc-held route configuration.
Each request has its own scratch region holding header position records, so nothing is copied. The
program renders `banner:id:content` and delivers it through the scripted writer.
- Failure cases: a header without a colon fails the request, site `request2.header2` fails while
  parsing the headers, and site `reply` fails when the reply storage is admitted.
- Order: request 1's pair is released before request 2 acquires.

- `arc.new`, `arc.share` (CORE); `map.new`, `map.insert`, `map.get` (CORE) on the config
- `string.starts_with` and `string.slice` (CORE) for `GET `, `string.find` and `string.slice` for
  lines and `:`, and `string.slice` to read a header back from its positions
- `span.builder`, `span.push`, `span.finish`, `span.len`, `span.get` (CORE) in the request's
  `region`, holding a plain stable `Header { name: (u64, u64), value: (u64, u64) }`
- `string.with_capacity` (site `reply`), `string.push` (CORE), then `stdout.write` (CORE)

## rope

A piece table over Rc chunks. An edit names a UTF-8 byte range and its replacement. Both
boundaries are validated, then the new chunk and the new piece list are staged (site `edit2.piece`).
The edit is installed only after that, and the prior piece list is pushed as undo. At most 64 undo
entries are kept, and the oldest is evicted explicitly. UNDO restores the latest snapshot. The
program reports live chunk counts after the history is released, and again after the current text
is released.

- `rc.new`, `rc.share` (CORE) for chunks; `Chunk { id: u64, text: String }`
- `string.is_boundary`; `string.byte_len`, `string.copy` (CORE)
- `bytes.sum(list.slice(string.as_bytes(c.text), from, to))` (`slice`, `as_bytes` CORE) to sum a
  piece's byte range; `string.slice` (CORE) to render it
- `list.with_capacity`, `list.push`, `list.pop`, `list.len` (CORE); `list.remove` at index 0
  to evict the oldest of 64 (O(64))
- `set.new`, `set.insert`, `set.len` for the live count

## versions

A balanced immutable BST over keys `0..n`. Updates path-copy, and the new root is published
only after every node is admitted (site `draft.node3`). One pin is held on the initial root. The
program sums both versions, then releases the pin, then the current root, and reports the live-node
counts at each step.

- `arc.new`, `arc.share` (CORE); `Node { id: u64, key: u64, value: u64, left, right: Option[Arc[Node]] }`
- `set.new`, `set.insert`, `set.len` for the live count

## Later: specified in the seed but used by none of the nine

The contract is written so the shape is fixed. The implementation lane may leave these
bodies for M2.

- `list.insert`, `list.swap_remove`, `list.swap`, `list.truncate`
- `string.from_utf8`, `string.into_bytes`, `string.push_i64`, `string.ends_with`,
  `string.compare`, `string.clear`
- `text.len`, `text.copy_in` (CORE): none of the nine keeps text in a region
- `map.contains`, `map.set` (P5); `set.contains`, `set.remove`
- `pool.new`, `pool.get_or`, `pool.contains`
- `file.remove`; `dir.create_dir`, `dir.remove_all` (its use in files is blocked by N3)
- `args.len`
- `span.fixed_builder`, `span.indexed_builder`
- `int.compare`, `char.utf8_len`, `char.digit`, `char.to_ascii_upper`; `float.parse` at f32

## Left out of the seed entirely (no contract written)

Each item is a later addition that changes no seed signature.

- `list.sort_stable` (allocates a buffer of len / 2), `list.retain`, `list.dedup`,
  `list.binary_search`, `list.map` and other callback combinators
- Sorted maps (`ordered` keys), `map.update` with a callback (needs P5 and a placeholder),
  `map.with_capacity` and `map.clear` (no core primitive, and `replace` of a whole Map through
  `mut` depends on N1), `map.shrink`
- `pool.clear`, pool iteration, branded Ids and `SlotOwner` (not in the first version)
- `string.split_once`, `string.strip_prefix` and `text.as_string` (N2); `string.trim`,
  `string.split` iteration, `string.replace`, case mapping beyond ASCII, float formatting,
  normalisation; `string.scalars` and `positions` cursors (`for` takes only built-in sources)
- `text.from_utf8_in` (copies through a temporary without N2), Text slicing
- `dir.narrow` (no host primitive), a Path type
- Shared and blocking file locks, seek, sync, directory listing, metadata, rename, a read-write
  open mode (N5)
- `Net`, `Random`, `Stdin`, `Env`, `Process` operations (core.md lists no host primitives);
  request-scratch's socket is scripted and is not `Net`
- `channel.try_receive`, channel selection, timeouts (none in v1)
- `span.reserve`, `span.copy_in`: `build_in` and `prepare_in` already admit a final size once
