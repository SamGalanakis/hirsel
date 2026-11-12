# Core library signatures

The core operations that the rules of this specification name, with their signatures.
Each signature is normative for the effects the rules give it: what it moves, what it
allocates and how it fails. Everything else about the libraries is outside this
specification. Two rules hold for every operation here: an operation that fails
with `Capacity` returns in its error every argument it would have taken and leaves its
other arguments unchanged; and only the failures its signature or comment names can occur,
`Bounds` and `Overflow` being failures ([FAIL-2](failure.md#fail-2-causes)) and the others
`Result` errors. `X?` in a comment marks the rule that governs the operation. Core names
are written unqualified; a type from any other module is written with its module path
(`path.Path`, [LANG-7](language.md#lang-7-names)).

## Error and result values

```hirsel
type Capacity { requested: u64, available: u64 }               // MEM-24
type Closed[T] { item: T }                                     // CONC-5
type SendError[T] = Full(T) | Closed(T)                        // CONC-5
type JoinError = Failed(Causes) | Cancelled(Causes)            // CONC-6
type Cause { kind: CauseKind, site: Site }                     // FAIL-2, FAIL-10
type CauseKind = Overflow | Bounds | Division | Unwrap | Fail(String) | Cancelled | Capacity
type BuildError = Capacity(Capacity) | Overflow                // MEM-4
type IoError = NotFound | Denied | Exists | Closed | Other(u32)
type WriteError { written: u64, error: IoError }               // bytes written before the error
type ParseError = Empty | Invalid(u64) | OutOfRange            // Invalid: the byte position
type FloatError = NaN | OutOfRange                             // SEM-2
type Order = Less | Equal | Greater
```

`Site` is an opaque plain type naming a source position. `Causes` is an opaque own
sequence of `Cause` read with `for`, primary cause first; it is the chain's cause cells
(FAIL-10), so delivering it allocates nothing.

## Integers, floats and chars (SEM-1 to SEM-3)

```hirsel
fn int.wrapping_add[T](a: T, b: T) -> T                        // and sub, mul, neg, shl
fn int.checked_add[T](a: T, b: T) -> Option[T]                 // and sub, mul, div
fn int.saturating_add[T](a: T, b: T) -> T                      // and sub, mul
fn int.widen[T, U](x: T) -> U                                  // only where every T fits in U
fn int.narrow[T, U](x: T) -> U                                 // fails with Overflow
fn int.try_narrow[T, U](x: T) -> Option[U]
fn int.truncate[T, U](x: T) -> U
fn int.parse[T](s: String) -> Result[T, ParseError]
fn float.mul_add[T](a: T, b: T, c: T) -> T
fn float.to_i64[T](x: T) -> Result[i64, FloatError]            // and to the other widths
fn float.total_order[T](a: T, b: T) -> Order
fn char.from_u32(n: u32) -> Option[char]
fn char.to_u32(c: char) -> u32
fn fail(take msg: String) -> Never                            // FAIL-2
```

`T` and `U` above range over the integer or float types only.

## Box, Rc and Arc (MEM-13 to MEM-17)

```hirsel
fn box.new[T](take x: T) -> Result[Box[T], (Capacity, T)]
fn box.unbox[T](take b: Box[T]) -> T
fn rc.new[T: shareable](take x: T) -> Result[Rc[T], (Capacity, T)]
fn rc.share[T: shareable](h: Rc[T]) -> Rc[T]
fn rc.try_unwrap[T: shareable](take h: Rc[T]) -> Result[T, Rc[T]]
fn arc.new[T: shareable + send](take x: T) -> Result[Arc[T], (Capacity, T)]
fn arc.share[T: shareable + send](h: Arc[T]) -> Arc[T]
fn arc.try_unwrap[T: shareable + send](take h: Arc[T]) -> Result[T, Arc[T]]
fn clone[T, U](x: T) -> Result[U, Capacity]                    // MEM-17; U is T or its twin
fn clone_in[T, U](r: Region, x: T) -> Result[U in r, Capacity] // MEM-17
```

A `Box[T]`, `Rc[T]` or `Arc[T]` is read where a view of `T` is expected.

## Lists, strings and maps (SEM-3 to SEM-6)

`Bytes` is `List[u8]`. `String` is an opaque own type over UTF-8 storage.

```hirsel
fn list.new[T]() -> List[T]                                    // allocates nothing
fn list.with_capacity[T](n: u64) -> Result[List[T], Capacity]  // exactly n
fn list.len[T](xs: List[T]) -> u64
fn list.capacity[T](xs: List[T]) -> u64
fn list.reserve[T](mut xs: List[T], n: u64) -> Result[(), Capacity]  // capacity ≥ len + n
fn list.get[T](xs: List[T], i: u64) -> Option[View[T]] in xs
fn list.slice[T](xs: List[T], from: u64, to: u64) -> View[List[T]] in xs   // OWN-8; Bounds
fn list.push[T](mut xs: List[T], take x: T) -> Result[(), (Capacity, T)]
fn list.pop[T](mut xs: List[T]) -> Option[T]
fn list.set[T](mut xs: List[T], i: u64, take x: T) -> T         // returns the old element; Bounds
fn list.clear[T](mut xs: List[T])
fn list.fill[T: plain](mut xs: List[T], n: u64, x: T) -> Result[(), Capacity]   // grows xs to length n
fn list.append[T: plain](mut xs: List[T], ys: List[T]) -> Result[(), Capacity]
fn list.sum[T](xs: List[T]) -> T                               // integers; Overflow
fn string.new() -> String                                      // allocates nothing
fn string.from(s: String) -> Result[String, Capacity]          // copies a literal or a view
fn string.copy(s: String) -> Result[String, Capacity]
fn string.from_utf8(take b: Bytes) -> Result[String, Bytes]    // returns b if it is not UTF-8
fn string.into_bytes(take s: String) -> Bytes
fn string.as_bytes(s: String) -> View[Bytes] in s              // OWN-8
fn string.slice(s: String, from: u64, to: u64) -> View[String] in s   // OWN-8; Bounds
fn string.next_scalar(s: String, at: u64) -> Option[(char, u64)]
fn string.byte_len(s: String) -> u64
fn string.push(mut s: String, t: String) -> Result[(), Capacity]
fn string.push_char(mut s: String, c: char) -> Result[(), Capacity]
fn string.push_u64(mut s: String, n: u64) -> Result[(), Capacity]   // decimal digits
fn text.len(t: Text) -> u64
fn text.copy_in(r: Region, s: String) -> Result[Text in r, Capacity]
fn map.new[K: key, V]() -> Map[K, V]                           // allocates nothing
fn map.len[K: key, V](m: Map[K, V]) -> u64
fn map.get[K: key, V](m: Map[K, V], k: K) -> Option[View[V]] in m
fn map.insert[K: key, V](mut m: Map[K, V], take k: K, take v: V) -> Result[(), (Capacity, K, V)]
fn map.remove[K: key, V](mut m: Map[K, V], k: K) -> Option[V]
fn replace[T](mut q: T, take v: T) -> T                        // OWN-13
```

A string literal has type `String` and is static: it allocates nothing and is never
charged. Indexing `xs[i]` is `list.get` that fails with `Bounds` instead of returning
`None`.

## Regions, builders and spans (MEM-1 to MEM-12)

```hirsel
fn span.builder[T](r: Region) -> Builder[SeqSpan[T] in r]
fn span.fixed_builder[T](r: Region) -> Builder[FixedSpan[T] in r]
fn span.indexed_builder[T](r: Region) -> Builder[IndexedSpan[T] in r]
fn span.push[S, T](mut b: Builder[S], take x: T) -> Result[(), (Capacity, T)]
fn span.finish[S](take b: Builder[S]) -> S                     // MEM-3; cannot fail
fn span.len[T](s: SeqSpan[T]) -> u64                           // and the other spans
fn span.get[T](s: IndexedSpan[T], i: u64) -> Option[T]         // and FixedSpan
fn build_in[T](r: Region, bytes n: u64, w: scoped fn(mut d: Dest[T]) -> Result[Child[T] of d, BuildError]) -> Result[Builder[T in r], BuildError]
fn prepare_in[T](r: Region, staging: T) -> Result[Builder[T in r], BuildError]   // MEM-4
fn finish[T](take b: Builder[T]) -> T                          // MEM-3
```

`dest.put(mut d, L)` is a built-in form, not a function ([MEM-4](memory.md#mem-4-sized-destinations)).

## Tasks and channels (CONC-1 to CONC-11)

```hirsel
fn task.spawn[X: send, R: send](tasks: Tasks, bytes n: u64, take x: X, f: fn(take X) -> R waits) -> Result[Task[R], (Capacity, X)]
fn task.join[R](take h: Task[R]) -> Result[R, JoinError] waits
fn task.cancel[R](mut h: Task[R])
fn task.check_cancel()                                         // CONC-7; may fail with Cancelled
fn channel.new[T: send](cap: u64) -> Result[(Sender[T], Receiver[T]), Capacity]   // Bounds if cap is 0
fn channel.sender[T](tx: Sender[T]) -> Sender[T]               // admits nothing
fn channel.send[T](tx: Sender[T], take x: T) -> Result[(), Closed[T]] waits
fn channel.try_send[T](tx: Sender[T], take x: T) -> Result[(), SendError[T]]
fn channel.receive[T](rx: Receiver[T]) -> Result[T, Closed[()]] waits
```

## Capabilities and host primitives (CAP-1 to CAP-8)

The core capability types are `Dir`, `Net`, `Clock`, `Random`, `Stdout`, `Stderr`, `Stdin`,
`Env`, `Process` and `Tasks`; all are plain and `send`. `Args` is an opaque own type
([PROG-3](programs.md#prog-3-entry)). The provider operations
([CAP-5](capabilities.md#cap-5-provider-operations)) are exactly these; the standard library
builds its capability modules from them. A `HostFile` is a resource whose finalizer closes
it and ignores the outcome. A path argument is resolved relative to its `Dir` and may not
leave it. None of them admits; reads fill capacity the caller already holds.

```hirsel
fn args.len(a: Args) -> u64
fn args.get(a: Args, i: u64) -> Option[View[String]] in a
type OpenMode = Read | Write | Append | CreateNew
fn host.open(d: Dir, p: String, mode: OpenMode) -> Result[HostFile, IoError] waits
fn host.read(mut f: HostFile, mut buf: Bytes) -> Result[u64, IoError] waits   // fills spare capacity
fn host.write(mut f: HostFile, b: Bytes) -> Result[(), WriteError] waits
fn host.close(take f: HostFile) -> Result[(), IoError] waits
fn host.try_lock(mut f: HostFile) -> Result[bool, IoError] waits
fn host.unlock(mut f: HostFile) -> Result[(), IoError] waits
fn host.mkdir(d: Dir, p: String) -> Result[(), IoError] waits
fn host.mkdtemp(d: Dir) -> Result[Dir, IoError] waits
fn host.remove(d: Dir, p: String) -> Result[(), IoError] waits
fn host.remove_tree(d: Dir, p: String) -> Result[(), IoError] waits
fn host.wall_ms(c: Clock) -> u64                               // milliseconds since 1970
fn host.mono_ns(c: Clock) -> u64                               // never decreases in a run
fn stdout.write(out: Stdout, s: String) -> Result[(), WriteError] waits
fn stderr.write(err: Stderr, s: String) -> Result[(), WriteError] waits
```

No host primitives are listed yet for `Net`, `Random`, `Stdin`, `Env` and `Process`; until
they are, those capabilities have no operations.
