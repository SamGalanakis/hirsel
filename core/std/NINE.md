# The nine M1 workloads and the library operations they use

The source is the corpus definitions, `core/corpus/m1/<workload>/SPEC.md`, with their
`cases.json`, inputs and EXPECTED files, and `core/SPEC-COMPARISON.md` (corpus commit
f50aa0da). The spec's illustrative examples are not a source here. For each workload, the list names the library
operations a Hirsel version calls. Language forms are not listed: `for`, `xs[i]`, `region`,
`bound`, `?`, `==`, `<`, records, `match` and resource `drop`. Operations
marked CORE are in the spec's core.md. A Gn tag means the operation needs spec gap n, and a Pn tag
means it needs a core primitive that core.md lacks. Both are listed in SPEC-GAPS.md in the brief folder and summarised in API.md.

What a workload does in its own code is not a library operation. `generics` writes its own
fallible generic map and its counting, because the point of that workload is to exercise
user generics. `rope` and `versions` count live chunks and nodes themselves. v1 has no
cells (CONC-12), so a shared counter like the Rust twins' cannot be written. Instead, each
counts the distinct ids reachable from its live roots into a `Set[u64]`. The SPEC's
oracle is defined the same way: the union of live origins or nodes.

## Shared by all nine (the driver protocol)

A run has two arguments: an input path and a variant (`twins/support.rs`). The run
configuration grants `main` a `Dir` for the case directory and the `Args`. The driver
passes the input path relative to that Dir, because a `Path` is never absolute.
Output is `key=value` lines, then the cleanup trace, then on failure a
`failure=<Kind>@<site>` line. Exit codes 2, 3 and 4 are the driver's mapping of the run outcome
(PROG-3), not a library operation.

- `args.get` twice, then `path.of`, `string.copy` (CORE) and `file.read_string`.
- Lines and fields are cut with `string.split_once` (G1); numbers are read with `int.parse` (CORE).
- A named Capacity site is matched with `string.strip_prefix` (G1) and `==` on the variant. The program
  then builds `Capacity { requested, available }` itself, which works because Capacity is a plain
  record (core.md:15). The site is logical, as in the corpus; no admission is forced.
- Output: `string.new`, `string.push`, `string.push_u64` (all CORE), then `stdout.write` (CORE).
- Checksum: `for b in string.as_bytes(out)` (G1).
- The acquire/release trace. The release lines must be printed after the result lines,
  and in the order the owners are actually cleaned. With no cells, a resource's `drop` cannot
  append to a list it does not own. Each traced resource therefore holds a `Sender[String]` to
  one trace channel. Its `drop` calls `channel.try_send`, and `main` drains the channel with
  `channel.receive` once every owner is gone. This uses `channel.new`, `channel.sender` (CORE,
  fallible after F13) and the two calls above. The capacity is the number of trace events, which is known
  per case. This is the one place the nine need a channel inside a single task;
  SPEC-GAPS G18 asks whether the corpus wants a simpler witness.

## bfs

The header gives the vertex count, source and target; then come directed edges in input order. All vertices
are validated before any search allocation. The parent and queue lists have V elements each (sites
`search.parent` and `search.queue`). A vertex is marked when it is enqueued. The path is a newly owned
list (site `result.path`), and NoPath is returned for an unreachable target.

- `list.with_capacity` (CORE) for adj, parent, queue and path; `list.fill` (CORE) for the
  sentinel; `list.push` (CORE), including `list.push(mut adj[u], v)` on an indexed place
- `list.len`, `list.get` (CORE); `list.reverse` for the walk back from the target
- The path is printed as `string.push_u64` (CORE) with `,` separators

## files

1. Make a private temporary directory, open the source, create the destination, and take an exclusive OS
   lock on a third file.
2. Copy through a 4,096-byte buffer (site `copy.buffer`).
3. The error-work case writes exactly 17 bytes and then fails `Io@copy`.
4. Clean up in the order lock, destination, source. Cleanup reports are fallible: the first error is
   primary and the later ones are suppressed, in order.
5. Probe the lock with a freshly opened file, then report the destination's size and sum.

- `dir.create_temp`, then `dir.remove_all` at the end
- `file.open`, `file.create_new` (error-second hits an existing destination), and
  `file.open_with` with `Create.IfMissing` for the lock file and for the probe
- `file.try_lock` (error-lock meets a real held lock), `file.unlock`, `file.close`
- `bytes.with_capacity` (P2), `bytes.clear` (P2), `file.read_into`, `file.write_all`, and
  `bytes.truncate` (P2) to cut the 17-byte prefix before the failing write
- `file.read`, `bytes.len` (P2) and `bytes.sum` (P2) for `destination-bytes` and `sum`
- `list.push` (CORE) to collect the suppressed causes

## generics

Lines become a `List[String]`. A scoped, non-retaining callback appends `!` and returns a
new owned String (site `callback2.string`). Strings of 6 or more UTF-8 bytes are counted, and the
mapped strings are tallied in first-insertion order (site `tally2.key`). The map, mapped list
and list are released in reverse order.

- `list.with_capacity`, `list.push`, `list.len`, `list.get` (CORE)
- `string.copy`, `string.push`, `string.byte_len` (CORE)
- `map.new` (CORE), `map.update` for a repeat, `map.insert` (CORE) for a first sight, and
  `for (k, v) in m` for the ordered output (G13)

## json

The first line is a `/`-separated path; each later line is one document. The program parses
null, booleans, finite numbers (`1e999` is rejected), strings with escapes and surrogate
pairs, arrays and objects. A duplicate key keeps its first position and takes the last value. Each
query is printed as compact JSON, and the whole document is pretty-printed with 2-space indentation
to count its bytes and sum. Capacity hits the third value in preorder of document 2.

- The parser reads bytes: `string.as_bytes` (G1), `bytes.get` (P2) and `bytes.len` (P2).
  Unescaped runs and number lexemes are cut with `string.slice` (G1) and kept with
  `string.copy` (CORE).
- Numbers: `float.parse` then `float.is_finite`. The lexeme is kept as text, so no float is formatted.
- `\uXXXX`: `int.parse_radix` (base 16), then the surrogate pair is combined in user code, then
  `char.from_u32` (P3) and `string.push_char` (P2).
- Tree: `List[Json]` and `Map[String, Json]`, built with `list.push` (CORE) and `map.insert`
  (CORE). The first-position/last-value rule for duplicates is exactly `map.insert`'s rule.
- Query: `map.get` (CORE), `list.get` (CORE), and `int.parse` (CORE) for an index step
- Printing: `string.next_scalar` and `char.to_u32` (P3) to escape, and `string.push_uint` (base 16,
  width 4) for `\u00XX`, plus `string.push` and `string.push_char`. `for (k, v) in m` gives
  object order (G13).

## lru

There are at most 64 entries in a recency chain linked by handles.
- `P key value` prepares a payload (site `put2.payload`) before anything changes, then installs or replaces the entry; at capacity the tail is evicted.
- `G key` returns a copied payload and moves a hit to the front.
- `S` tries the handle of the first entry ever inserted, which must be stale after reuse.

- `pool.with_capacity` and `pool.len`; `pool.insert`, `pool.get`, `pool.update` (relinking and
  replacement) and `pool.remove` (eviction)
- `pool.handle` and `pool.resolve` for the prev/next links, the map's values and the saved
  first handle (G6, G7). The index is a `Map[String, Handle[Entry]]`, not
  `Map[String, Id[Entry] of .pool]`.
- `map.new`, `map.get`, `map.insert`, `map.remove` (CORE)
- `string.copy` (CORE) for the key, the payload and the copied hit

## pipeline

There are three tasks (source, transform, sink) and two channels holding 8 batches each. Batches are owned and moved
from task to task. Transform allocates the uppercase output (site `batch2.output`).
- Normal case: before the consumer is released, the source shows that `try_send` returns Full on q1. Batch release ids are printed sorted.
- Failure cases: the actual release order is printed.
- Cancellation cases: one cancels at an explicit transform check, one at the next receive, and one at a send blocked on a full queue.

- `channel.new` (CORE, 3 channels: q1, q2 and a 1-slot gate the transform waits on), `channel.sender`
  (CORE), `channel.send`, `channel.try_send`, `channel.receive` (CORE)
- `task.spawn`, `task.join`, `task.cancel`, `task.check_cancel` (CORE)
- `string.copy` (CORE) for the new uppercase row (site `batch2.output`), then
  `string.make_ascii_upper` (P2)
- `list.with_capacity`, `list.push` (CORE) for rows; the release ids use the shared trace channel

## request-scratch

This is a finite scripted request plan. Requests are separated by blank lines. Each one starts with `GET <route>`, followed by
`Key: value` headers. There is one immutable Arc-held route configuration. Each request has its own scratch region
holding header position records, so nothing is copied. The program renders `banner:id:content` and delivers it through
the scripted writer.
- Failure cases: a header without a colon fails the request, site `request2.header2` fails while parsing the headers, and site `reply` fails when the reply storage is admitted.
- Order: request 1's pair is released before request 2 acquires.

- `arc.new`, `arc.share` (CORE); `map.new`, `map.insert`, `map.get` (CORE) on the config
- `string.strip_prefix` (G1) for `GET `, `string.split_once` (G1) for lines and `:`, and
  `string.slice` (G1) to read a header back from its positions
- `span.builder`, `span.push`, `span.finish`, `span.len`, `span.get` (CORE) in the request's
  `region`, holding a plain stable `Header { name: (u64, u64), value: (u64, u64) }`.
  This is provisional on F02/F07: the builder may sit on the new destination brand.
- `string.with_capacity` (site `reply`), `string.push` (CORE), then `stdout.write` (CORE)

## rope

A piece table over Rc chunks. An edit names a UTF-8 byte range and its replacement. Both
boundaries are validated, then the new chunk and the new piece list are staged (site `edit2.piece`).
The edit is installed only after that, and the prior piece list is pushed as undo. At most 64 undo entries are kept, and the oldest is evicted
explicitly. UNDO restores the latest snapshot. The program reports live chunk counts after the history is released, and again
after the current text is released.

- `rc.new`, `rc.share` (CORE) for chunks; `Chunk { id: u64, text: String }`
- `string.is_boundary`, `string.byte_len`, `string.copy` (CORE)
- `string.as_bytes` (G1) and `bytes.get` (P2) to render and sum a piece's byte range
- `list.with_capacity`, `list.push`, `list.pop`, `list.len` (CORE); `list.remove` at index 0
  to evict the oldest of 64 (O(64))
- `set.new`, `set.insert`, `set.len` for the live count

## versions

A balanced immutable BST over keys `0..n`. Updates path-copy, and the new root is published
only after every node is admitted (site `draft.node3`). One pin is held on the initial root. The program
sums both versions, then releases the pin, then the current root, and reports the live-node counts at each step.

- `arc.new`, `arc.share` (CORE); `Node { id: u64, key: u64, value: u64, left, right: Option[Arc[Node]] }`
- `set.new`, `set.insert`, `set.len` for the live count

## Later: specified in API.md but used by none of the nine

The contract is written so the shape is fixed. The implementation lane may leave these
bodies for M2.

- `list.new`, `list.capacity` and `list.reserve` (P1), `list.insert`, `list.swap_remove`,
  `list.swap`, `list.truncate`, `list.set`, `list.clear`, `list.sum`, `list.sort`
- `string.from_utf8`, `string.into_bytes`, `string.push_i64`, `string.find`, `string.starts_with`,
  `string.ends_with`, `string.compare`, `string.clear`, `string.scalars` and `string.positions`
  (G12)
- `text.from_utf8_in`, `text.as_string` (G1), `text.len` and `text.copy_in` (CORE): none of the nine keeps
  text in a region (request-scratch keeps positions)
- `bytes.new`, `bytes.capacity`, `bytes.push`, `bytes.extend`, `bytes.slice`
- `map.with_capacity`, `map.contains`, `map.clear`; `set.contains`, `set.remove`
- `pool.new`, `pool.get_or`, `pool.contains`
- `file.open`'s siblings `file.create`, `file.write`, `file.read_string` beyond the input,
  `file.remove`; `dir.narrow`, `dir.create_dir`
- `clock.now_ms`, `clock.monotonic_ns`; `args.len`; `path.as_string`
- `span.fixed_builder`, `span.indexed_builder`, `span.reserve`, `span.copy_in`
- `int.compare`, `char.utf8_len`, `char.digit`, `char.to_ascii_upper`; `float.parse` at f32

## Left out of the seed entirely (no contract written)

Each item is a later addition that changes no seed signature.

- `list.sort_stable` (allocates a buffer of len / 2), `list.retain`, `list.dedup`,
  `list.binary_search`, `list.extend`, `list.slice` (G1), `list.map` and other callback
  combinators
- Sorted maps (`ordered` keys), `map.entry` with an owned key, `map.update_all`, `map.shrink`
- `pool.insert_owned` returning `SlotOwner[T] of p` (MEM-20), `pool.clear`, pool iteration
- `string.trim`, `string.split` iteration, `string.replace`, case mapping beyond ASCII,
  `string.push_f64` and float formatting, normalisation
- Text slicing (needs G1 for stable bytes, or a copy)
- Shared and blocking file locks, `file.seek`, `file.sync`, directory listing, metadata,
  rename, read-only Dir
- `Stderr`, `Stdin`, `Env`, `Random`, `Net`, `Process` operations; request-scratch's
  socket is scripted and is not `Net`
- `channel.try_receive`, channel selection, timeouts (none in v1)
- `build_in` helpers: blocked by F02/F07 until the destination brand is settled (G9). The nine
  use builders.
