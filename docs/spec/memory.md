# Memory

Own values live in blocks owned by exactly one slot and are freed when their owner is
cleaned. Stable values live in regions, never move, are never written after publication,
and are freed together when their region closes. Counted values are shared, immutable
nodes freed when their last holder goes. Every allocation is admitted against a limit
first ([budgets](budgets.md)).

## Regions and stable data

### MEM-1: Regions

`region r { body }` opens a region and binds `r`, a value of type `Region`, for the body.
The region's storage is charged to the current limit as it is allocated
([T5.2](ownership.md#own-26-the-transition-table)). `Region` is scoped
([OWN-19](ownership.md#own-19-scoped-values)) and anchored to itself: it can be passed as a
parameter, never stored or returned. There is no ambient region.

```hirsel
fn count_words(src: String) -> Result[u64, Capacity] {
  region r { let words = split_words(src, r)?; Ok(span.len(words)) }
}
```
```hirsel
fn leak(src: String) -> Region { region r { r } }          // refused: MEM-7
```

### MEM-2: Placement

A stable value has a type written `T in r`, where `r` is a `Region` value in scope or a
region parameter (MEM-6). It is created only by a builder (MEM-3, MEM-4), a literal placed
with `in r`, or a function that takes `r` and returns `T in r`. A placed literal `T(..) in r`
of a stable type that is not compact evaluates its fields left to right, admits one block in
`r` and publishes the value; its type is `Result[T in r, Capacity]`
([T1.2](ownership.md#own-26-the-transition-table)).

```hirsel
stable type Span2 { start: u64, end: u64 }
fn mk(r: Region) -> Result[Span2 in r, Capacity] { Span2(start: 0, end: 1) in r }
```
```hirsel
fn mk() -> Span2 { Span2(start: 0, end: 1) }                 // refused: MEM-2, no region
```

### MEM-3: Builders

A `Builder[T]` builds a stable value of type `T` in one region. It is opaque and affine:
it exposes no view of its contents and cannot be copied. Each append may allocate in the
region and may fail with `Capacity`; a failed append returns its argument in the error and
leaves the builder unchanged ([T1.6](ownership.md#own-26-the-transition-table)). When a
builder outgrows its current allocation it continues in a new, larger one; the bytes it
abandons stay charged until the region closes ([FAIL-6](failure.md#fail-6-region-prefixes)).
`finish(take b) -> T in r` publishes and cannot fail or allocate. Dropping an unfinished
builder publishes nothing.

```hirsel
fn digits(n: u64, r: Region) -> Result[SeqSpan[u8] in r, Capacity] {
  let mut b = span.builder(r)
  let mut k = n
  loop { span.push(mut b, int.narrow(k % 10))?; k = k / 10; if k == 0 { break } }
  Ok(span.finish(take b))
}
```

### MEM-4: Sized destinations

`build_in(r, bytes n, writer)` admits one allocation of `n` bytes in `r`, then calls the
scoped callback `writer` with a private destination into which nested constructions are
written in place; it returns `Result[Builder[T], BuildError]`, where `BuildError` is
`Capacity` or the destination overflowing `n`. `prepare_in(r, staging)` admits an
allocation of the staging value's exact size and copies it; it returns the same type.
Neither runs `finish`. Bytes of an unused or failed destination stay charged until the
region closes. A destination cannot be stored, returned or passed to `finish`.

```hirsel
fn doc(r: Region, n: u64) -> Result[Doc in r, BuildError] {
  let b = build_in(r, bytes 4096, fn(d: Dest[Doc]) -> Result[(), BuildError] { write_doc(mut d, n) })?
  Ok(finish(take b))
}
```

### MEM-5: Publication

A published stable value is immutable for the life of its region and never moves. No
operation writes it, and there are no cells. Reading a stable value is reading a view of
it ([OWN-25](ownership.md#own-25-stable-values-are-read-as-views)); `let` copies its locator,
never its bytes.

```hirsel
fn f(r: Region, t: Text in r) -> u64 { text.len(t) }
```
```hirsel
fn f(r: Region, mut t: Text in r) {}                       // refused: MEM-5, no mut of stable
```

### MEM-6: Roots

A stable type's stable fields are in the region of the value that holds them, unless the
declaration names a region parameter: `stable type Token[in src] { text: Text in src }`,
used as `Token[in s] in r`. A region `a` **outlives** `r` when `a` is opened in a block that
encloses `r`'s block, when `a` is a parameter and `r` is opened in the body, or when the
signature declares `where a: r`. Building `Token[in s] in r` requires `s` to outlive `r`.
A published `T in a` is usable as `T in r` when `a` outlives `r`. Each check reads the
lexical nesting and the signature only.

```hirsel
stable type Token[in src] { text: Text in src, line: u32 }
fn tok(s: Region, t: Text in s) -> Result[u64, Capacity] {
  region r { let k = (Token(text: t, line: 1) in r)?; Ok(text.len(k.text)) }
}
```
```hirsel
fn tok(t: Text) -> Result[(), Capacity] {
  region r { let x = text.copy_in(r, t)?; region s { let k = (Token(text: x, line: 1) in s)? }; Ok(()) }
}                                         // accepted: r outlives s; the reverse is refused: MEM-6
```

### MEM-7: Region escape

A value whose type mentions a region `r` opened by a block may not leave that block: not
as its result, not into a slot declared outside it, not into an escaping closure, a send
or a spawn. A function's result may mention only its region parameters. These are checks
on types; nothing is traversed.

```hirsel
fn words(src: String) -> Result[u64, Capacity] { region r { let w = split_words(src, r)?; Ok(span.len(w)) } }
```
```hirsel
fn words(src: String) -> Result[SeqSpan[Text], Capacity] {
  region r { split_words(src, r) }                        // refused: MEM-7
}
```

### MEM-8: Close

A region closes on every exit edge of its block: after the block's views end and its owners
are cleaned, all its storage is freed and refunded at once
([T5.3](ownership.md#own-26-the-transition-table)). Closing cannot fail. A region cannot be
reset or reopened; `loop { region r { .. } }` opens a fresh region each iteration.

## Compact stable layout

### MEM-9: Compact types

`@compact(W) stable type T` declares a compact layout, where `W` is `u16`, `u32` or `u64`.
A compact value is one contiguous allocation (a **blob**) of at most `2^W − 1` bytes, laid
out with fields in declaration order, each at the next byte with no padding. Scalars keep
their written width. A variant tag uses the smallest of `u8`, `u16` and `u32` that holds
the variant count. A compact type's fields are scalars, `bool`, `char`, other compact
types (inline), `Text`, the spans of MEM-12, and `Link[U]` (MEM-10). Compact values are
built only through MEM-3 and MEM-4. Compact layout is the only layout a program can
declare in v1; every other layout is private ([MEM-13](#mem-13-own-layout)).

```hirsel
@compact(u32) stable type Entry { id: u64, name: Text, tags: SeqSpan[Text] }
```
```hirsel
@compact(u32) stable type Bad { owner: String }             // refused: MEM-9, own field
```

### MEM-10: Inline fields and links

A compact field of compact type, `Text` or span type is stored inline: its bytes are part
of the blob. A `Link[U]` field refers to a `U` earlier in the same blob and is stored as
the `W`-width byte distance back from the field to its target; `Option[Link[U]]` uses zero
for `None`. Links are formed only from the tokens a destination returns for completed
children, and a child is always completed before its parent refers to it, so every
distance is positive and in bounds. Reading a link field through a view of its holder
yields a view of the target, at the cost of one load and one subtraction with no check; a
link is never a value apart from its field, and no integer converts to one. Edges to other
blobs or regions are ordinary stable fields of a non-compact type.

```hirsel
@compact(u32) stable type Expr = Num(i64) | Add(Link[Expr], Link[Expr])
fn eval(e: Expr) -> i64 { match e { Num(n) => n, Add(a, b) => eval(a) + eval(b) } }
```
```hirsel
fn keep(e: Expr) -> Link[Expr] { match e { Add(a, _) => a, _ => fail("leaf") } }
                                              // refused: MEM-10, a link is not a value
```

### MEM-11: Active-size variants

A compact variant value occupies its tag plus its active payload only. Replacing the
variant of a published value is impossible (MEM-5); a new value is built instead.

### MEM-12: Stable sequences

There are three stable sequence types. `FixedSpan[T]` holds elements of one fixed size and
supports `span.get(s, i)` with one bounds test. `SeqSpan[T]` stores a count and a byte
extent, then the elements; it supports iteration and length only, never `s[i]`.
`IndexedSpan[T]` adds an offset table with one `W`-width entry per element and supports
`span.get`. `Text` is a `SeqSpan[u8]` of valid UTF-8 ([SEM-3](semantics.md#sem-3-text)).
Skipping a nested sequence reads its stored extent and costs constant work.

```hirsel
fn total(xs: FixedSpan[u64]) -> u64 { let mut t: u64 = 0; for x in xs { t += x }; t }
```
```hirsel
fn second(xs: SeqSpan[Text]) -> Text { xs[1] }               // refused: MEM-12
```

## Own and counted data

### MEM-13: Own layout

Own types are capacity-based: a record holds its fields inline, a variant reserves its
largest payload, `Box[T]` holds one block made by `box.new(take x) -> Result[Box[T], (Capacity,
T)]`, and `List`, `String`, `Bytes` and `Map` hold a block with a length and a
capacity. Growing a collection may move its block, which is safe because a view freezes
it ([OWN-10](ownership.md#own-10-freezes)). Field order, padding, alignment and the native
calling convention are private to the implementation and may differ between engines;
nothing in a program can observe them. A type that contains itself other than through
`Box`, `List`, `Map`, `Rc`, `Arc` or a pool is refused.

```hirsel
type Tree { label: String, kids: List[Tree] }
```
```hirsel
type Chain = End | Link(u64, Chain)                       // refused: MEM-13, use Box[Chain]
```

### MEM-14: Rc

`rc.new(take x) -> Result[Rc[T], (Capacity, T)]` requires `T: shareable` and moves the
complete owner `x` into a new counted node with count 1
([T1.3](ownership.md#own-26-the-transition-table)). An `Rc[T]` is immutable and task-local:
it is never `send`. A counted value is read where a view of its payload is expected.
Because a node is made only from a complete owner, counted values form no cycles; a
cyclic structure uses branded handles (MEM-18).

```hirsel
type Node: shareable { label: String, next: Option[Rc[Node]] }
fn cons(take l: String, take tail: Option[Rc[Node]]) -> Result[Rc[Node], (Capacity, Node)] {
  rc.new(take Node(label: take l, next: take tail))
}
```
```hirsel
fn wrap(take f: File) -> Rc[File] { .. }                       // refused: OWN-4, not shareable
```

### MEM-15: Arc

`arc.new(take x) -> Result[Arc[T], (Capacity, T)]` requires `T: shareable + send`. An
`Arc[T]` is immutable and `send`. Its creation, every share, every release and every
`try_unwrap` is an event in the run's event order ([CONC-10](tasks.md#conc-10-event-order)),
so the count each operation observes is the count that order gives.

### MEM-16: Counting

`share(h)` adds a holder and cannot fail: counts are 64-bit and cannot overflow, because
every holder occupies storage. Dropping a holder decrements the count; at zero the payload
is released with a worklist that uses constant stack, allocates nothing and cannot fail,
and each node is refunded as it is freed ([T4](ownership.md#own-26-the-transition-table)).
No user code runs during a release because shareable payloads hold no resources.
`try_unwrap(take h) -> Result[T, Rc[T]]` returns the payload without copying when the
count is 1, and otherwise returns `h` unchanged.

```hirsel
fn own_copy(take h: Rc[Node]) -> Result[Node, Capacity] {
  match rc.try_unwrap(take h) { Ok(take n) => Ok(n), Error(take h) => clone(h) }
}
```

### MEM-17: clone

`clone(x) -> Result[U, Capacity]` deep-copies the own parts of `x` into new blocks charged
to the current limit and shares its counted edges. `clone_in(r, x) -> Result[U in r,
Capacity]` copies into region `r`. The result type `U` is the expected type: it is `x`'s
type, or a **twin**, a type with the same field names in the same order whose field types
are identical or twins (`String` and `Text`; `List[T]` and `FixedSpan[T]` or
`SeqSpan[T]`; user types by this rule). A stable target cannot hold counted fields. A
failure at node k drops the copied nodes and releases the shared edges in reverse, each
refunded, and leaves `x` untouched ([T1.5](ownership.md#own-26-the-transition-table)).
Resources are never cloned (OWN-4).

```hirsel
type Row { name: String, n: u64 }
stable type RowS { name: Text, n: u64 }
fn freeze(r: Region, x: Row) -> Result[RowS in r, Capacity] { clone_in(r, x) }
```
```hirsel
stable type RowC { name: Rc[Node], n: u64 }                  // refused: MEM-17 (and MEM-9)
```

## Brands and pools

### MEM-18: Brands

A type may carry a **brand**, `T of p`, naming a slot `p` in scope, or a sibling field as
`of .f` in a field declaration. A value of type `T of p` is accepted only where `of p` with
the same slot is expected. Moving `p`, assigning `p` or ending `p`'s scope ends every value
branded by it: a later use is refused. Branded values can be created only by functions
whose signature returns `of` one of their parameters. Brands have no run-time
representation.

```hirsel
type Lru { pool: Pool[Entry], head: Option[Id[Entry] of .pool] }
fn top(l: Lru) -> Option[Id[Entry] of l.pool] { l.head }
```
```hirsel
fn mix(a: Pool[u64], b: Pool[u64], i: Id[u64] of a) -> u64 { pool.get_or(b, i, 0) }
                                                             // refused: MEM-18, i is of a
```

### MEM-19: What a pool library may rely on

A library type with private fields (`pub opaque`, [PROG-2](programs.md#prog-2-visibility))
may rely on: its fields being invisible and unconstructible outside its module; brands
(MEM-18); checked 64-bit arithmetic; checked indexing of its own collections; and its own
`resource` cleanup (MEM-20). It may rely on no layout fact. A generation-checked pool is
library code built from these: a slot array whose slots hold a 64-bit generation, an
`Id[T] of p` holding an index and a generation, and lookups that compare both. A
generation at its maximum retires its slot instead of wrapping.

### MEM-20: Branded resource cleanup

A resource type may be branded by its owner, `resource type SlotOwner[T] of p`, and declare
a finalizer that receives `mut p`. Its implicit drop is then a `mut` use of `p` at its
cleanup point: the definition is refused if `p` is frozen, lent, moved or out of scope at
any point where the resource may be cleaned. Moving or dropping `p` while a resource branded
by it is live is refused.

```hirsel
fn use_slot(mut p: Pool[String]) -> Result[(), Capacity] {
  let so = pool.insert(mut p, string.from("a"))?   // so: SlotOwner[String] of p
  Ok(())                                           // so's finalizer removes the entry
}
```
```hirsel
fn bad(mut p: Pool[String]) -> Result[(), Capacity] {
  let so = pool.insert(mut p, string.from("a"))?
  let v = view p                                   // refused: MEM-20, p frozen at so's cleanup
  Ok(())
}
```
