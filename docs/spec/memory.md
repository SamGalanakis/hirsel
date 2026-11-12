# Memory

## Regions and stable data

### MEM-1: Regions

`region r { body }` opens a region and binds `r`, a value of type `Region`, for the body.
The region's storage is charged to the current limit as it is allocated
([T5.2](ownership.md#own-26-the-transition-table)). `Region` is scoped
([OWN-19](ownership.md#own-19-scoped-values)) and anchored to itself: it can be passed as a
parameter, never stored or returned. There is no ambient region.

```hirsel
fn width(s: String) -> Result[u64, Capacity] {
  region r { let t = text.copy_in(r, s)?; Ok(text.len(t)) }
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
([T1.2](ownership.md#own-26-the-transition-table)). A stable type's fields are plain, or
stable (and `Option` and tuples of these) whose regions outlive the value's (MEM-6); a field
whose type is a type parameter needs the bound `plain`. Own, counted, resource and scoped
fields are refused, at every depth; a plain capability may be held, as in any plain value.

```hirsel
stable type Span2 { start: u64, end: u64 }
fn mk(r: Region) -> Result[Span2 in r, Capacity] { Span2(start: 0, end: 1) in r }
```
```hirsel
stable type Bad { name: String }                            // refused: MEM-2, own field
```

### MEM-3: Builders

Two builder types exist, each naming its element or result type and its region.
`span.builder(r)` makes a `SpanBuilder[T, in r]`, which builds a `FixedSpan[T] in r` (MEM-12)
of a plain `T`; `span.with_capacity(r, n)` makes one with room for `n` elements, admitted at
once ([core](core.md)):
`span.push(mut b, x: T)` appends one element and may fail with `Capacity`, returning its
argument and leaving the builder unchanged ([T1.6](ownership.md#own-26-the-transition-table));
`span.finish(take b)` publishes it. A `Builder[T, in r]`, made by `build_in`
(MEM-4), holds one finished compact blob; `finish(take b)` publishes it as a `T in r`. Both
builders are own, affine and not `send`, expose no view of their contents, and cannot leave
the block that opened `r` (MEM-7). Neither `finish` can fail or allocate. When a span
builder outgrows its current allocation it continues in a new, larger one; the bytes it
abandons stay charged until the region closes ([FAIL-6](failure.md#fail-6-region-prefixes)).
`span.builder` admits nothing until the first push; finishing a builder that holds no element publishes the
empty span, which allocates nothing. Dropping an unfinished builder publishes nothing.

```hirsel
fn digits(n: u64, r: Region) -> Result[FixedSpan[u8] in r, (Capacity, u8)] {
  let mut b: SpanBuilder[u8, in r] = span.builder(r)
  let mut k = n
  loop { span.push(mut b, int.narrow(k % 10))?; k = k / 10; if k == 0 { break } }
  Ok(span.finish(take b))
}
```
```hirsel
fn names(r: Region, s: String) -> Result[FixedSpan[String] in r, (Capacity, String)] {
  let mut b = span.builder(r)                                // refused: MEM-3, String is not plain
  span.push(mut b, s)?
  Ok(span.finish(take b))
}
```

### MEM-4: Sized destinations

```
fn build_in[T](r: Region, bytes n: u64,
               w: scoped fn(mut Dest[T]) -> Result[Child[T], BuildError])
    -> Result[Builder[T, in r], BuildError]
type BuildError = Capacity(Capacity) | Overflow
```

`build_in` requires a compact `T` and fixes the destination's `W` to `T`'s. It admits one
allocation of `n` bytes in `r`, then calls the writer `w` once with a new destination
`d`. Every destination has an identity, a number no other destination of
the run has. `dest.put(mut d, L)`, where `L` is a construction literal of a compact type
`U`, is a built-in form of type `Result[Child[U], BuildError]`: it writes `L` completely at
the destination's next byte. A `Child[U]` is plain data: the destination's identity and the
offset of a complete `U`. In `L`, a `Link[V]` field takes a `Child[V]`; an inline `Text` or
span field takes a view of a `String` or `List`, whose contents are copied; an inline compact
field takes a nested literal. Every put, a separately put child of another compact type included, is encoded with the
destination's `W`, whatever the literal's own declaration says. A put evaluates `L`'s
fields left to right without writing, then checks each `Link` child's identity in
depth-first field order, then the size and width, and only then writes. A child with
another destination's identity, and a writer that returns such a root, fail with `Bounds`;
this check is one comparison per link and comes before the size check. A put that does not
fit in what is left of `n`, or one in which a count, a link distance or the used prefix
would exceed `2^b − 1` for that `W` (MEM-9), writes nothing, returns `Error(Overflow)` and
leaves the destination usable for a smaller put. The `Child[T]` that `w` returns is the root: the builder holds the
destination's whole used prefix as the blob, and the reference that `finish` publishes
records the root's offset in it. A published compact value is copied into another region by
`clone_in` (MEM-17). Nothing in a destination assumes a fixed element stride. Bytes of an unused or failed destination stay charged until the region closes.
`Dest` is scoped and moves (OWN-3), so a destination cannot be stored, returned or passed
to `finish`.

```hirsel
@compact(u32) stable type Note { id: u64, body: Text }
fn note(r: Region, s: String) -> Result[Note in r, BuildError] {
  let b = build_in(r, bytes 64, fn(mut d: Dest[Note]) -> Result[Child[Note], BuildError] {
    dest.put(mut d, Note(id: 1, body: s))
  })?
  Ok(finish(take b))
}
```

A document of a `streamable` type (MEM-12) is instead written top-down, each value once at
its final offset:

```
fn build_stream_in[T: streamable](r: Region, bytes n: u64,
                   w: scoped fn(take Writer[T, in r]) -> Result[Child[T], BuildError])
    -> Result[Builder[T, in r], BuildError]
```

It returns `Error(Overflow)` when `n` exceeds the blob cap of `T`'s `W`, then admits `n`
bytes as `build_in` does, and only then calls `w` once. A `Writer[U, in r]` is the right to
write one complete `U` at the allocation's next byte. It is scoped and moves (OWN-3), has
no size and no cleanup, and is never captured by any closure, stored, returned or held in
another value; it passes only to a parameter that declares a writer. It is completed in one of two ways, each giving the `Child[U]` of the complete value:

- `emit(take w, L)` writes the construction literal `L` of `U` in declaration order. A
  field's operand in `L` may be `emit(f)`, where `f` is a scoped callback that takes the
  field's writer and returns its child; `f` runs, and its captures are lent, only while
  that field is written ([OWN-21](ownership.md#own-21-scoped-callbacks)).
- `writer.close(take w)` completes a `Text`, `SeqSpan` or `IndexedSpan`, whose header was
  reserved when its writer was made. Closing an `IndexedSpan` checks that its table fits,
  then writes it in one walk of the elements.

Before closing, `writer.scalar(mut w, c)` appends a `char` to a `Text`, and
`writer.append(mut w, f)` calls `f` once with a writer for the sequence's next element; `w`
is lent to that call alone, and the count advances only when `f` returns the element's
child. `writer.completed(w, i)`, which walks `i` elements, and `writer.last_completed(w)`, in
constant time, return views anchored to `r` of complete elements; nothing views an open
value. Each operation that receives a `Child` checks that its identity and offset are
those of the frame it handed out, and fails with `Bounds` otherwise; this guards against a
program mixing children, not against the engine's own writes. A write that does not fit in
`n`, or that would make a count, byte length, offset or the used prefix exceed `2^b − 1`,
returns `Error(Overflow)` before its first byte; as for a put, the identity check comes
first.

Every call that takes a writer and returns a `Result` is the operand of `?` or in tail
position ([LANG-11](language.md#lang-11-typing-is-local)); its result is never bound, matched
or dropped. So an `Error` leaves the whole build: nothing is published, no abandoned frame
resumes, the `n` bytes stay charged until `r` closes, and views of complete elements stay
valid ([FAIL-6](failure.md#fail-6-region-prefixes)). Nothing infers `n`, runs `w` twice,
moves or grows a written byte, or admits in `finish`; a program that wants the exact size
computes it first from input it can read twice ([example 8](examples.md#8-a-json-document-written-in-place)).
`finish_trim(take b)`, for either builder, publishes as `finish` does and refunds the
unused end, `round8(n) − round8(used)`, to the limit the allocation's record leads to
([MEM-20](budgets.md#mem-20-limit-records-and-hand-over)), whatever limit is current; it
moves nothing and cannot fail.

```hirsel
@compact(u32) stable type J = Null | Arr(SeqSpan[J])
fn list[in r](take w: Writer[SeqSpan[J], in r]) -> Result[Child[SeqSpan[J]], BuildError] {
  writer.append(mut w, fn(take x: Writer[J, in r]) -> Result[Child[J], BuildError] { emit(take x, Null) })?
  writer.close(take w)
}
```
```hirsel
@compact(u32) stable type J = Null | Arr(SeqSpan[J])
fn quiet(f: scoped fn() -> Result[(), BuildError]) -> Result[(), BuildError] {
  match f() { Ok(()) => Ok(()), Error(_) => Ok(()) }
}
fn list[in r](take w: Writer[SeqSpan[J], in r]) -> Result[Child[SeqSpan[J]], BuildError] {
  quiet(fn() -> Result[(), BuildError] {
    writer.append(mut w, fn(take x: Writer[J, in r]) -> Result[Child[J], BuildError] { emit(take x, Null) })?
    Ok(())                                     // refused: MEM-4, the closure captures w
  })?
  writer.close(take w)
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
used as `Token[in s] in r`. An own or resource type with a stable field names a region
parameter for it (`type Holder[in r] { t: Text in r }`). A parameter written with a bare
stable type (`t: Text`) accepts a value in any region that outlives the call; a result or
field with a stable type names its region. A function's region parameters are its `Region`
parameters and its generics written `in a`, which the argument types fix
(`fn finish[T, in r](take b: Builder[T, in r]) -> T in r`).

A region `a` **outlives** `r` when `a` is `r`, when `a` is opened in a block that encloses
`r`'s block, when `a` is a parameter and `r` is opened in the body, when the signature
declares `where a: r`, or by transitivity. Building `Token[in s] in r` requires `s` to
outlive `r`. A published `T in a` is usable as `T in r` when `a` outlives `r`, and the
same holds inside `Option`, tuples and stable type arguments. Each check reads the lexical
nesting and the signature only.

```hirsel
stable type Token[in src] { text: Text in src, line: u32 }
fn tok(s: Region, t: Text in s) -> Result[u64, Capacity] {
  region r { let k = (Token(text: t, line: 1) in r)?; Ok(text.len(k.text)) }
}
```
```hirsel
stable type Token[in src] { text: Text in src, line: u32 }
type Hidden { text: Text }                                   // refused: MEM-6, no region
fn tok(t: String) -> Result[(), Capacity] {
  region outer {
    region inner { let x = text.copy_in(inner, t)?; let k = (Token(text: x, line: 1) in outer)? }
    Ok(())                                     // refused: MEM-6, inner does not outlive outer
  }
}
```

### MEM-7: Region escape

A value whose type mentions a region `r` opened by a block may not leave that block: not
as its result, not into a slot declared outside it, not into an escaping closure, a send
or a spawn. A function's result may mention only its region parameters. These are checks
on types; nothing is traversed.

```hirsel
fn width(s: String) -> Result[u64, Capacity] { region r { let t = text.copy_in(r, s)?; Ok(text.len(t)) } }
```
```hirsel
fn leak(s: String) -> Result[Text, Capacity] {
  region r { text.copy_in(r, s) }                           // refused: MEM-7
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
A compact value is one contiguous allocation (a **blob**) of at most `2^b − 1` bytes, where
`b` is `W`'s bit width, laid out with fields in declaration order, each at the next byte
with no padding. A compact type's fields are scalars, `bool`, `char`, other compact types
(inline), `Text`, `FixedSpan[T]`, `SeqSpan[T]`, `IndexedSpan[T]` (MEM-12), and `Link[U]`
(MEM-10); an `Inline` array is not one in v1. A compact type held inline or as a link
target is encoded with its holder's `W`, and a view of it, however it is projected, passed,
returned or copied, keeps that `W`, its blob and its offset in the blob; a `Text` or
sequence that is not inside a compact type is encoded with `W = u64`. Every compact type's smallest encoding is at least
one byte; a type that can encode to zero bytes is refused. Compact values are built only
through MEM-3, MEM-4 and MEM-17. Compact layout is the only layout a program can declare in v1;
every other layout is private ([MEM-13](#mem-13-own-layout)).

The language fixes, and an engine's backend may rely on: scalars little-endian at their
written width; `bool` one byte, 0 or 1; `char` its scalar value as a `u32`; floats their
IEEE bits, little-endian; a variant tag, numbered from 0 in declaration order, in the
smallest of `u8`, `u16` and `u32` that holds the variant count; a field's offset is the
sum of the sizes of the fields before it; the count, byte-length and offset words of MEM-12, `W` wide; the
link encoding of MEM-10; and the blob cap. Private: where a blob sits in its region and its
alignment, region chunking, every non-compact layout and the calling convention.

```hirsel
@compact(u32) stable type Entry { id: u64, name: Text, scores: FixedSpan[u32] }
```
```hirsel
@compact(u32) stable type Bad { owner: String }             // refused: MEM-9, own field
@compact(u16) stable type Empty {}                          // refused: MEM-9, zero bytes
@compact(u16) stable type Packed { data: Inline[u8, 4] }    // refused: MEM-9, an Inline field
```

### MEM-10: Inline fields and links

A compact field of compact type, `Text` or a sequence type is stored inline: its bytes are part
of the blob. A `Link[U]` field refers to a `U` earlier in the same blob and is stored as
the `W`-width byte distance back from the field to its target; `Option[Link[U]]` uses zero
for `None`. Links are formed only from the `Child` values of MEM-4; a child is complete
before its parent is put, and every encoding is at least one byte, so every distance is at
least 1 and in bounds. Reading a link field through a view of its holder yields a view of
the target, at the cost of one load and one subtraction with no check; a link is never a
value apart from its field, and no integer converts to one. A blob is moved or copied only
whole (MEM-4, MEM-17), so its links stay valid. Edges to other blobs or regions are ordinary
stable fields of a non-compact type.

```hirsel
@compact(u32) stable type Expr = Num(i64) | Add(Link[Expr], Link[Expr])
fn eval(e: Expr) -> i64 { match e { Num(n) => n, Add(a, b) => eval(a) + eval(b) } }
```
```hirsel
@compact(u32) stable type Expr = Num(i64) | Add(Link[Expr], Link[Expr])
fn keep(e: Expr) -> Link[Expr] { match e { Add(a, _) => a, _ => fail("leaf") } }
                                              // refused: MEM-10, a link is not a value
```

### MEM-11: Active-size variants

A compact variant value occupies its tag plus its active payload only. Replacing the
variant of a published value is impossible (MEM-5); a new value is built instead.

### MEM-12: Stable sequences

`FixedSpan[T]` holds elements of a plain type `T`. It is a `W`-width count, then the
elements at a stride of `T`'s logical size; an element's bytes are the encoding of MEM-9
when `T` is a scalar, `bool`, `char`, or a tuple or record of these, and private otherwise.
`Text` is a `W`-width byte count, then that many bytes of valid UTF-8
([SEM-3](semantics.md#sem-3-text)). `span.get(s, i)` returns `Option[T]` after one bounds
test; `s[i]` fails with `Bounds` instead.

`SeqSpan[T]` holds elements of a `packed` type, each of its own size: a `W`-width count, a
`W`-width **byte length** (of the elements), then the elements back to back in order.
`IndexedSpan[T]` is the same followed by a table of `count` offsets, each `W` wide, from the
first element byte to each element; its own byte length excludes the table, and the byte
length of a sequence holding it includes it. So an `IndexedSpan[T]` is read where a `SeqSpan[T]` is
expected, and skipping any value costs work fixed by its type, never by its contents.
`seq.len(s)` reads the count, `seq.nth(s, i)` walks `i` elements, `indexed.get(s, i)` makes
one bounds test and one load, and `for` visits the elements in order
([SEM-6](semantics.md#sem-6-iteration)); neither has `s[i]`. They are written only by the
writers of MEM-4. `seq.index_in(r, s)` builds a separate `SeqIndex` of `s` in `r`, read in
constant time by `seq.get(x, i)`; it keeps `s`'s locator, and nothing makes one from
offsets or pairs it with another sequence.

A type is **packed** when it is a scalar, `bool`, `char`, `Text`, a compact type, or a
`SeqSpan` or `IndexedSpan` of a packed type, its holds set
([OWN-2](ownership.md#own-2-type-summaries)) has no capability, and each `FixedSpan` in it
has elements with MEM-9's encoding. It is **streamable** when it is packed and holds no
`Link` at any depth. Both are bounds (OWN-4) that summaries compute; neither is declared. A
compact type may contain itself through a sequence, whose header ends its layout (OWN-2),
when every such cycle keeps its type arguments; inline it still may not.

```hirsel
@compact(u32) stable type J = Null | Arr(SeqSpan[J])
fn count(j: J) -> u64 { match j { Null => 1, Arr(xs) => { let mut n: u64 = 1; for x in xs { n += count(x) }; n } } }
```
```hirsel
fn names(xs: FixedSpan[Text]) -> u64 { span.len(xs) }       // refused: MEM-12, Text has no fixed size
fn first(xs: SeqSpan[u64]) -> u64 { xs[0] }                 // refused: MEM-12, no s[i]
```

## Own and counted data

### MEM-13: Own layout

Own types are capacity-based: a record holds its fields inline, a variant reserves room for
its largest payload, `Box[T]` holds one block made by `box.new(take x)`, and `List`,
`String` and `Map` hold a block with a length and a capacity. Growing a collection may move
its block, which is safe because a view freezes it
([OWN-10](ownership.md#own-10-freezes)). Field order, padding, alignment and the native
calling convention, including how several scalar results are returned, are private to the
implementation and may differ between engines; nothing in a program can observe them. A
type that contains itself inline is refused by
[OWN-2](ownership.md#own-2-type-summaries).

```hirsel
type Tree { label: String, kids: List[Tree] }
```
```hirsel
type Chain = End | Link(u64, Chain)                       // refused: OWN-2, use Box[Chain]
```

### MEM-14: Rc

`rc.new(take x) -> Result[Rc[T], (Capacity, T)]` requires `T: shareable` and moves the
complete owner `x` into a new counted node with count 1
([T1.3](ownership.md#own-26-the-transition-table)). An `Rc[T]` is immutable and task-local:
it is never `send`. A counted value is read where a view of its payload is expected.
Because a node is made only from a complete owner, counted values form no cycles; a
cyclic structure uses a library pool (MEM-18). Wrapping a value in `Rc` or `Arc` frees nothing it
owned.

```hirsel
type Node: shareable { label: String, next: Option[Rc[Node]] }
fn cons(take l: String, take tail: Option[Rc[Node]]) -> Result[Rc[Node], (Capacity, Node)] {
  rc.new(Node(label: take l, next: take tail))
}
```
```hirsel
fn wrap(take f: HostFile) -> Result[Rc[HostFile], (Capacity, HostFile)] { rc.new(take f) }
                                                             // refused: OWN-4, not shareable
```

### MEM-15: Arc

`arc.new(take x) -> Result[Arc[T], (Capacity, T)]` requires `T: shareable + send`. An
`Arc[T]` is immutable and `send`. Its creation, every share, every release and every
`try_unwrap` is an event in the run's event order ([CONC-10](tasks.md#conc-10-event-order)),
so the count each operation observes is the count that order gives.

### MEM-16: Counting

`share(h)` adds a holder. Counts are 64-bit. A count never wraps and never becomes
permanent: an increment past `2^64 − 1` is a run stop
([FAIL-15](failure.md#fail-15-run-stops)), so `share` returns no error. Dropping a holder
decrements the count; at zero the payload is released with a worklist that uses constant
stack, allocates nothing and cannot fail, and each node is refunded as it is freed
([T4](ownership.md#own-26-the-transition-table)). No user code runs during a release
because shareable payloads hold no resources. `try_unwrap(take h) -> Result[T, Rc[T]]`
returns the payload without copying when the count is 1, and otherwise returns `h`
unchanged.

```hirsel
type Node: shareable { label: String }
fn own_copy(take h: Rc[Node]) -> Result[Node, Capacity] {
  match rc.try_unwrap(take h) { Ok(take n) => Ok(take n), Error(take g) => clone(g) }
}
```

### MEM-17: clone

`clone(x)` and `clone_in(r, x)` are built-in typing forms. The expected type selects
their result `U` and what is copied: first `x`'s own type, if it is `U` or `U`'s declared
**twin**; otherwise the payloads through `Box`, `Rc` and `Arc`, outermost first, the first
that is `U` or its twin. No match is refused. `clone(x) -> Result[U, Capacity]` makes a
plain, own or counted `U`: it deep-copies the own parts of the selected value into new
blocks charged to the current limit and shares its counted edges, a counted root selected
as `U` included, and keeps the regions of its stable fields. `clone_in(r, x) -> Result[U
in r, Capacity]` makes a stable `U` in `r`; a compact value is copied as the whole used
blob at its original `W`, keeping the selected value's offset, never re-encoded at `U`'s
own `W`. `copy_closed_in(r, x)` copies a value of a streamable type (MEM-12) as exactly its
own bytes, keeping its `W`, even from a blob still being written (MEM-4); it admits those
bytes once and leaves `x` as it was.
`@twin(P)` on a non-generic declaration names the type `P` it pairs with; the pair is
symmetric. The two have the same field names in the same order, and each pair of field
types is identical, a built-in pair (`String` and `Text`; `List[T]` and `FixedSpan[T]`;
`Box[X]` and a stable reference to `Y` where `X` and `Y` are twins), a declared twin pair, or
`Option`, `Result` or a tuple of such pairs, component by component. Two variant types pair
when their arms have the same names in the same order and each pair of arm payloads pairs
by the same rule. While a declaration's
fields are checked, its own pair is assumed, so recursive twins check. A stable target
cannot hold counted fields. Each copied `List`, `String` or `Map` gets capacity equal to its
length. The copy is depth-first: a record's fields in declaration order, a variant's active
payload, a list's elements in index order and a map's entries in insertion order (key, then
value), and each block is admitted when the walk reaches it, before what it holds. A value
whose type summary holds a resource, a scoped value or an opaque owner
([OWN-2](ownership.md#own-2-type-summaries)) is never cloned and is refused here, at the
form; plain callables and capabilities copy as plain values. Failure is
[T1.5](ownership.md#own-26-the-transition-table).

```hirsel
type Row { name: String, n: u64 }
@twin(Row) stable type RowS { name: Text, n: u64 }
fn freeze(r: Region, x: Row) -> Result[RowS in r, Capacity] { clone_in(r, x) }
```
```hirsel
type Row { name: String, n: u64 }
@twin(Row) stable type RowN { label: Text, n: u64 }          // refused: MEM-17, field names
```
```hirsel
fn dup(r: Region, b: SpanBuilder[u8, in r]) -> Result[SpanBuilder[u8, in r], Capacity] { clone(b) }
                                                            // refused: MEM-17, an opaque owner
```

## Pools

### MEM-18: Pools are library code

v1 has no brands and no language rule for pools. A pool is an ordinary library type over
core operations, in the manner of a generational arena: a `pub opaque` type
([PROG-2](programs.md#prog-2-visibility)) holding a `List` of slots, each with a 64-bit
generation and an `Option` value, and an identity from `unique.new()`. A handle is plain
data: the pool's `Unique`, an index and a generation. Every access compares all three and
reports a stale handle as the library's error; removal is an explicit operation that
advances the slot's generation; a slot whose generation reaches its maximum is retired,
never wrapped; dropping the pool cleans what is still in it by ordinary cleanup
([FAIL-8](failure.md#fail-8-exit-edge-cleanup)). A `Unique` is plain, so cloning a pool
copies its identity, and a handle of the original also resolves in the copy. The core
operations a pool needs are `unique.new`, `list.with_capacity`, `list.push`, `list.get`,
`list.set`, `list.len`, `replace` and checked arithmetic. Against static brands this costs
one more comparison per access and a handle of 24 bytes instead of 16; brands that elide
the identity check are reserved ([reserved](reserved.md)).

`Unique` is an opaque plain type with the bound `key` and no other: `unique.new()` returns a
value unequal to every other `Unique` made in the run, admits nothing and cannot fail.
Equality is its only observation: it has no order, no conversion to a number or text, and
no operation reveals its hash. Which values are made first may depend on the event order,
so this is what keeps that order from reaching output or a recording.

```hirsel
type Handle: key { pool: Unique, index: u64, generation: u64 }
fn same_pool(a: Handle, b: Handle) -> bool { a.pool == b.pool }
```
```hirsel
fn older(a: Unique, b: Unique) -> bool { a < b }            // refused: SEM-7, Unique is not ordered
```
