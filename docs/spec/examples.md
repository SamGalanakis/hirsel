# Examples

Nine programs written in v1, each followed by edits that are refused and the rule each
cites. Helper functions whose bodies add nothing are shown by signature only. These
examples are informative; the rules they cite are normative.

## 1. Request server with per-request scratch

Configuration is immutable and counted; each request parses into a region inside a bound
scope, so one request can use at most 64 KiB and leaves nothing behind.

```hirsel
type Config: shareable + send { routes: Map[String, u64], banner: String }
stable type Head { path: Text, length: u64 }
type ServeError = Io(IoError)

fn parse_head(r: Region, mut s: Socket) -> Result[Head in r, Capacity]
fn render(r: Region, cfg: Config, h: Head) -> Result[Bytes, Capacity]

fn handle(mut s: Socket, cfg: Arc[Config]) -> Result[(), ServeError] {
  let sent = bound bytes 65536 {
    region r {
      let head = parse_head(r, mut s)?            // Capacity leaves the bound scope
      let reply = render(r, cfg, head)?
      socket.write(mut s, reply)
    }
  }
  match sent {
    Ok(res) => Ok(res?),
    Error(_) => Ok(socket.write(mut s, b"HTTP/1.1 413\r\n\r\n")?),
  }
}
```

- Returning `head` from the region block: refused, MEM-7.
- `type Config: shareable + send { routes: Map[String, u64], log: Rc[Node] }`: refused,
  CONC-2.
- `fn parse_head(mut s: Socket) -> Head`: refused, MEM-2 (a stable value needs a region).

## 2. JSON tree, path query and pretty-print

The document is one compact blob; a query returns a view anchored to the document.

```hirsel
@compact(u32) stable type Json = Null | Bool(bool) | Num(f64) | Str(Text)
  | Arr(SeqSpan[Link[Json]]) | Obj(SeqSpan[Member])
@compact(u32) stable type Member { key: Text, value: Link[Json] }

fn parse(r: Region, src: Bytes) -> Result[Json in r, ParseError]

fn field(doc: Json, key: String) -> Option[View[Json]] in doc {
  match doc {
    Obj(ms) => { for m in ms { if text.eq_string(m.key, key) { return Some(m.value) } }; None }
    _ => None,
  }
}

fn print(doc: Json, mut out: String) -> Result[(), Capacity] {
  match doc {
    Num(n) => string.push_f64(mut out, n),
    Arr(xs) => { for x in xs { print(x, mut out)? }; Ok(()) }
    _ => string.push(mut out, "…"),
  }
}
```

- `fn field(doc: Json, key: String) -> Option[View[Json]]` without `in doc`: refused,
  OWN-20.
- `xs[2]` on `Arr(xs)`: refused, MEM-12 (a `SeqSpan` has no indexing).
- `type Cache { last: View[Json] }`: refused, OWN-19.

## 3. LRU cache used by request handlers

Entries live in a generation-checked pool; links between entries are branded ids.

```hirsel
type Entry { key: String, value: Bytes, prev: Option[Id[Entry] of .pool], next: Option[Id[Entry] of .pool] }
type Lru { pool: Pool[Entry], index: Map[String, Id[Entry] of .pool], head: Option[Id[Entry] of .pool] }

fn get(mut l: Lru, k: String) -> Option[u64] {
  let id = match map.get(l.index, k) { Some(i) => i, None => return None }
  touch(mut l, id)
  Some(bytes.len(pool.get(l.pool, id)?.value))
}
fn touch(mut l: Lru, id: Id[Entry] of l.pool)
```

- `fn get_any(a: Lru, b: Lru, id: Id[Entry] of a.pool) -> .. { pool.get(b.pool, id) }`:
  refused, MEM-18.
- Moving `l.pool` out while `l.head` holds an id: refused, OWN-14.

## 4. Graph search returning a path

The graph is an own edge list; the search scratch is own lists cleaned when the function
returns, and the path is a new own list.

```hirsel
type Graph { adj: List[List[u32]] }

fn path(g: Graph, from: u32, to: u32) -> Result[Option[List[u32]], Capacity] {
  let mut prev: List[u32] = list.new()
  let mut queue: List[u32] = list.new()
  list.fill(mut prev, list.len(g.adj), 0xffff_ffff)?
  bfs(g, from, mut prev, mut queue)?
  walk_back(prev, from, to)
}
fn bfs(g: Graph, from: u32, mut prev: List[u32], mut q: List[u32]) -> Result[(), Capacity]
fn walk_back(prev: List[u32], from: u32, to: u32) -> Result[Option[List[u32]], Capacity]
```

- `for v in g.adj { list.push(mut g.adj, ..) }`: refused, OWN-10.
- Returning a `View[List[u32]]` into `g` without `in g`: refused, OWN-20.

## 5. Three tasks with backpressure

Batches move through two bounded channels; a full channel makes the sender wait.

```hirsel
type Batch: send { rows: List[String] }
type Ends: send { input: Receiver[Batch], output: Sender[Batch] }

fn transform(take e: Ends) -> Result[(), Closed[Batch]] {
  while let Ok(take b) = channel.receive(e.input) {
    let out = upper(take b)
    channel.send(e.output, take out)?
  }
  Ok(())
}
fn upper(take b: Batch) -> Batch
```

`main` creates the channels with `channel.new(8)?`, spawns the source, `transform` and
the sink with `task.spawn(tasks, bytes 1048576, take ends, transform)?`, and joins all
three. Cancelling `main` drops the handles, which cancels each child at its next `send`
or `receive` (CONC-7, CONC-8), and queued batches are dropped (CONC-9).

- Declaring `Batch: send` with an `Rc` field: refused, CONC-2.
- Passing `e.input` to a second task while the first holds `e`: refused, OWN-5.
- Using `b` after `channel.send(e.output, take b)`: refused, OWN-5.

## 6. Versioned map with pins

Readers pin a version by holding an `Arc`; a writer publishes a new version, and an old
version is freed when its last pin goes.

```hirsel
type Version: shareable + send { n: u64, data: Map[String, u64] }

fn publish(mut cur: Arc[Version], take next: Version) -> Result[(), (Capacity, Version)] {
  let v = arc.new(take next)?
  let _ = replace(mut cur, take v)                 // old released at statement end; freed if unpinned
  Ok(())
}
fn pin(cur: Arc[Version]) -> Arc[Version] { arc.share(cur) }
```

- `map.insert(mut cur.data, ..)`: refused, MEM-15 (an `Arc` payload is immutable).
- `type Version: shareable { log: File }`: refused, OWN-4.

## 7. Text buffer with edits and undo

Pieces are counted and shared between the current text and the undo stack.

```hirsel
type Piece: shareable { text: String }
type Buffer { pieces: List[Rc[Piece]], undo: List[List[Rc[Piece]]] }

fn snapshot(mut b: Buffer) -> Result[(), Capacity] {
  let mut snap: List[Rc[Piece]] = list.with_capacity(list.len(b.pieces))?
  for p in b.pieces { let _ = list.push(mut snap, rc.share(p)) }   // capacity is reserved
  match list.push(mut b.undo, take snap) { Ok(()) => Ok(()), Error((c, _)) => Error(c) }
}
```

- `for p in b.pieces { list.push(mut b.pieces, ..) }`: refused, OWN-10.
- Mutating `p.text` through an `Rc[Piece]`: refused, MEM-14.

## 8. Two files, then a lock, with failure at every step

Each resource is cleaned once on every path; an error at any step drops what is already
open in reverse order.

```hirsel
type CopyError = Io(IoError)

fn copy_locked(d: Dir, a: Path, b: Path) -> Result[(), CopyError] {
  let src = file.open(d, a)?
  let dst = file.create(d, b)?
  let lock = file.lock(d, path.of("lock"))?
  file.copy(src, mut dst)?                         // on error: lock, dst, src are dropped
  file.close(take dst)?
  file.unlock(take lock)?
  Ok(())                                           // src dropped here
}
```

- `file.close(take dst)` followed by `file.copy(src, mut dst)`: refused, OWN-5.
- A finalizer that uses `?`: refused, FAIL-11.

## 9. List, Map, String and callback APIs

```hirsel
fn count_long(xs: List[String], min: u64) -> u64 {
  list.count_if(xs, fn(s: String) -> bool { string.byte_len(s) >= min })
}
fn tally(xs: List[String]) -> Result[Map[String, u64], Capacity] {
  let mut m: Map[String, u64] = map.new()
  for x in xs {
    let n: u64 = match map.get(m, x) { Some(v) => v, None => 0 }
    match map.insert(mut m, string.copy(x)?, n + 1) { Ok(()) => {}, Error((c, _, _)) => return Error(c) }
  }
  Ok(m)
}
```

- `fn count_long(xs: List[String]) -> fn(String) -> bool { fn(s: String) -> bool {
  list.len(xs) > 0 } }`: refused, LANG-14.
- `let first = list.get(xs, 0); list.clear(mut xs)` while `first` is used later: refused,
  OWN-10.
