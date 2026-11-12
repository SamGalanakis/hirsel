# The nine M1 workloads and the library operations they use

The source is the corpus definitions, `core/corpus/m1/<workload>/SPEC.md`, with their
`cases.json`, inputs and EXPECTED files, and `core/SPEC-COMPARISON.md`. For each workload, the
list names the operations a Hirsel version calls under spec round 4 (0d649bad), and what it
uses in place of anything revision 3 deleted. Language forms are not listed: `for`, `while let`,
`xs[i]`, `region`, `bound`, `?`, `==`, `<`, records, `match`, `replace` and resource `drop`.
Core operations are named as core declares them (`core/core/`); the rest are the seed's.

What a workload does in its own code is not a library operation. `generics` writes its own
fallible generic map, because the point of that workload is to exercise user generics. `rope`
and `versions` count live chunks and nodes themselves: v1 has no cells (CONC-12), so each counts
the distinct ids reachable from its live roots into a `set.Set[u64]`, which is also how the
SPEC's oracle is defined.

**Verdict under round 4.** All nine can be written, and none needs anything revision 3
deleted. Of the deleted spans (`SeqSpan`, `IndexedSpan`, the fixed and indexed builders) the
seed's nine used none: request-scratch's span was always a `FixedSpan` of a plain record, and
json's tree and rope's document are own data (`List`, `Map`, `Rc`). The "matched" json
variant waits for compact sequences of variable-size elements (see json). Revision 3 removed three
awkward points (the leaked temp directory, the assumed open modes, the side list in generics)
and line cutting is now `split_once`. Round 4 removed two more: float parsing no longer
allocates (`Inline` arrays, `int.mul_wide`), so json converts no third error type, and
request-scratch's builder is sized with `span.with_capacity`, so it abandons no blocks. What
remains awkward, most useful first:

1. **Error conversion (all nine).** `?` propagates only an identical error type (LANG-18), and
   every allocating core call returns `Capacity` or `(Capacity, X)`. Each workload has its own
   error type with a Capacity arm (`JsonError`, `LruError`, `PathError`, ...), so every
   `list.push`, `map.insert`, `string.from`, `string.push` and `rc.new` inside it is wrapped:
   `result.map_err(list.push(mut xs, v), fn(take e: (Capacity, Json)) -> JsonError { Capacity(e.0) })?`,
   or a `match`. Counting the calls sketched below, that is roughly 10 to 30 wrap sites per
   workload: json and request-scratch the most, bfs the fewest (its four `with_capacity` calls).
   A function that returns plain `Result[_, Capacity]` keeps bare `?` with
   `result.capacity_only`, so the useful shape is small helpers returning `Capacity` and one
   conversion at their caller.
2. **generics tally.** `map.get_or` then `map.set` costs two hash probes per repeated word,
   where the side list it replaces cost one probe and an O(1) `list.set`. Either allocates
   nothing on a repeat. The tally uses `map.set`, as asked; the side list stays the faster
   shape if the measurement cares.

## Shared by all nine (the driver protocol)

A run has two arguments: an input path and a variant (`twins/support.rs`). The run
configuration grants `main` a `Dir` for the case directory, the `Stdout` and the `Args`. The
driver passes the input path relative to that Dir.

- `args.get` twice, then `file.read_string`.
- Lines and fields are cut with `string.split_once`, `string.strip_prefix` and `string.find`;
  the results are sub-views of the input, so nothing is copied. Numbers are read with
  `int.parse` (core).
- A named Capacity site is matched with `string.strip_prefix`, then `==` on the variant. The
  program builds `Capacity(requested: r, available: a)` itself; Capacity is a plain record. The
  site is logical, as in the corpus; no admission is forced.
- Output: `string.new`, `string.push`, `string.push_u64`, then `stdout.write`.
- Checksum: `list.sum_bytes(string.as_bytes(out))`.
- The acquire/release trace. The release lines are printed after the result lines, in the
  order the owners are actually cleaned. With no cells, a resource's `drop` cannot append to a
  list it does not own, so each traced resource holds a `Sender[String]` to one trace channel,
  declares a finalizer grant for its line (MEM-27), and its `drop` calls `channel.try_send`;
  `main` drains the channel with `channel.receive` once every owner is gone. `channel.sender`
  admits nothing (Sender is mechanical, MEM-24). The capacity is the number of trace events,
  known per case.
- Errors: each workload's error type has a `Capacity(Capacity)` arm; conversions as in item 1.

## bfs

The header gives the vertex count, source and target; then come directed edges in input order.
All vertices are validated before any search allocation. The parent and queue lists have V
elements each (sites `search.parent` and `search.queue`). A vertex is marked when it is
enqueued. The path is a newly owned list (site `result.path`), and NoPath is returned for an
unreachable target.

- `list.with_capacity` for adj, parent, queue and path; `list.fill` for the sentinel;
  `list.push`, including `list.push(mut adj[u], v)` on an indexed place
- `list.len`, `list.get`; `list.reverse` for the walk back from the target
- The path is printed with `string.push_u64` and `,` separators
- Unchanged by revision 3 apart from error conversion.

## files

1. Make a private temporary directory, write the source into it, open the source, create the
   destination, and take an exclusive OS lock on a third file.
2. Copy through a 4,096-byte buffer (site `copy.buffer`).
3. The error-work case writes exactly 17 bytes and then fails `Io@copy`.
4. Clean up in the order lock, destination, source. Cleanup reports are fallible: the first
   error is primary and the later ones are suppressed, in order.
5. Probe the lock with a freshly opened file, then report the destination's size and sum.

- `let mut name = string.with_capacity(32)?` then `let tmp = dir.create_temp(d, mut name)?`:
  `host.mkdtemp` writes the directory's name into capacity the caller already holds. The run
  ends with `dir.remove_all(d, name)`, so nothing leaks.
- `file.write(tmp, "source.txt", data)` for the source.
- `file.open` with `Read` for the source, `CreateNew` for the destination (error-second hits an
  existing destination: `Exists`), and `Write` for the lock file, which core.md now defines to
  create a missing file. The probe opens the lock file with `Read`.
- `file.try_lock` (error-lock meets a real held lock), `file.unlock`, `file.close`
- `list.with_capacity(4096)`, `list.clear`, `file.read_into`, `file.write_all`, and
  `list.slice(buf, 0, 17)` for the 17-byte prefix before the failing write
- `file.read`, `list.len` and `list.sum_bytes` for `destination-bytes` and `sum`
- `list.push` to collect the suppressed causes
- Opening admits nothing now (HostFile is mechanical); only `try_lock` admits (the Lock's cause
  cell), and the case's Capacity site is logical.

## generics

Lines become a `List[String]`. A scoped, non-retaining callback appends `!` and returns a new
owned String (site `callback2.string`). Strings of 6 or more UTF-8 bytes are counted, and the
mapped strings are tallied in first-insertion order (site `tally2.key`). The map, mapped list and
list are released in reverse order.

- `list.with_capacity`, `list.push`, `list.len`, `list.get`
- `string.from` (was `string.copy`), `string.push`, `string.byte_len`
- Tally: a `Map[String, u64]`. A new word is `map.insert(mut m, string.from(w)?, 1)`; a repeat is
  `let n = map.get_or(m, w, 0)` then `map.set(mut m, w, n + 1)`, which takes no key and admits
  nothing. Insertion order is the map's own, so the output walks `for (k, n) in m` (SEM-6). This
  replaces the side `List[u64]` of counts revision 2 needed; cost in item 3 above.

## json

The first line is a `/`-separated path; each later line is one document. The program parses
null, booleans, finite numbers (`1e999` is rejected), strings with escapes and surrogate pairs,
arrays and objects. A duplicate key keeps its first position and takes the last value. Each
query is printed as compact JSON, and the whole document is pretty-printed with 2-space
indentation to count its bytes and sum. Capacity hits the third value in preorder of document 2.

The seed writes the corpus's **idiomatic** variant: an owned recursive tree,
`type Json = Null | Bool(bool) | Num(String) | Str(String) | Arr(List[Json]) | Obj(Map[String, Json])`.
It used no span in revision 2 and uses none now.

- The parser reads bytes: `string.as_bytes` and `xs[i]`, `list.len`. Unescaped runs and number
  lexemes are cut with `string.slice` and kept with `string.from`.
- Numbers: `float.parse64` for the range check only (`Error(OutOfRange)` is the
  invalid-number case); the lexeme is kept as text, so no float is formatted. It allocates
  nothing and returns core's `ParseError`, converted to the program's `JsonError`.
- `\uXXXX`: `int.parse_uint(hex4, 16)`, then the surrogate pair is combined in user code, then
  `char.from_u32` and `string.push_char`.
- Tree: `list.push` and `map.insert`; the first-position/last-value duplicate rule is exactly
  `map.insert`'s.
- Query: `map.get`, `list.get`, and `int.parse` for an index step.
- Printing: `string.next_scalar` and `char.to_u32` to escape, `string.push_uint` (base 16,
  width 4) for `\u00XX`, `string.push`, `string.push_char`; `for (k, v) in m` gives object order.
- Document release: each document's tree is a local of the per-line loop and is cleaned at the
  end of its iteration; the "document region releases once" trace line is that owner's release.

**The matched variant: waits for compact sequences.** The corpus's matched Rust keeps each
document as one immutable blob with u32 child offsets, built once and freed at document close.
In Hirsel that is one compact value per document in a per-document region. It needs compact
sequences of variable-size elements (a sequential walk, an optional index, and nested
documents built in one blob with no staging tree). Revision 3 does not have them: it keeps only
`FixedSpan` of plain elements, and a `Link` is not a value (MEM-10). They will return in the
first version, from the goose-parts-897 design lane (GOOSE-PARTS.md) and a later spec round.
The seed writes this variant as one blob per document when those rules land, and not before.

Measured reason, for the record: without compact sequences an array or object would be
backward-linked cells, costing 2 x W bytes of links per element instead of W, reverse order
(printing recurses once per element), O(n) index queries, and an own staging tree before every
blob, because the duplicate-key rule must be settled before an immutable blob is written. That
would measure "idiomatic tree, then a compact copy", not the blob.

## lru

There are at most 64 entries in a recency chain linked by handles.
- `P key value` prepares a payload (site `put2.payload`) before anything changes, then installs
  or replaces the entry; at capacity the tail is evicted.
- `G key` returns a copied payload and moves a hit to the front.
- `S` tries the handle of the first entry ever inserted, which must be stale after reuse.

- `pool.with_capacity` and `pool.len`; `pool.insert`, `pool.get`, `pool.update` and
  `pool.remove` (eviction). Relinking sets prev and next inside `update`. Replacing a payload
  lends the staged payload to the callback (OWN-21) and swaps it in with
  `replace(mut e.payload, replace(mut staged, string.new()))`.
- The index is a `Map[String, pool.Id[Entry]]`; head, tail and each entry's prev and next are
  `Option[pool.Id[Entry]]`. Ids are plain data checked at run time (MEM-18).
- The saved first Id is probed with `pool.get`, which returns `Error(Removed)` after reuse; the
  output's StaleHandle comes from that.
- `map.get`, `map.insert`, `map.remove`
- `string.from` for the key, the payload and the copied hit
- Unchanged by revision 3 apart from `string.from` and error conversion.

## pipeline

There are three tasks (source, transform, sink) and two channels holding 8 batches each.
Batches are owned and moved from task to task. Transform allocates the uppercase output (site
`batch2.output`).
- Normal case: before the consumer is released, the source shows that `try_send` returns Full
  on q1. Batch release ids are printed sorted.
- Failure cases: the actual release order is printed.
- Cancellation cases: one cancels at an explicit transform check, one at the next receive, and
  one at a send blocked on a full queue.

- `channel.new` (3 channels: q1, q2 and a 1-slot gate the transform waits on),
  `channel.sender`, `channel.send`, `channel.try_send`, `channel.receive`
- `task.spawn` with a plain `fn(take X) -> R waits` per stage (it captures nothing, so it holds
  no resource, LANG-14) and its endpoints as `x`; `task.join`, `task.cancel`,
  `task.check_cancel`
- `string.from` for the new uppercase row (site `batch2.output`), then `string.make_ascii_upper`
  (its `Capacity` cannot occur on the owned copy, but is converted like any other)
- `list.with_capacity`, `list.push` for rows; `list.sort` with `order.compare` for the
  release ids
- Unchanged by revision 3 otherwise: the stages already took their endpoints as the spawn
  payload rather than capturing them.

## request-scratch

A finite scripted request plan. Requests are separated by blank lines. Each one starts with
`GET <route>`, followed by `Key: value` headers. There is one immutable Arc-held route
configuration. Each request has its own scratch region holding header position records, so
nothing is copied. The program renders `banner:id:content` and delivers it through the scripted
writer.
- Failure cases: a header without a colon fails the request, site `request2.header2` fails
  while parsing the headers, and site `reply` fails when the reply storage is admitted.
- Order: request 1's pair is released before request 2 acquires.

- `arc.new`, `arc.share`; `map.new`, `map.insert`, `map.get` on the config
- `string.strip_prefix(line, "GET ")` for the route and `string.split_once(line, ": ")` per
  header; positions come from `string.find`, and `string.slice` reads a header back
- `span.with_capacity` (the header count is known from the request's lines), `span.push`,
  `span.finish`, `span.len`, `span.get` in the request's `region`, holding a plain
  `Header { name: (u64, u64), value: (u64, u64) }`: a `SpanBuilder[Header, in r]` building a
  `FixedSpan[Header] in r`, one admission per request and nothing abandoned.
- `string.with_capacity` (site `reply`), `string.push`, then `stdout.write`

## rope

A piece table over Rc chunks. An edit names a UTF-8 byte range and its replacement. Both
boundaries are validated, then the new chunk and the new piece list are staged (site
`edit2.piece`). The edit is installed only after that, and the prior piece list is pushed as
undo. At most 64 undo entries are kept, and the oldest is evicted explicitly. UNDO restores the
latest snapshot. The program reports live chunk counts after the history is released, and again
after the current text is released.

- `rc.new`, `rc.share` for chunks; `Chunk { id: u64, text: String }`, the document a
  `List[Piece]` of `(Rc[Chunk], lo, hi)`: own data, no span
- `string.is_boundary`, `string.byte_len`, `string.from`
- `list.sum_bytes(list.slice(string.as_bytes(c.text), lo, hi))` to sum a piece's byte range;
  `string.slice` to render it
- `list.with_capacity`, `list.push`, `list.pop`, `list.len`; `list.remove` at index 0 to evict
  the oldest of 64 (O(64))
- `set.new`, `set.insert`, `set.len` for the live count
- Unchanged by revision 3.

## versions

A balanced immutable BST over keys `0..n`. Updates path-copy, and the new root is published
only after every node is admitted (site `draft.node3`). One pin is held on the initial root. The
program sums both versions, then releases the pin, then the current root, and reports the
live-node counts at each step.

- `arc.new`, `arc.share`; `Node { id: u64, key: u64, value: u64, left, right: Option[Arc[Node]] }`
- `set.new`, `set.insert`, `set.len` for the live count
- Unchanged by revision 3.

## Specified in the seed but used by none of the nine

The contract fixes the shape; the implementation lane may leave these bodies for M2.

- `list.first`, `list.last`, `list.insert`, `list.swap_remove`, `list.swap`, `list.truncate`
- `string.push_i64`, `string.ends_with`, `string.strip_suffix`, `string.trim_ascii`,
  `string.clear`
- `int.parse_int`, `float.parse32`, `float.is_finite`, `order.min`, `order.max`
- `map.contains`; `set.contains`, `set.remove`
- `pool.new`, `pool.get_or`, `pool.contains`
- `file.remove`; `dir.create_dir`
- `char.utf8_len`, `char.digit`, `char.to_ascii_upper`

## Left out of the seed entirely

Each item is a later addition that changes no seed signature.

- `list.sort_stable` (a buffer of len / 2), `list.retain`, `list.dedup`,
  `list.binary_search`, `list.map` and other callback combinators
- Sorted maps, `map.update` with a callback (needs a placeholder V), `map.with_capacity`,
  `map.clear`, an entry API
- `pool.clear`, pool iteration, branded Ids (reserved R24)
- `text.as_string` (impossible: different representations), Text slicing; `string.split`
  iteration, `string.replace`, case mapping beyond ASCII, float formatting, normalisation;
  scalar cursors (`for` takes only built-in sources)
- `dir.narrow` (no host primitive), a Path type
- Shared and blocking file locks, seek, sync, directory listing, metadata, rename, a
  read-write open mode (core.md: there is none)
- `Net`, `Random`, `Stdin`, `Env`, `Process` operations (none in v1)
- `channel.try_receive`, channel selection, timeouts (none in v1)
