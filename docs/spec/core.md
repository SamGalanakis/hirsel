# Core library signatures

The core operations that the rules of this specification name, with their signatures.
Each signature is normative for the effects the rules give it: what it moves, what it
allocates and how it fails. Everything else about the libraries is outside this
specification. `X?` in a comment marks the rule that governs the operation.

## Error values

```hirsel
type Capacity { requested: u64, available: u64 }               // MEM-24
type Closed[T] { item: T }                                     // CONC-5
type SendError[T] = Full(T) | Closed(T)                        // CONC-5
type JoinError = Failed(List[Cause]) | Cancelled               // CONC-6
type Cause { kind: CauseKind, site: Site }                     // FAIL-2, FAIL-10
type CauseKind = Overflow | Bounds | Division | Unwrap | Fail(String) | Cancelled | Capacity
type BuildError = Capacity(Capacity) | Overflow                // MEM-4
```

`Site` is an opaque plain type naming a source position. `JoinError`'s `Failed` list
holds the primary cause first.

## Integers and floats (SEM-1, SEM-2)

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
fn fail(msg: String) -> Never                                  // FAIL-2
```

`T` and `U` above range over the integer or float types only.

## Box, Rc and Arc (MEM-13 to MEM-16)

```hirsel
fn box.new[T](take x: T) -> Result[Box[T], (Capacity, T)]
fn box.unbox[T](take b: Box[T]) -> T
fn rc.new[T: shareable](take x: T) -> Result[Rc[T], (Capacity, T)]
fn rc.share[T: shareable](h: Rc[T]) -> Rc[T]
fn rc.try_unwrap[T: shareable](take h: Rc[T]) -> Result[T, Rc[T]]
fn arc.new[T: shareable + send](take x: T) -> Result[Arc[T], (Capacity, T)]
fn arc.share[T: shareable + send](h: Arc[T]) -> Arc[T]
fn arc.try_unwrap[T: shareable + send](take h: Arc[T]) -> Result[T, Arc[T]]
fn clone[T, U](x: T) -> Result[U, Capacity]                    // MEM-17; U is T or a twin
fn clone_in[T, U](r: Region, x: T) -> Result[U in r, Capacity] // MEM-17
```

A `Box[T]`, `Rc[T]` or `Arc[T]` is read where a view of `T` is expected.

## Lists, strings and maps (SEM-3 to SEM-5)

```hirsel
fn list.new[T]() -> List[T]                                    // allocates nothing
fn list.with_capacity[T](n: u64) -> Result[List[T], Capacity]
fn list.len[T](xs: List[T]) -> u64
fn list.get[T](xs: List[T], i: u64) -> Option[View[T]] in xs
fn list.push[T](mut xs: List[T], take x: T) -> Result[(), (Capacity, T)]
fn list.pop[T](mut xs: List[T]) -> Option[T]
fn list.set[T](mut xs: List[T], i: u64, take x: T) -> T         // returns the old element; Bounds
fn list.clear[T](mut xs: List[T])
fn list.sum[T](xs: List[T]) -> T                               // integers; Overflow
fn string.new() -> String
fn string.from(s: String) -> Result[String, Capacity]          // copies a literal or a view
fn string.copy(s: String) -> Result[String, Capacity]
fn string.byte_len(s: String) -> u64
fn string.push(mut s: String, t: String) -> Result[(), Capacity]
fn text.len(t: Text) -> u64
fn text.copy_in(r: Region, s: String) -> Result[Text in r, Capacity]
fn map.new[K: key, V]() -> Map[K, V]
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
fn build_in[T](r: Region, bytes n: u64, w: scoped fn(mut Dest[T]) -> Result[(), BuildError]) -> Result[Builder[T in r], BuildError]
fn prepare_in[T, U](r: Region, staging: U) -> Result[Builder[T in r], BuildError]
fn finish[T](take b: Builder[T]) -> T                          // MEM-3
```

## Tasks and channels (CONC-1 to CONC-9)

```hirsel
fn task.spawn[X: send, R: send](tasks: Tasks, bytes n: u64, take x: X, f: fn(take X) -> R) -> Result[Task[R], (Capacity, X)]
fn task.join[R](take h: Task[R]) -> Result[R, JoinError] waits
fn task.cancel[R](mut h: Task[R])
fn task.check_cancel()                                         // CONC-7; may fail with Cancelled
fn channel.new[T: send](cap: u64) -> Result[(Sender[T], Receiver[T]), Capacity]
fn channel.sender[T](tx: Sender[T]) -> Sender[T]
fn channel.send[T](tx: Sender[T], take x: T) -> Result[(), Closed[T]] waits
fn channel.try_send[T](tx: Sender[T], take x: T) -> Result[(), SendError[T]]
fn channel.receive[T](rx: Receiver[T]) -> Result[T, Closed[()]] waits
```

## Capabilities (CAP-1 to CAP-7)

The core capability types are `Dir`, `Net`, `Clock`, `Random`, `Stdout`, `Stderr`, `Stdin`,
`Env`, `Process` and `Tasks`. Their operations are library contracts and are not part of
this specification, except that each takes its capability as an argument, states its
allocation in its result type, and is marked `waits` when it may wait.
