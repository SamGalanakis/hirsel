# Memory

## Memory model: settled

Settled on 2026-11-18 (Sam: "The memory thing seems settled we can go with that - update the
spec and make sure we do not drift again"). The model is chosen as the clean end-state, not to
spare existing code. In five lines:

1. **The type decides who frees.** An ordinary `type` is own: one owner, freed when the owner
   goes away ([OWN-3](ownership.md#own-3-kinds)). Sharing is opt-in and counted (`Rc`, `Arc`),
   resources stay resources, loans stay loans.
2. **`@compact(W) type` declares region data** ([MEM-2](#mem-2-region-data)): a document that
   dies together lives in one region and is freed at once, with no per-node walk. It is the one
   way to declare region data; there is no `stable` keyword and no `@twin`.
3. **Construction in place where it keeps custody** ([MEM-31](#mem-31-construction-in-place),
   a performance target): a fresh value returned to the caller or put into a container slot
   is built in that destination when the destination is known at construction; otherwise it
   is built locally and moved, and a move never copies out-of-line data.
4. **Servers get a per-request scratch region**, closed when the handler returns
   ([HTTP-6](http.md#http-6-the-anchored-request-and-the-read-buffer)).
5. **Speed is the engine's, not the language's** ([MEM-32](#mem-32-engine-obligations), a
   performance target): a per-task size-class allocator, a demand-sized first region chunk,
   per-type pools inside a region, and no teardown walk for plain data. Charges are logical
   upper bounds, deterministic per build ([MEM-24](budgets.md#mem-24-logical-charges),
   [MEM-28](budgets.md#mem-28-charges-are-an-upper-bound)); a program that fits its logical
   charges fits on every engine.

Rejected, each for a reason that still holds:

- **Region or immutable data by default.** Ordinary mutation, caches and messages that expire
  one by one become exceptions; a prototype of this default charged 65× to 107× the needed
  bytes where own charged 1.12× to 7×, and MLKit's inferred regions used from 8% to over 3000%
  of the space SML/NJ's collected heap used on the same programs.
- **Build every value in its destination**, a placement read from each binding's later uses
  with an arena per block. One allocation site can need two lifetimes (a value returned on one
  path is sent to another task on another); a reference into a block's arena can outlive the
  slot cleanup of that block or be copied into another task inside plain data; and arena
  release lists break finalizer order when a return fails.
- **Region views in place of region values** (`View[T] in r` as the only way to read region
  data). A view can be no field, element or generic argument, so an own index over a region
  document (`Map[Text in r, u64]`) cannot be written, or region views become an exempt kind
  under another name.
- **A declarable `stable type` beside `@compact`, with `@twin` pairs.** Two ways to say region
  data; in core, std and the programs every declared region type was compact and no twin was
  used.

**Reopen condition.** Only a measurement reopens this: matched ports of the AWFY Json and
Storage benchmarks to an explicit document pool, in Hirsel and in Goose, with the same input,
output and checksums. If, with every target of MEM-32 met, Hirsel still runs above
1.5× Goose's instructions on either, the default is reopened. A new design opinion is not a
reason to reopen it.

## Regions and region data

### MEM-1: Regions

`region r { body }` opens a region and binds `r`, a value of type `Region`, for the body.
The region's storage is charged to the current limit as it is allocated
([T5.2](ownership.md#own-26-the-transition-table)). `Region` is scoped
([OWN-19](ownership.md#own-19-scoped-values)) and anchored to itself: it can be passed as a
parameter, never stored or returned. There is no ambient region. A `Region` parameter may
be marked `mut` or `take` as any scoped parameter may; a value installed through a `mut` one
reads only that region, its one anchor ([OWN-11](ownership.md#own-11-call-arguments)), so
no callee stores another region in its caller's slot.

```hirsel
fn width(s: String) -> u64 {
  region r { let t = text.copy_in(r, s); text.len(t) }
}
```
```hirsel
fn leak() -> u64 { let k = region r { r }; 0 }             // refused: MEM-7
```
```hirsel
fn swap(mut r: Region, s: Region) { r = s }                // refused: OWN-11, s is not an anchor of r
```

### MEM-2: Region data

Region data is exactly the values of the **region** kind
([OWN-3](ownership.md#own-3-kinds)): the types declared `@compact(W) type T` (MEM-9) and
the built-in `Text`, `FixedSpan`, `SeqSpan`, `IndexedSpan` and `SeqIndex`. A region value has
a type written `T in r`, where `r` is a `Region` value in scope or a region parameter (MEM-6).
It is created only by a builder (MEM-3, MEM-4), by a core operation that names `r` (such as
`text.copy_in(r, s)` and `seq.index_in(r, s)`), by `clone_in` (MEM-17), or by a function
that takes `r` and returns `T in r`. A construction literal of a region type is written only
inside `dest.put` and `emit` (MEM-4); anywhere else it is refused here. The fields a compact
type may hold are MEM-9's. An edge from region data to another blob or region is a region
field of an own type (MEM-6).

```hirsel
@compact(u32) type Span2 { start: u64, end: u64 }
fn mk(r: Region) -> Result[Span2 in r, BuildError] {
  let b = build_in(r, bytes 16, fn(mut d: Dest[Span2]) -> Result[Child[Span2], BuildError] {
    dest.put(mut d, Span2(start: 0, end: 1))
  })?
  Ok(finish(take b))
}
```
```hirsel
@compact(u32) type Span2 { start: u64, end: u64 }
fn mk(r: Region) -> Span2 in r { Span2(start: 0, end: 1) }   // refused: MEM-2, a literal outside a destination
```

### MEM-3: Builders

Two builder types exist, each naming its element or result type and its region.
`span.builder(r)` makes a `SpanBuilder[T, in r]`, which builds a `FixedSpan[T] in r` (MEM-12)
of a plain `T`; `span.with_capacity(r, n)` makes one with room for `n` elements, admitted at
once ([core](core.md)):
`span.push(mut b, x: T)` appends one element; a refused growth raises `Capacity`, leaving the
builder unchanged ([T1.6](ownership.md#own-26-the-transition-table));
`span.finish(take b)` publishes it. A `Builder[T, in r]`, made by `build_in`
(MEM-4), holds one finished compact blob; `finish(take b)` publishes it as a `T in r`. Both
builders are own, affine and not `send`, expose no view of their contents, and cannot leave
the block that opened `r` (MEM-7). Neither `finish` can fail or allocate. When a span
builder outgrows its current allocation it continues in a new, larger one; the bytes it
abandons stay charged until the region closes ([FAIL-6](failure.md#fail-6-region-prefixes)).
`span.builder` admits nothing until the first push; finishing a builder that holds no element publishes the
empty span, which allocates nothing. Dropping an unfinished builder publishes nothing.

```hirsel
fn digits(n: u64, r: Region) -> FixedSpan[u8] in r {
  let mut b: SpanBuilder[u8, in r] = span.builder(r)
  let mut k = n
  loop { span.push(mut b, int.narrow(k % 10)); k = k / 10; if k == 0 { break } }
  span.finish(take b)
}
```
```hirsel
fn names(r: Region) -> FixedSpan[String] in r {  // refused: MEM-12, String is not plain
  let mut b: SpanBuilder[String, in r] = span.builder(r)  // refused: LANG-12, T: plain
  span.finish(take b)                                     // refused: LANG-12, T: plain
}
```

### MEM-4: Sized destinations

```
fn build_in[T](r: Region, bytes n: u64,
               w: scoped fn(mut Dest[T]) -> Result[Child[T], BuildError])
    -> Result[Builder[T, in r], BuildError]
type BuildError = Overflow()
```

`build_in` requires a compact `T` and fixes the destination's `W` to `T`'s. It admits one
allocation of `n` bytes in `r`, whose refusal raises `Capacity`
([MEM-21](budgets.md#mem-21-admission)) before `w` runs, then calls the writer `w` once with a new destination
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
would exceed `2^b − 1` for that `W` (MEM-9), writes nothing, returns `Error(Overflow())` and
leaves the destination usable for a smaller put. The `Child[T]` that `w` returns is the root: the builder holds the
destination's whole used prefix as the blob, and the reference that `finish` publishes
records the root's offset in it. A published compact value is copied into another region by
`clone_in` (MEM-17). Nothing in a destination assumes a fixed element stride. Bytes of an unused or failed destination stay charged until the region closes.
`Dest` is scoped and moves (OWN-3), so a destination cannot be stored, returned or passed
to `finish`.

```hirsel
@compact(u32) type Note { id: u64, body: Text }
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

Its root `T` is a streamable region type: a compact type, which supplies its `W`, or `Text`
or a sequence, encoded with `W = u64`. Both checks read the root's outermost type at the
call: a type parameter is never compact or a root, whatever its bounds, because no bound
says so and a generic body uses only its bounds
([OWN-23](ownership.md#own-23-generic-bodies)), while a sequence of a parameter is a root,
`SeqSpan[T]` when `T: streamable`. So a generic function cannot build or stream a document
whose root is its own parameter; its caller calls `build_in` or `build_stream_in` at its
own type. It returns `Error(Overflow())` when `n` exceeds the
blob cap of that `W`, then admits `n` bytes as `build_in` does, and only then calls `w`
once. A `Writer[U, in r]` is the right to
write one complete `U` at the allocation's next byte. It is scoped and moves (OWN-3), has
no size and no cleanup, and is never captured by any closure, stored, returned or held in
another value; it passes only to a parameter that declares a writer. An element or field
writer may be of any packed `U`, plain ones included. It is completed in one of two ways,
each giving the `Child[U]` of the complete value:

- `emit(take w, x)` writes all of `U` in one operation: for a compact `U`, `x` is a
  construction literal of `U`, written in declaration order; for a scalar, `bool` or
  `char`, a value; for a plain tuple or record, its literal; for `Text`, a view of a
  `String`, whose bytes are copied after one fit check (`text.as_string` reads a `Text` as
  one); for `FixedSpan[T]`, a view of a `List[T]`. A field's operand in a literal may be
  `emit(f)`, where `f` is a scoped callback that takes the field's writer and returns its
  child; `f` runs, and its captures are lent, only while that field is written
  ([OWN-21](ownership.md#own-21-scoped-callbacks)). `emit(f)` is written nowhere else.
- `writer.close(take w)` completes a `Text`, `SeqSpan` or `IndexedSpan` written piece by
  piece, whose header was reserved when its writer was made. Closing an `IndexedSpan`
  checks that its whole table fits, then writes it in one walk of the elements.

Before closing, `writer.scalar(mut w, c)` appends a `char` to a `Text`,
`writer.push(mut w, s)` appends the bytes of a view of a `String` after one fit check, and
`writer.append(mut w, f)` calls `f` once with a writer for the sequence's next element; `w`
is lent to that call alone, and the count advances only when `f` returns the element's
child. `writer.completed(w, i)`, which walks `i` elements, and `writer.last_completed(w)`, in
constant time, return views anchored to `r` of complete elements; nothing views an open
value. Each header, scalar or field is checked before its bytes are written, and an
`IndexedSpan`'s whole table before its first entry is stored: a write that does not fit in
`n`, or that would make a count, byte length, offset or the used prefix exceed `2^b − 1`,
returns `Error(Overflow())` and writes none of the bytes it guards, while bytes written before
it stay. A `Child` that a callback returns is checked when it is received, before the
element counts or its parent completes: its identity and offset must be those of the frame
handed out, or the operation fails with `Bounds`. This guards against a program mixing
children, not against the engine's own writes. A fit failure earlier in the callback is
returned at once; no `Child` exists yet to check.

Every call that takes a writer and returns a `Result` is the operand of a `?` that
propagates to the enclosing function or callback, or in its **returned tail**: the operand
of `return`, or the body's result reached through blocks and `if` and `match` arms, never a
`let` initializer, an argument, a scrutinee or a discarded expression. Its result is never
bound, matched or dropped. So an `Error` leaves the whole build: nothing is published, no abandoned frame
resumes, the `n` bytes stay charged until `r` closes, and views of complete elements stay
valid ([FAIL-6](failure.md#fail-6-region-prefixes)). Nothing infers `n`, runs `w` twice,
moves or grows a written byte, or admits in `finish`; a program that wants the exact size
computes it first from input it can read twice ([example 8](examples.md#8-a-json-document-written-in-place)).
`finish_trim(take b)`, for either builder, publishes as `finish` does and refunds the
unused end, `round8(n) − round8(used)`, to the limit the allocation's record leads to
([MEM-20](budgets.md#mem-20-limit-records-and-hand-over)), whatever limit is current; it
moves nothing and cannot fail.

```hirsel
fn batch[T: streamable](r: Region, bytes n: u64,
    f: scoped fn(take Writer[SeqSpan[T], in r]) -> Result[Child[SeqSpan[T]], BuildError])
    -> Result[SeqSpan[T] in r, BuildError] {
  let b = build_stream_in(r, bytes n, f)?
  Ok(finish_trim(take b))
}
```
```hirsel
fn doc[T: streamable](r: Region, bytes n: u64,
    f: scoped fn(take Writer[T, in r]) -> Result[Child[T], BuildError])
    -> Result[T in r, BuildError] {
  let b = build_stream_in(r, bytes n, f)?          // refused: MEM-4, T is not a root
  Ok(finish_trim(take b))
}
```
```hirsel
@compact(u32) type J = Null | Arr(SeqSpan[J])
fn list[in r](take w: Writer[SeqSpan[J], in r]) -> Result[Child[SeqSpan[J]], BuildError] {
  writer.append(mut w, fn(take x: Writer[J, in r]) -> Result[Child[J], BuildError] { emit(take x, Null) })?
  writer.close(take w)
}
```
```hirsel
@compact(u32) type J = Null | Arr(SeqSpan[J])
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
```hirsel
@compact(u32) type J = Null | Arr(SeqSpan[J])
fn add[in r](mut w: Writer[SeqSpan[J], in r]) -> Result[(), BuildError] {
  let z = { writer.append(mut w, fn(take x: Writer[J, in r]) -> Result[Child[J], BuildError] { emit(take x, Null) }) }
  z                                            // refused: MEM-4, the append is in a let initializer, not a returned tail
}
```

### MEM-5: Publication

A published region value is immutable for the life of its region and never moves. No
operation writes it, and there are no cells. Reading a region value is reading a view of
it ([OWN-25](ownership.md#own-25-region-values-are-read-as-views)); `let` copies its locator,
never its bytes.

```hirsel
fn f(r: Region, t: Text in r) -> u64 { text.len(t) }
```
```hirsel
fn f(r: Region, mut t: Text in r) {}                       // refused: MEM-5, no mut of region data
```

### MEM-6: Roots

A compact value's parts are in its own blob (MEM-10). An own or resource type with a region
field names a region parameter for it: `type Token[in src] { text: Text in src, line: u32 }`.
A parameter written with a bare region type (`t: Text`) accepts a value in any region that
outlives the call; a result or field with a region type names its region. A function's region
parameters are its `Region` parameters and its generics written `in a`, which the argument
types fix (`fn finish[T, in r](take b: Builder[T, in r]) -> T in r`).

A region `a` **outlives** `r` when `a` is `r`, when `a` is opened in a block that encloses
`r`'s block, when `a` is a parameter and `r` is opened in the body, when the signature
declares `where a: r`, or by transitivity. A published `T in a` is usable as `T in r` when
`a` outlives `r`, and the same holds inside `Option`, tuples and the region arguments of a
type. Storing a `T in a` into a slot or field typed `T in r` requires `a` to outlive `r`.
Each check reads the lexical nesting and the signature only.

```hirsel
type Token[in src] { text: Text in src, line: u32 }
fn tok(s: Region, t: Text in s) -> u64 {
  region r { let k: Token[in r] = Token(text: t, line: 1); text.len(k.text) }
}
```
```hirsel
type Hidden { text: Text }                                   // refused: MEM-6, no region
```
```hirsel
type Token[in src] { text: Text in src, line: u32 }
fn tok(t: String) {
  region outer {
    let mut k: Option[Token[in outer]] = None
    region inner { let x = text.copy_in(inner, t); k = Some(Token(text: x, line: 1)) }
                                               // refused: MEM-6, inner does not outlive outer
  }
}
```

### MEM-7: Region escape

A value whose type mentions a region `r` opened by a block may not leave that block: not
as its result, not into a slot declared outside it, not into an escaping closure, a send
or a spawn. A function's result may mention only its region parameters. These are checks
on types; nothing is traversed.

```hirsel
fn width(s: String) -> u64 { region r { let t = text.copy_in(r, s); text.len(t) } }
```
```hirsel
fn leak(s: String) -> u64 {
  let t = region r { text.copy_in(r, s) }                   // refused: MEM-7, t names r
  text.len(t)
}
```

### MEM-8: Close

A region closes on every exit edge of its block: after the block's views end and its owners
are cleaned, all its storage is freed and refunded at once
([T5.3](ownership.md#own-26-the-transition-table)). Closing cannot fail. A region cannot be
reset or reopened; `loop { region r { .. } }` opens a fresh region each iteration.

## Compact layout

### MEM-9: Compact types

`@compact(W) type T` declares a compact layout and makes `T` region data (MEM-2), where `W`
is `u16`, `u32` or `u64`.
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
@compact(u32) type Entry { id: u64, name: Text, scores: FixedSpan[u32] }
```
```hirsel
@compact(u32) type Bad { owner: String }             // refused: MEM-9, own field
@compact(u16) type Empty {}                          // refused: MEM-9, zero bytes
@compact(u16) type Packed { data: Inline[u8, 4] }    // refused: MEM-9, an Inline field
```

### MEM-10: Inline fields and links

A compact field of compact type, `Text` or a sequence type is stored inline: its bytes are part
of the blob. A `Link[U]` field refers to a `U` earlier in the same blob and is stored as
the `W`-width byte distance back from the field to its target; `Option[Link[U]]` uses zero
for `None`. Links are formed only from the `Child` values of MEM-4; a child is complete
before its parent is put, and every encoding is at least one byte, so every distance is at
least 1 and in bounds. Reading a link field through a view of its holder yields a view of
the target, at the cost of one load and one subtraction with no check; a link is never a
value apart from its field, and no integer converts to one. A blob that may hold a link is moved or
copied only whole (MEM-4, MEM-17), so its links stay valid. Edges to other blobs or regions are region
fields of an own type (MEM-6).

```hirsel
@compact(u32) type Expr = Num(i64) | Add(Link[Expr], Link[Expr])
fn eval(e: Expr) -> i64 { match e { Num(n) => n, Add(a, b) => eval(a) + eval(b) } }
```
```hirsel
@compact(u32) type Expr = Num(i64) | Add(Link[Expr], Link[Expr])
fn keep(e: Expr) -> Link[Expr] { match e { Add(a, _) => a, _ => fail("leaf") } }
                                              // refused: LANG-11, a link is not a value: a is a view
```

### MEM-11: Active-size variants

A compact variant value occupies its tag plus its active payload only. Replacing the
variant of a published value is impossible (MEM-5); a new value is built instead.

### MEM-12: Region sequences

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
expected ([LANG-11](language.md#lang-11-typing-is-local)), and skipping any value costs work fixed by its type, never by its contents.
`seq.len(s)` reads the count, `seq.nth(s, i)` walks `i` elements, `indexed.get(s, i)` makes
one bounds test and one load, and `for` visits the elements in order
([SEM-6](semantics.md#sem-6-iteration)); neither is indexable
([LANG-21](language.md#lang-21-operators)). They are written only by the
writers of MEM-4. `seq.index_in(r, s)` builds a separate `SeqIndex` of `s` in `r`, read in
constant time by `seq.get(x, i)`; it keeps `s`'s locator, and nothing makes one from
offsets or pairs it with another sequence.

A type is **packed** when it is an integer, a float, `bool`, `char`, `Text`, a compact type,
a tuple or record of plain kind with at least one field, each packed, a `FixedSpan` whose
elements have MEM-9's encoding, or a `SeqSpan` or `IndexedSpan` of a packed type; nothing
else is. So every packed encoding has positive size and holds no capability, callable,
scoped, own or counted part: `()`, `Never`, callables, capabilities and `Inline` are not
packed. The test reads every arm and field, those behind a sequence header included. It is
**streamable** when it is packed and no `Link` appears in it at any depth, sequence elements
included; `Link`'s empty holds set does not decide this. Both are bounds (OWN-4) that
summaries compute once per declaration group; neither is declared, and `FixedSpan[T]` keeps
its own `T: plain` domain. A
compact type may contain itself through a sequence, whose header ends its layout (OWN-2),
when every such cycle keeps its type arguments; inline it still may not.

```hirsel
@compact(u32) type J = Null | Arr(SeqSpan[J])
fn count(j: J) -> u64 { match j { Null => 1, Arr(xs) => { let mut n: u64 = 1; for x in xs { n += count(x) }; n } } }
```
```hirsel
fn names(xs: FixedSpan[Text]) -> u64 { 0 }                  // refused: MEM-12, Text has no fixed size
fn units(xs: SeqSpan[()]) -> u64 { 0 }                      // refused: MEM-12, () is not packed
```
```hirsel
fn first(xs: SeqSpan[u64]) -> u64 { xs[0] }                 // refused: LANG-21, use seq.nth
```

## Own and counted data

### MEM-13: Own layout

Own types are capacity-based: a record holds its fields inline, a variant reserves room for
its largest payload, `Box[T]` holds one block made by `box.new(take x)`, and `List`,
`String` and `Map` hold a block with a length and a capacity. Growing a collection may move
its block, which is safe because a view freezes it
([OWN-10](ownership.md#own-10-freezes)). Field order, padding, alignment, how a variant stores its tag and the native
calling convention, including how several scalar results are returned, are private to the
implementation and may differ between engines ([SEM-8](semantics.md#sem-8-determinism)). A
type that contains itself inline is refused by
[OWN-2](ownership.md#own-2-type-summaries).

```hirsel
type Tree { label: String, kids: List[Tree] }
```
```hirsel
type Chain = End | Link(u64, Chain)                       // refused: OWN-2, use Box[Chain]
```

### MEM-14: Rc

`rc.new(take x) -> Rc[T]` requires `T: shareable` and moves the
complete owner `x` into a new counted node with count 1
([T1.3](ownership.md#own-26-the-transition-table)). An `Rc[T]`'s payload is immutable except through `rc.with_unique`, and an `Rc` is task-local:
it is never `send`. A counted value is read where a view of its payload is expected.
Because a node is made only from a complete owner, counted values form no cycles; a
cyclic structure uses a library pool (MEM-18). Wrapping a value in `Rc` or `Arc` frees nothing it
owned.

```hirsel
type Node: shareable { label: String, next: Option[Rc[Node]] }
fn cons(take l: String, take tail: Option[Rc[Node]]) -> Rc[Node] {
  rc.new(Node(label: take l, next: take tail))
}
```
```hirsel
fn wrap(take f: HostFile) { let _ = rc.new(take f) }       // refused: LANG-12, at take f: not shareable
```

`rc.with_unique[T: shareable, R](mut h: Rc[T], f: scoped fn(mut T) -> R) -> Option[R]`
has the same contract as `arc.with_unique` below, with a non-atomic task-local count
test and no CONC event because `Rc` is not `send`.

### MEM-15: Arc

`arc.new(take x) -> Arc[T]` requires `T: shareable + send`. An
`Arc[T]`'s payload is immutable except through `arc.with_unique`, and an `Arc` is `send`.
Its creation, every share, every release, every `try_unwrap` and every `with_unique` test
is an event in its unit's event order ([CONC-10](tasks.md#conc-10-event-order)); an `Arc`
never leaves its unit ([CONC-15](tasks.md#conc-15-units)), so the
count each operation observes is the count that order gives.

`arc.with_unique[T: shareable + send, R](mut h: Arc[T], f: scoped fn(mut T) -> R) ->
Option[R]` tests whether `h` is the payload's only holder. When the count is 1 it lends
the payload to `f` as `f`'s `mut` parameter, runs `f` once in place and returns `Some(r)`;
when the count is greater than 1 it returns `None` and `f` does not run. The operation
itself copies nothing, admits nothing and leaves the count unchanged; `f` may admit as any
code does. The test is one event, of the same kind as `try_unwrap`'s and ordered the same
way (CONC-10, [CONC-11](tasks.md#conc-11-arc-across-tasks)), and a `None` is a poll. The
payload is the one place reached through a counted value that is ever writable
([OWN-12](ownership.md#own-12-assignment)), and only as `f`'s parameter. That is sound
because, while `f` runs, `h` is the only holder and nothing else can reach the payload or
add a holder:

- **Count 1 means `h` alone.** Every holder is a slot, a field, an element or a `Latest`
  root that holds the node, and `h` is one of them, so a count of 1 leaves none other, in
  this task or any other. There are no weak counted references in v1
  ([reserved](reserved.md#not-in-v1)); if they are added, the test must also find none.
- **`h` is a writable place.** It is lent `mut` ([OWN-11](ownership.md#own-11-call-arguments)),
  so an `Arc` reached through a view, a region value or another counted value cannot be
  passed: an `Arc` field of a shared payload has count 1 and is reachable by every holder of
  the outer node, and is refused here. Inside an outer `with_unique`, the outer payload is
  `f`'s `mut` parameter, so a nested `with_unique(mut p.inner, g)` is accepted.
- **The count cannot rise while `f` runs.** A share needs a holder
  ([MEM-16](#mem-16-counting)), the only holder is `h`, and `h` is lent to `with_unique` for
  the whole call, so `f` cannot name it, view it, share it, move it, send it or assign it
  ([OWN-9](ownership.md#own-9-overlap), [OWN-21](ownership.md#own-21-scoped-callbacks)). No
  other task holds the node, so none can share it either. This holds whatever events `f`
  performs.
- **The loan cannot escape.** `f` is scoped, so neither `R` nor anything `f` stores may be
  anchored to its parameter ([OWN-21](ownership.md#own-21-scoped-callbacks)), and a
  `mut` parameter is never returned or taken (OWN-11). Views of the payload made inside `f`
  end with its invocation.
- **The payload is a complete `T` at every point.** `f` changes it only by assignment,
  `replace` and `mut` loans, which keep a complete value in place
  ([OWN-12](ownership.md#own-12-assignment), [OWN-13](ownership.md#own-13-replace),
  [OWN-14](ownership.md#own-14-no-partial-moves)). A failure inside `f` propagates from
  `with_unique` and leaves the payload holding what `f` had installed before it; `h` stays
  with its owner and is cleaned by [FAIL-8](failure.md#fail-8-exit-edge-cleanup) like any
  other holder. A run stop ends the run without cleanup
  ([FAIL-15](failure.md#fail-15-run-stops)).
- **Charges follow the existing rules.** The node stays charged to the limit that created
  it (CONC-11). Growing a collection in the payload charges the current limit and refunds
  the old storage to the limit its record leads to
  ([MEM-26](budgets.md#mem-26-growth-charges-the-current-limit)). A refund that crosses
  tasks is an event ([MEM-20](budgets.md#mem-20-limit-records-and-hand-over)).

A write through `f` is not an event: once the test has found the count 1, no other task
reaches the payload (CONC-10's rule), and every earlier read by another task is ordered
before the release that dropped the count to 1.

Copy-on-write is std, not core: `arc.update[T: shareable + send, R](mut h: Arc[T], f:
scoped fn(mut T) -> R) -> R` runs `f` through `with_unique`, and when
`h` is shared first replaces `h` with a new `Arc` holding `clone(h)`
([MEM-17](#mem-17-clone)), releasing the old share. `T: shareable` is the whole clone bound:
a shareable type holds no resource, scoped value or opaque owner
([OWN-4](ownership.md#own-4-bounds)), so `clone` accepts every payload. When the copy or
the new node is refused, `h` is unchanged and `f` has not run. Otherwise `f` runs exactly
once.

```hirsel
type Hits: shareable + send { n: u64 }
fn bump(mut a: Arc[Hits]) -> Option[()] { arc.with_unique(mut a, scoped fn(mut h: Hits) { h.n += 1 }) }
```
```hirsel
type Hits: shareable + send { n: u64 }
fn twice(mut a: Arc[Hits]) -> Option[u64] {
  arc.with_unique(mut a, scoped fn(mut h: Hits) -> u64 { let b = arc.share(a); h.n })   // refused: OWN-9, a is lent to with_unique
}
```
```hirsel
type Leaf: shareable + send { n: u64 }
type Pair: shareable + send { leaf: Arc[Leaf] }
fn reach(mut p: Arc[Pair]) -> Option[()] {
  arc.with_unique(mut p.leaf, scoped fn(mut l: Leaf) { l.n = 1 })   // refused: OWN-8, p.leaf is reached through an Arc
}
```

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
fn own_copy(take h: Rc[Node]) -> Node {
  match rc.try_unwrap(take h) { Ok(take n) => take n, Error(take g) => clone(g) }
}
```

### MEM-17: clone

`clone(x)` and `clone_in(r, x)` are built-in typing forms. The expected type selects
their result `U` and what is copied: first `x`'s own type, if it is `U`; otherwise the
payloads through `Box`, `Rc` and `Arc`, outermost first, the first that is `U`. No match is
refused; a conversion between two types is an explicit operation, such as `text.copy_in`,
never a clone. With no expected type `U` is open, and the
call is refused ([LANG-12](language.md#lang-12-generics)): `let k = clone(key)` is,
`let k: Bytes = clone(key)` is not. `clone(x) -> U` makes a
plain, own or counted `U`: it deep-copies the own parts of the selected value into new
blocks charged to the current limit and shares its counted edges, a counted root selected
as `U` included, and keeps the regions of its region fields. A region reference (`S in r`),
at the root or as a part, is copied as the reference: the copy shares its block, and
nothing is allocated for it. `clone_in(r, x) -> U
in r` copies a region value `x` into `r`. A compact value or sequence is never
re-encoded at `U`'s own `W`; which bytes are copied follows its type, with no check at run
time. A value of a streamable type (MEM-12) is copied as exactly its own bytes at its `W`,
as the root of a new blob, admitting its length rounded up to 8. Any other compact value,
which may hold a `Link`, is copied as the whole used blob, keeping the selected value's
offset. A streamable value is complete once it can be viewed, so it may be cloned while its
blob is still being written (MEM-4); a value that may hold a link is viewed only in a
published blob, because `build_stream_in` writes only streamable documents. Each copied `List`, `String`, `Map` or `Heap` gets capacity
equal to its length, and each copied `Deque` the least power of two not below its length, 0
when it is empty. The copy is depth-first: a record's fields in declaration order, a variant's active
payload, a list's elements in index order and a map's entries in insertion order (key, then
value), and each block is admitted when the walk reaches it, before what it holds. A value
whose type summary holds a resource, a scoped value or an opaque owner
([OWN-2](ownership.md#own-2-type-summaries)) is never cloned and is refused here, at the
form; plain callables and capabilities copy as plain values. A core type may instead have a
**clone body**, a core item `clone[P..](x: T[P..]) { .. }` in any core module and refused
anywhere else: one per type, taking one view of the type, generic in exactly the type's
parameters with no stronger bounds, and writing no result type; it makes the copy that
`clone` returns, `T[P..]`. It is checked assuming its parameters hold no
resource, scoped value or opaque owner, which is sound because a clone reaches it only
after this rule's check has passed at the form. Failure is
[T1.5](ownership.md#own-26-the-transition-table).

```hirsel
fn keep(r: Region, t: Text) -> Text in r { clone_in(r, t) }
```
```hirsel
fn freeze(r: Region, s: String) -> Text in r { clone_in(r, s) }
                                                            // refused: MEM-17, String is not Text
```
```hirsel
fn dup(r: Region, b: SpanBuilder[u8, in r]) -> SpanBuilder[u8, in r] { clone(b) }
                                                            // refused: MEM-17, an opaque owner
```
```hirsel
fn size(key: Bytes) -> u64 { let k = clone(key); list.len(k) }
                                                            // refused: LANG-12, U is open
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
so this is what keeps that order from reaching any observable
([SEM-8](semantics.md#sem-8-determinism)).

```hirsel
type Handle: key { pool: Unique, index: u64, generation: u64 }
fn same_pool(a: Handle, b: Handle) -> bool { a.pool == b.pool }
```
```hirsel
fn older(a: Unique, b: Unique) -> bool { a < b }            // refused: LANG-21, Unique is not ordered
```

### MEM-29: Append-only logs and task-local cells

A `Cell[T]` is an own, task-local, opaque handle of 8 bytes to a separately admitted node
that owns one `T`. It is never `send`, `shareable`, `key`, `ordered` or `codec`, and it is
never cloned ([MEM-17](#mem-17-clone)): it holds an opaque owner, and its element's holds.
What the node owns is state that no view exposes ([OWN-8](ownership.md#own-8-views)):
every operation takes a view of the handle and writes only the node and what it owns,
never the handle's 8 bytes, so no byte a view of the handle exposes changes.

A `Log[E: inert]` is a library type, not a primitive: core's `log` module declares it
`pub opaque type Log[E: inert] { entries: Cell[List[E]] }`, with ordinary bodies over the
cell ([PROG-2](programs.md#prog-2-visibility)). It is a cell to everything above, its
summary included. Outside the module its only operations are `new`, `append` and a
consuming `finish`, so no holder of a view can read, remove or replace an entry.

**The element condition.** A `Cell[T]` is accepted only where `T` holds no resource, no
capability, no view, no scoped value and no region reference at any depth, as its summary
records ([OWN-2](ownership.md#own-2-type-summaries)), the condition of an owning closure's
captures ([LANG-14](language.md#lang-14-captures)). A rigid type parameter is read from
its bounds: one bounded by `inert` meets the condition, and an unbounded one, which may
hold a capability, does not. The condition is checked where the type is formed and at
every call of a `cell` operation, inferred and forwarded type arguments included, and for
`cell.with`'s `A`, which appears in no result. Any other `Cell` or argument type is
refused here, at the type; core's `cell` module declares its operations over open
parameters, and the condition holds where they are called. `Log[E]` refuses scoped type
arguments under [OWN-19](ownership.md#own-19-scoped-values). A scoped argument also
fails its `inert` bound ([LANG-12](language.md#lang-12-generics), OWN-4), as does an
opaque owner such as `Causes`.

**Logs.** A log is charged as its cell: the 24-byte node of a `Cell[List[E]]`. No
operation reads, counts, clears or iterates its entries.

- `log.new() -> Log[E]` admits the node.
- `log.try_reserve(l: Log[E], n: u64) -> Result[(), log.Full]` reserves room for `n`
  additional entries through `cell.with` and `list.try_reserve`. A refused reserve
  returns `Error(Full())` and leaves the log unchanged.
- `log.append(l: Log[E], take e: E)` takes a view of the log and pushes `e` through
  `cell.with`. It admits the growth of the entries' storage, as `list.push` grows a
  `List[E]` ([MEM-24](budgets.md#mem-24-logical-charges)), before it commits `e`; a
  refused growth raises `Capacity` and leaves the log unchanged. Its callback is a core
  function, so the call is one step ([MEM-30](budgets.md#mem-30-step-bounds)).
- `log.finish(take l: Log[E]) -> List[E]` returns the entries in append order, moving their
  storage into the `List` without admitting, and frees and refunds the node. Dropping a
  log cleans its entries in append order, then frees and refunds the node.

**Cells.** A cell's node is a `Box[T]`'s: its logical size is `T`'s rounded up to 8
([MEM-24](budgets.md#mem-24-logical-charges)), so a `Cell[List[E]]` is charged the 24
bytes a `Log[E]` is, and a `Cell[()]` nothing.

- `cell.new(take v: T) -> Cell[T]` admits the node before it takes `v`; a refused
  admission raises `Capacity` and leaves `v` the caller's.
- `cell.get[T: plain](c: Cell[T]) -> T` copies the payload.
- `cell.replace(c: Cell[T], take v: T) -> T` moves the payload out and installs `v`. It
  admits nothing and cannot fail. `cell.set(c, take v)` is `replace` with the old payload
  dropped.
- `cell.with(c: Cell[T], take a: A, f: fn(mut T, take A) -> R) -> R` lends the payload in
  place to `f` as its `mut` parameter ([OWN-12](ownership.md#own-12-assignment)), moves
  `a` into `f`, and returns `f`'s result. It copies and admits nothing itself; `f` admits
  as any code does, charged to the limit current where it runs
  ([MEM-26](budgets.md#mem-26-growth-charges-the-current-limit)), and entering `f` is a step
  when `f` is user code ([MEM-30](budgets.md#mem-30-step-bounds)); a core function takes
  none.
- `cell.into_inner(take c: Cell[T]) -> T` moves the payload out without admitting, its
  blocks keeping their payers, and frees and refunds the node. Dropping a cell drops its
  payload, then frees and refunds the node.

While `f` runs no expression names `c`, so `with` needs no flag and no `Option`:

- `f` is a plain `fn` and captures nothing ([LANG-13](language.md#lang-13-callables)).
- `a` holds no view and no scoped value, and no argument of the call may move the owner
  `c` is a view of, nor any part of it ([OWN-9](ownership.md#own-9-overlap)), so `a`
  cannot carry it in.
- The `mut T` parameter reaches only the payload; a cell inside it is a different cell,
  which the payload owns.
- There are no mutable statics ([CONC-12](tasks.md#conc-12-no-shared-mutable-state)), and
  a cell is never sent.

`R` is a plain callable's result, which anchors to no parameter (LANG-13), so no loan of the
payload outlives `f`. A second operation on `c` inside `f` is unreachable by typing.

**Persisting effects.** An append to a log and every change to a cell stay: a later
`Error`, failure or exhausted inner `bound` undoes nothing
([FAIL-16](failure.md#fail-16-isolation)). A cell's payload is a complete `T` at every
point, since it changes only by `replace` and by `mut` loans. If `f` fails, the payload
holds what `f` installed before the failure, and the cell is not poisoned: `with` is not
atomic. Log and cell operations act on a value only their own task reaches, so none is an
event ([CONC-10](tasks.md#conc-10-event-order)).

```hirsel
type Row { name: String }
fn read(m: Map[u64, Row], seen: Log[u64], k: u64) -> Option[View[Row]] in m {
  log.append(seen, k)
  map.get(m, k)
}
```
```hirsel
// refused: OWN-19, View[String] is scoped
fn names(l: Log[View[String]]) {}                 // refused: LANG-12, View[String] is not inert
```
```hirsel
fn seen(take l: Log[u64]) -> List[u64] {
  let v = view l
  let xs = log.finish(take l)                     // refused: OWN-10, l is frozen
  log.append(v, 1)
  xs
}
```
```hirsel
fn bump(mut n: u64, take by: u64) -> u64 {
  n += by
  n
}
fn hits(c: Cell[u64]) -> u64 { cell.with(c, 1, bump) }
```
```hirsel
fn senders(c: Cell[Sender[u64]]) {}               // refused: MEM-29, at Sender[u64]
fn keep(mut n: u64, take c: Cell[u64]) -> u64 { n }
fn again(take c: Cell[u64]) -> u64 {
  cell.with(c, take c, keep)                      // refused: OWN-9, c is viewed by this call
}
```

## Performance targets

This section is not normative. MEM-31 and MEM-32 are performance targets for native engines:
they say what a fast engine does, and nothing a program observes depends on them. They keep
their numbers so that benchmarks and engine notes can cite them.

### MEM-31: Construction in place

Target, not a rule. A value's **inline bytes** are the bytes MEM-13 holds in its owner: a record's fields, a
variant's payload, and the pointer, length and capacity of a `Box`, `List`, `String` or `Map`,
never the blocks they point to. A native engine aims to write a fresh value's inline bytes
directly into its destination, with no temporary of the value and no copy of it, in three
cases:

- **Results.** The operand of `return`, or a body's result reached through blocks and `if`
  and `match` arms, is built in the caller's receiving place. So is a local that every exit
  of the body that reaches it returns whole.
- **Container slots.** The element or entry argument of a core operation that stores it into
  a collection (`list.push`, `list.set`, `map.insert` and the like, [core](core.md)) is built
  in its slot when that slot exists without a new admission: spare capacity, or the element
  being set.
- **Fields.** A literal's field operand that is itself fresh is built inside the literal's
  own destination.

Any other value is built in a local place and moved. What follows in this paragraph and the
next is not a target: it restates rules that bind every engine whether or not it builds in
place. A move copies only inline bytes: the
blocks a value owns move with their pointer, are never copied, and are never re-admitted, so
a move never allocates and never fails ([OWN-6](ownership.md#own-6-let-take-and-copy)). The destination's
owner is the owner the value would have after that move, so building in place never changes
custody: a result built in the caller's place and then sent from a path that does not return
it is moved out like any other value.

Nothing a program observes depends on which case applies. Fields are evaluated left to right
into their owners as [T1.1](ownership.md#own-26-the-transition-table) says; every allocation is
admitted where its row says; a collection admits its growth after its arguments are evaluated
and before it commits, and nothing is admitted earlier or speculatively because the
destination is known; an assignment does not overwrite its old value before its right side
succeeds ([OWN-12](ownership.md#own-12-assignment)). A failure before completion leaves the
destination as it was, each survivor with the owner
[FAIL-7](failure.md#fail-7-one-owner-per-survivor) names, and a pending result is cleaned in
[FAIL-9](failure.md#fail-9-pending-results)'s order whether or not it was built in place.
Charges, refusal points, cleanup order and output therefore do not depend on whether a value
was built in place; only the instructions and the bytes copied differ. No conformance test
checks which case an engine took: a missed case, such as a container slot built locally and
moved in, is a performance bug.

### MEM-32: Engine obligations

Targets, not rules. They describe the native engines Hirsel aims for; the reference evaluator
need not meet them, and no program can observe whether an engine does. Charges stay within
their logical bounds whatever an engine's layout
([MEM-28](budgets.md#mem-28-charges-are-an-upper-bound)), and everything
[SEM-8](semantics.md#sem-8-determinism) makes observable is unaffected by them. Each target
names the count a benchmark or an engine test can measure; missing one is a performance bug,
not a conformance failure:

- **A size-class allocator per task.** Own blocks (`Box` nodes, collection storage, counted
  nodes, closure environments) come from the allocating task's allocator, by size class, with
  no process-wide lock on the allocation or free path. A block freed by another task returns
  without a process-wide lock too. Count: a loop that allocates and frees blocks of one size
  class makes no request to the operating system after its first iteration.
- **A demand-sized first region chunk.** A region's first chunk is sized to its first
  allocation, rounded up to the allocator's granule, never a fixed size; later chunks grow
  geometrically. Count: a region whose allocations total `n` bytes reserves at most `2n` bytes
  plus one granule.
- **Pools inside a region.** A region keeps allocations of one fixed size and alignment
  together, so `k` such allocations occupy `k` × their size with no header per allocation,
  apart from the slack at a chunk's end. Count: reserved bytes minus occupied bytes in a
  region is at most its chunks' end slack.
- **Regions reuse their storage.** A closing region returns its chunks to its task's
  allocator, and the next region the task opens takes them first, so a region opened and
  closed per request or per iteration is a rewind. Count: a loop that opens a region, makes
  the same allocations in it and closes it makes no request to the operating system after its
  first iteration.
- **No teardown walk for plain data.** Dropping a value visits only the parts that have
  cleanup: storage whose elements are plain (`List[u64]`, `String`, a `Map` of plain keys and
  values) is freed without visiting its elements, and closing a region frees its chunks
  without visiting any value in them, because region data has no cleanup. Count: dropping a
  `List[T]` of plain `T` and length `n`, or closing a region holding `n` values, does work
  independent of `n`.

Size classes, chunk sizes, pools and their placement are private
([MEM-9](#mem-9-compact-types), [MEM-13](#mem-13-own-layout)).
