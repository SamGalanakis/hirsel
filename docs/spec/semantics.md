# Semantics

### SEM-1: Integers

The integer types are `i8` to `i64` and `u8` to `u64`. `+`, `-`, `*` and unary `-` fail
with `Overflow` when the result does not fit ([FAIL-2](failure.md#fail-2-causes)). Other
arithmetic is named: `int.wrapping_add`, `int.checked_add` (an `Option`),
`int.saturating_add` and their siblings; the `checked_` operations return `Option[T]`. `/`
truncates toward zero and `%` takes the dividend's sign; division or remainder by zero
fails with `Division`, `MIN / -1` fails with `Overflow`, and `MIN % -1` is 0. A shift count
has the left operand's type; a count that is negative or at least the width fails with
`Overflow`; `<<` discards the bits shifted out and never fails otherwise, and `>>`
sign-extends signed and zero-extends unsigned values. Which operand types each operator
takes is [LANG-21](language.md#lang-21-operators). Conversions are named and take their
target from the expected type. `int.widen` is accepted only where every value of `T` fits
in `U`, and `int.truncate` only where `U` is no wider than `T`; any other instantiation is
refused at the call. `int.narrow` fails with `Overflow`, `int.try_narrow` returns
`Option[U]`, and `int.truncate` keeps the low bits. `int.mul_wide` gives the full 128-bit product of two `u64`
as its high and low halves. The bit counts are `int.leading_zeros`, the zero bits above the
highest one bit, `int.trailing_zeros`, the zero bits below the lowest one bit, and
`int.count_ones`, the one bits; each returns a `u32`, the first two the width for 0, and
reads a signed value's two's-complement bits. For an unsigned type, `int.ilog2(x)` returns
`Some(floor(log2(x)))` as a `u32` for positive `x` and `None` for 0; a signed instantiation
is refused. `int.rotate_left(x, n)` and
`int.rotate_right(x, n)` rotate those bits by the `u32` count `n` modulo the width, and
`int.swap_bytes(x)` reverses their byte order, so it is `x` on an 8-bit type; none fails. These are all of core's bit operations
besides the operators of [LANG-21](language.md#lang-21-operators).

```hirsel
fn add(x: u8) -> u8 { int.wrapping_add(x, 100) }
```
```hirsel
fn grow(x: u8) -> u32 { int.truncate(x) }                // refused: SEM-1, u32 is wider than u8
```

### SEM-2: Floats

`f32` and `f64` are IEEE 754 binary32 and binary64 with round-to-nearest-even and no mode
state. Expressions are evaluated as written: no contraction, reassociation or wider
intermediates; `float.mul_add` fuses explicitly. Subnormals are kept and zero is signed, as
IEEE 754 says. Every NaN an operation produces, `float.from_bits32` and `from_bits64`
included, has one bit pattern: `0x7fc00000` for `f32`,
`0x7ff8000000000000` for `f64`. `float.to_int` truncates toward zero and returns `NaN` or
`OutOfRange` as its error; conversions from integers to floats and from `f64` to `f32`
round to nearest even. [core](core.md) lists every numeric conversion. `%` is integer-only.
Floats are neither `key` nor `ordered`; `float.total_order` orders them by IEEE 754
totalOrder. The predicates `float.eq`, `float.lt`, `float.le`, `float.gt` and `float.ge`
perform the IEEE 754 quiet comparisons described in [core](core.md#contracts-the-signatures-do-not-state).

### SEM-3: Text

`char` is a Unicode scalar value; `char.to_u32` and `char.from_u32` (an `Option`, `None` for
surrogates and values above `0x10FFFF`) convert it. `String` is an opaque own type over
UTF-8 storage that is always valid; `Text` is the same text as region data, made from a `String` by `text.copy_in`; `Bytes` is `List[u8]`.
`string.from_utf8` validates a `Bytes` into a `String`, `string.into_bytes` gives its
owned storage back, copying a static literal into new storage, which admits
([core](core.md)), and `string.as_bytes` views it as bytes. `text.as_string(t)` views a `Text` as a `String`
anchored to `t`, with no copy, so every reading operation on strings, `==` and `<` against a
`String` included, reads a `Text`. Length is `byte_len`; scalars are
read with `string.next_scalar(s, at) -> Option[(char, u64)]`, which returns the scalar at
byte position `at` and the next position. `string.slice` fails with `Bounds` at a position
that would split a scalar, and so does `string.truncate(mut s, n)`, which keeps the first
`n` bytes, never shrinks the capacity and fails with `Bounds` past the length.
`string.reserve(mut s, n)` grows the capacity to at least the length plus `n`, as
`list.reserve` does (SEM-4). Equality, order and hashing are bytewise;
normalisation is explicit. String formatting is ordinary calls; there are no format
strings.

### SEM-4: Lists

`List[T]` is own and growable, with the capacity-based layout of
[MEM-13](memory.md#mem-13-own-layout). `xs[i]` and `list.get(xs, i)` read through a view and
fail with `Bounds`, or return `None`, when `i` is out of range. `list.push` and the other
growing operations grow as [MEM-24](budgets.md#mem-24-logical-charges) says, and a refused
growth raises `Capacity` ([MEM-21](budgets.md#mem-21-admission)); `list.capacity` reads the
capacity and `list.reserve(mut xs, n)` grows it to at least `len + n`, as
`list.try_reserve` does with the refusal as a value. Removing elements never
allocates or shrinks. An own `List`'s length is never more than its capacity: growing
operations admit capacity before they add elements, removing elements only shortens it,
and a host read fills only spare capacity ([core](core.md#contracts-the-signatures-do-not-state)). A view of a
list reports that list's length and capacity, and a slice's capacity is its length. A
`View[Bytes]` from `string.as_bytes` instead reports its `String`'s capacity, which for
static storage is 0, below its length ([core](core.md#contracts-the-signatures-do-not-state)). A `List[T]` whose `T` has size 0 ([core](core.md#built-in-types)) has
no element storage: its capacity is `2^64 − 1` from creation, and creating, reserving and
growing it admit nothing; only its length is bounded, so a `reserve` past `2^64 − 1`
elements and a push at that length are refused with `requested` `2^64 − 1`
([MEM-22](budgets.md#mem-22-the-capacity-value)). A view
returned by `list.get` or indexing is anchored to the list
([OWN-20](ownership.md#own-20-scoped-results-name-their-anchor)) and freezes it until the
view's extent ends ([OWN-10](ownership.md#own-10-freezes)); iteration freezes it for the
loop (SEM-6).

```hirsel
fn units() -> u64 { let xs: List[()] = list.new(); list.capacity(xs) }   // 2^64 − 1
```

### SEM-5: Maps

`Map[K: key, V]` is own and iterates in insertion order. Insertion, lookup and removal take
amortised expected constant time apart from hashing and comparing keys, which is linear in
a key's size, and from cleaning what they drop; removal keeps the order of the remaining entries.
Inserting an existing key installs the new value in place, keeps the stored key, cleans
the old value and then drops the incoming key; it admits nothing, so it is never refused. `map.get(m, k)` returns `Option[View[V]] in m`
([OWN-20](ownership.md#own-20-scoped-results-name-their-anchor)). Each `key`
type has exactly one equality, the structural one of SEM-7, and one hash, fixed by the type;
no program supplies or replaces them in v1 ([reserved](reserved.md), R11). The hash is keyed
by a per-run seed that the engine draws from host entropy at entry. The seed is not an input:
it does not pass the replay point ([CAP-8](capabilities.md#cap-8-the-replay-point)), no
recording holds it, and a replay draws a fresh one. No operation reveals a hash word and
iteration is in insertion order, so no value depends on the seed
([CAP-6](capabilities.md#cap-6-no-ambient-input)), and neither do cleanup order, admission,
refunds or steps, so a replay under another seed behaves as the recorded run did (SEM-8).
Lookups and insertions stay expected constant time
for keys chosen without the seed, even keys chosen to collide: a Map whose cheap placement
crowds rebuilds its index once under a keyed hash whose key is derived from the seed
([PROG-11](programs.md#prog-11-map-index)), and that rebuild is not observable either. A Map literal builds its index at run time.
Views from `map.get` are anchored to the map and freeze it as list views do.
`map.update` changes a present value in place through a scoped callback
([core](core.md)).

```hirsel
fn count(m: Map[String, u64], k: String) -> u64 { match map.get(m, k) { Some(v) => v, None => 0 } }
```
```hirsel
fn tally(mut m: Map[String, u64], take w: String) {
  match map.update(mut m, w, fn(mut n: u64) { n += 1 }) {
    Some(()) => (),
    None => map.insert(mut m, take w, 1),
  }
}
```
```hirsel
fn bump(mut m: Map[u64, u64]) -> Option[()] {
  map.update(mut m, 1, fn(mut n: u64) { n = map.len(m) })   // refused: OWN-9, m is lent to update
}
```

### SEM-6: Iteration

`for x in e` iterates a range, or a view of a `List` (a column `c.f` of a `Columns` is
one, [core](core.md)), an `Inline` array, a `FixedSpan`, a
`SeqSpan` or `IndexedSpan` ([MEM-12](memory.md#mem-12-region-sequences)) or a `Causes`
([core](core.md)), binding a view of each element from the lowest index. Over a view of a
`Map` it binds `(View[K], View[V])` for each entry in insertion order. `for take x in e`
consumes an own `List`, moving each element out from the lowest index, and `for take (k, v)
in e` consumes an own `Map`, moving each entry out as a `(K, V)` in insertion order; no
usable collection remains. When the loop ends, by exhaustion, `break`, `return` or failure,
the current iteration's bindings are cleaned first by the ordinary exit-edge rules
([FAIL-8](failure.md#fail-8-exit-edge-cleanup)), so a `(k, v)` pattern's value before its
key; then the elements or entries not yet reached are cleaned in that same order, a map's
each key before its value; then the storage, a map's index included, is freed. Each element
or entry is cleaned exactly once, and none of this hashes, probes or admits. Nothing
else is iterable; scalars are read with `string.next_scalar` (SEM-3). A range `a..b` is
half-open and `a..=b` closed; both are plain, iterate upward, and stop at the type's
maximum without overflow. Its endpoints are typed as a binary operator's operands, the left
giving the right its type ([LANG-21](language.md#lang-21-operators)), and have one integer
type; a range of any other type is refused here, at the range. `for` gives a range no
expected type, so two unsuffixed endpoints have none and are refused by
[LANG-11](language.md#lang-11-typing-is-local). The source is frozen for the loop
([OWN-10](ownership.md#own-10-freezes)), so iteration never observes a change.

```hirsel
fn sum(xs: List[u64]) -> u64 { let mut t: u64 = 0; for x in xs { t += x }; t }
fn three() -> u64 { let mut n: u64 = 0; for _ in 0u64..3 { n += 1 }; n }
fn total(take m: Map[String, u64]) -> u64 {
  let mut t: u64 = 0
  for take (k, v) in m { let _ = string.into_bytes(take k); t += v }
  t
}
```
```hirsel
fn three() { for _ in 0..3 {} }                           // refused: LANG-11, 0 has no expected type
```
```hirsel
fn again(take m: Map[String, u64]) -> u64 {
  for take (k, v) in m {}
  map.len(m)                                              // refused: OWN-5, m moved
}
```
```hirsel
fn letters() { for _ in 'a'..='c' {} }                    // refused: SEM-6, char is not an integer type
```
```hirsel
fn grow(mut xs: List[u64]) {
  for x in xs { list.push(mut xs, x) }                     // refused: OWN-10, xs frozen by the loop
}
```

### SEM-7: Equality and order

`==` and `!=` take `key` operands and `<`, `<=`, `>` and `>=` take `ordered` operands
([OWN-4](ownership.md#own-4-bounds)); any other operand is refused by
[LANG-21](language.md#lang-21-operators), at the operator. This rule says what equality and
order mean, and refuses nothing. Integers, `bool`, `char`, unit, `String` and `Text`
are both. Tuples, `Option`, `Result`, `List`, `Inline` and the region sequences have a
bound when their components do. `Unique` is `key` and not `ordered`
([MEM-18](memory.md#mem-18-pools-are-library-code)). A nominal type has a bound when it declares it and its fields have it.
Equality is structural. Order is numeric for integers and `char`, `false` before `true`,
bytewise for text, lexicographic for tuples, sequences and records (fields in declaration
order, a proper prefix first), and by declaration ordinal then payload for variants. A
counted value and a view compare by payload. Floats, maps, callables, capabilities, resources and
scoped types have neither bound. Comparison never allocates. So an order on two keys is one
tuple comparison, not an `if` on the first key.

```hirsel
type P: key + ordered { x: i64, y: i64 }
fn first(a: P, b: P) -> bool { a < b }
fn pair_before(k1: u64, v1: u64, k2: u64, v2: u64) -> bool {
  (k1, v1) < (k2, v2)                       // k1 < k2, or k1 == k2 and v1 < v2
}
```
```hirsel
type Q: key { w: f64 }                                     // refused: OWN-4, f64 is not key
```

### SEM-8: Determinism

Given the same program, the same values at the replay point
([CAP-8](capabilities.md#cap-8-the-replay-point)), the same root limit and the same charge
policy, every run that no run stop ends ([FAIL-15](failure.md#fail-15-run-stops)) and whose
program reaches no native foreign function ([FFI-8](foreign.md#ffi-8-a-program-that-reaches-foreign-code-is-never-recorded-replayed-or-reused);
a contained one keeps this rule, [FFI-14](foreign.md#ffi-14-determinism-traps-and-replay)) behaves
identically on every run of one **build**: the program as one engine compiled it, for one
target, at one optimisation level. A recording or coverage build of it is the same build
([CAP-9](capabilities.md#cap-9-recording-is-outside-the-program)). Its **observables** are exactly:

- its values, its output and its outcome;
- its steps and slice ends ([MEM-30](budgets.md#mem-30-step-bounds));
- its failures: each cause, its site and its place in the cause chain
  ([FAIL-2](failure.md#fail-2-causes), [FAIL-10](failure.md#fail-10-cause-chain));
- its charges as limits see them: which operations admit, how much, which are refused
  and with what `Capacity` value, and which refunds each admission sees
  ([MEM-21](budgets.md#mem-21-admission) to [MEM-24](budgets.md#mem-24-logical-charges),
  [MEM-28](budgets.md#mem-28-charges-are-an-upper-bound));
- its finalizer runs and their order ([FAIL-8](failure.md#fail-8-exit-edge-cleanup),
  [FAIL-11](failure.md#fail-11-resources));
- each unit's events and their order ([CONC-10](tasks.md#conc-10-event-order));
- its host effects: each provider operation's issue, its arguments and its order
  ([CAP-5](capabilities.md#cap-5-provider-operations), [CAP-8](capabilities.md#cap-8-the-replay-point)).

A build with recording machinery is one of these builds: compiling recording in, and
switching it on, changes none of the observables
([CAP-9](capabilities.md#cap-9-recording-is-outside-the-program)).

**Across builds.** Two of the observables are a build's own. Steps are fuel: each build
counts them deterministically, but two engines or optimisation levels may take different
steps for the same code, so where a step bound runs out and where slices end can differ
between builds ([MEM-30](budgets.md#mem-30-step-bounds)). Charges are an upper-bound
contract: a build charges at most the logical charge of
[MEM-24](budgets.md#mem-24-logical-charges) and may charge less, so an admission that the
logical charges admit is admitted by every build, and the amounts, the `Capacity` values
and the refusals of an admission the logical charges refuse can differ between builds
([MEM-28](budgets.md#mem-28-charges-are-an-upper-bound)). Every other observable, from
values and output to events and host effects, is the same on every engine, build and
optimisation level for every run in which no step bound runs out, no admission is refused,
and every slice end resumes the task that reached it. No test may compare steps, slice ends
or exact charges between builds; a test compares them between runs of one build, or
checks a charge against its logical bound.

**As if.** An engine may do anything whose effects on the observables are those this
specification gives. Anything not listed is the engine's choice: layout, addresses (of
blocks or of functions), allocators, parallelism, timing, and the order of steps that change
no observable between them, such as freeing blocks whose refunds no admission sees apart.
Nothing observable depends on addresses, on the order of declarations in a source, on
timing, on the host, on the Map hash seed ([SEM-5](#sem-5-maps)) or on unrecorded
scheduling. The one value a compiler may change
is a function's revision ([PROG-9](programs.md#prog-9-code-descriptors)), which names the
checked IR its compiler made: it is still the same on every engine and optimisation level.

### SEM-9: Inline arrays

`Inline[T, n]`, where `n` is an integer literal or names a length constant, is `n` values of
type `T` held inline, with no block: its size is `n` × the size of `T` and it admits nothing.
`n` is a value from 0 to `2^64 − 1`; a suffix on it is not part of the type, so
`Inline[u8, 2u8]` is `Inline[u8, 2]`.

**Named lengths.** A **length constant** is a manifest constant
([LANG-20](language.md#lang-20-constants)) of an integer type whose initialiser holds only
integer literals, operators and other length constants. A length names one bare or through
its module, `Inline[u8, KEY_LEN]` or `Inline[u8, keys.LEN]`. It is evaluated with the
declarations, before any body is checked, and the type is the type its value names: with
`KEY_LEN` 32, `Inline[u8, KEY_LEN]` is `Inline[u8, 32]`, either passes for the other, and no
program can tell them apart. A length that names anything else is refused at the length: a
computed constant, whose value no type reads (LANG-20); a constant whose initialiser reads
`bytes[T]`, a field, an element or a construction, which would need layouts before types
exist; a constant whose type is not an integer type or whose value is negative; and a name
that is no constant. A constant refused itself is refused once, at the constant. No length
depends on `target`: a type is the same on every target. A length constant's value is part of
every type that names it, as a literal's is, so changing the value changes those types, and
whatever is keyed by them, as editing the literal would. The count of `[x; n]` and a vector's
lane count (SEM-11) stay integer literals.
`T` is plain or own, and the array has `T`'s kind and its bounds
([core](core.md#built-in-types)); a type parameter is a valid `T` only when its bounds prove
it plain, and `n = 0` changes neither. `[a, b, c]` builds one from its elements, evaluated
left to right: with an expected `Inline[T, n]` it has exactly `n` elements, each checked
against `T`, and without one every element synthesises the same `T`; `[]` needs an expected
type. `[x; n]` evaluates `x` exactly once, even when `n` is 0, and needs a plain `x`. Either
form may be a constant ([LANG-20](language.md#lang-20-constants)). A construction
interrupted by a failure cleans its completed elements in reverse. `a[i]` is a place, read
through a view and assigned or replaced like a field, never moved out of (OWN-14); it fails
with `Bounds` when `i` is not below `n`. `for` iterates it from index 0 (SEM-6). An own
array is cleaned from index 0. Region data holds no `Inline` array: a compact type has no
`Inline` field in v1 and uses `FixedSpan` ([MEM-9](memory.md#mem-9-compact-types)). The length is part of the type, and no code is
generic over it in v1.

```hirsel
const POW10: Inline[u64, 4] = [1, 10, 100, 1000]
fn scale(x: u64, k: u64) -> u64 { x * POW10[k] }
```
```hirsel
fn first(xs: Inline[u64, 3]) -> u64 { xs[3] }              // fails with Bounds when called
```
```hirsel
const KEY_LEN: u64 = 32
const BLOCK: u64 = KEY_LEN * 2
type Key { data: Inline[u8, KEY_LEN], pad: Inline[u8, BLOCK] }
fn raw(k: Key) -> Inline[u8, 32] { k.data }                // the same type as Inline[u8, KEY_LEN]
```
```hirsel
fn four() -> u64 { 4 }
const N: u64 = four()
type T { xs: Inline[u8, N] }                               // refused: SEM-9, N is computed
```

Version note (2026-11-26, comptime-phase5-2883; COMPTIME-DIRECTION decision 8 and T2.1,
ruled A; built, and its examples are checked): an `Inline` length may name a length
constant. It was "an integer literal". Decision 8 admits a manifest constant expression;
this admits a name only, which needs no change to the grammar, and `Inline[u32, A * B]` is
written `const N: u64 = A * B` and `Inline[u32, N]`. A length constant excludes `bytes[T]`,
projections and indexing because types are lowered before layouts exist. Reversible before
1.0: widening to expressions, or to `bytes[T]` once layouts can be ordered before the types
that need them, turns refusals into acceptances and changes no accepted type.

### SEM-10: Codecs

A **codec** is the encoding, the decoding and the schema of a type that has the `codec`
bound ([OWN-4](ownership.md#own-4-bounds)). The standard library's `codec` module gives one
as a plain value of type `Codec[T]`: `codec.of`, declared `fn of[T: codec]() -> Codec[T]`,
admits nothing, and a call at a type without the bound is refused at that call
([LANG-12](language.md#lang-12-generics)), so generic code that needs a codec writes `T:
codec`. Its `T` is fixed as every type argument is, by the expected type or an earlier
argument (`let c: codec.Codec[u8] = codec.of()`); `of[T]` names an instance in prose, never a
call. Each call resolves at its instance's `T`: to the built-in codec of a scalar, `()`,
`String`, `Never` or a tuple, and to the codec generated from the declaration of a nominal
type.

**Tuples and `Never`.** A tuple's codec is componentwise: it encodes its components in
order, each with its own codec, decodes them in the same order, and its schema is the
sequence of theirs, so a tuple is `codec` exactly when each component is, and `(u8, String)`
and `(String, u8)` are different schemas. `Never` is `codec` with zero valid encodings:
no bytes decode to a `Never`, and an item that would carry one is `Invalid` at the place it
begins. Its schema is its own, equal to no other type's, `()` included. So `Result[T,
Never]` is encoded as every `Result` is: an `Ok` is tag 0 and its `T`, unchanged, and tag 1,
which would carry the `Never` of an `Error`, decodes as `Invalid(i)`, `i` being where the
tag begins. Its schema is `Result` over T's and Never's, distinct from that of `Result[T,
E]` for every other `E`, so bytes written as one never resolve as the other: a changed error
type is a schema change, as a changed field type is. A nominal type's codec is generated from its declaration as ordinary checked code,
which visits its fields in declaration order and its variants in declared order. The byte
format, the framing of a value among others and the encoding of a schema are the library's;
this rule fixes what every codec does, and how scalars are encoded: integers at their
width, a float as its IEEE bits and a `char` as its scalar value in a `u32`, all
little-endian, as compact layout fixes them ([MEM-9](memory.md#mem-9-compact-types)). A
float round-trips exactly, since SEM-2 has one NaN. A variant's **tag** is its position
among its declaration's variants, removed ones included, counting from 0, encoded as a
`u32` whatever the number of variants, so a variant appended later leaves every earlier
tag, and the bytes that carry it, unchanged.

**Defaults.** A field of a record whose definition declares `codec` may give a **default**
after its type, `pinned: bool = false`: a literal, or `-` and an integer literal
([LANG-3](syntax.md#lang-3-literals)), typed by the field's type as the initializer of a
`let` with that type is, so `n: u8 = 300` is refused at the literal. A default on a field
of a definition that does not declare `codec` is refused, at the default. Construction
never reads a default ([LANG-9](language.md#lang-9-nominal-types)); only decoding does.

**Override.** The one way to replace a generated codec is `via`, written in the declaration
after its bounds: `pub opaque type Email: codec via String (to: text, from: parse) { text:
String }`. So only the declaring module can write it, as only it can declare the bound.
`R`, the type after `via`, must be `codec`, checked as a field's type is against the
declaration's bounds ([OWN-4](ownership.md#own-4-bounds)). A `via` codec encodes only `R`.
The actual fields need no codec of their own. OWN-4 still governs which types can
declare `codec` and requires the actual fields to be `inert` (an empty holds set,
[OWN-2](ownership.md#own-2-type-summaries)), so a decoded value holds no capability or
other authority.
`to` and `from` name functions of the declaring module: `to` takes the value as a view and
returns a view of an `R` it holds, `fn text(e: Email) -> View[String] in e`, and `from` is
`fn(take R) -> Result[T, E]` for some `E`. Encoding writes `to`'s `R` with R's codec, so a
value's bytes are its `R`'s bytes. Decoding decodes an `R` under R's contract (below), then
calls `from` once; on an `Error` it drops the error and returns `Value(i)`, `i` being where
the `R` began. The schema is the type's definition path ([PROG-1](programs.md#prog-1-packages))
over R's schema: resolution requires the writer's path to equal the reader's and then
resolves R's, so a changed `R` is a schema change of `R`, and a changed path needs a
migration program. No other codec form exists, and no owner writes bytes. A type that
validates after a derived decode is written `via` a private record that derives its codec,
as `Range` below.

**Revision.** A codec's encoder, decoder and describer are described functions
([PROG-9](programs.md#prog-9-code-descriptors)): a hand-made codec is
`codec.make(code.describe(e), code.describe(d), code.describe(w), least)`, a key codec
`codec.key_make(code.describe(e), code.describe(d), code.describe(w))`, and a generated one
describes the functions its declaration generates. So `codec.revision(c)` (and
`codec.key_revision(k)`) returns the three parts' code revisions in that order, 96 bytes, or
None when the program was assembled without revisions. A part's code address
([PROG-10](programs.md#prog-10-code-addresses)) covers every default literal, every `via`
`to` and `from` body and the codec of every field type the part reaches, so a change to any of
them changes the revision even where the schema, which holds no default and no body, stays
equal. The revision names the code a codec runs, so a change in how std is lowered, or in
what its parts charge ([MEM-30](budgets.md#mem-30-step-bounds)), changes it although no byte
does. Stored data is keyed on the storage fingerprint below instead.

**Storage fingerprint.** `codec.fingerprint(c)` (and `codec.key_fingerprint(k)`) returns 32
bytes that name what `c` stores: the bytes it encodes each value to, the value it decodes
each accepted byte string to under every writer schema it resolves, which bytes it refuses,
and, for a key codec, the order of its key bytes. Two codecs with equal fingerprints agree on
all of that. A codec has one of two forms, and its fingerprint says which:

- **Structural.** The built-in codecs that `of` and `key_of` resolve to, the std codecs of
  `List`, `Bytes`, `Option`, `Map`, `Result`, tuples, `Desc` and the scalars, and every
  codec the checker generates from a declaration without `via`. Their fingerprint is a hash of
  a **canonical descriptor** (below), whose structural nodes hold no code revision. So a
  conforming change to how a rule is lowered, or to what it charges, leaves the fingerprint
  of a codec that reaches no opaque part and no `via` node equal.
- **Opaque.** Every codec whose bytes come from code that no std rule describes: a codec
  returned by the public `codec.make`, `codec.make_derived` or `codec.key_make`, whatever its
  parts are, and the part of a `via` codec that `to` and `from` decide. Its fingerprint is a
  separately tagged hash, or for `via` a separately tagged node, over the exact code
  revisions (PROG-9) of that code, so any edit that changes their checked IR, or anything
  they reach, changes it, as the revision does.

Only std's own structural makers and the codecs the checker generates assign a structural
descriptor. A program cannot hand one to a codec it builds from parts: the public makers
always give the opaque form, and generated codecs reach the structural form through a maker
of std's own, not through `make_derived`. So no handwritten encoder or decoder can claim the
descriptor of a structural codec, and a changed opaque body always changes the fingerprint.

The descriptor is a byte string built from these items: `u8` and `u32` little-endian
numbers, a **text** written as its UTF-8 byte count in a `u32` and then its bytes, and a
fingerprint as its 32 bytes. Each **node** names one codec. It starts with its **family**,
a text, and the family's **rule version**, a `u32`, then gives the family's inputs:

| family | inputs after the family and version |
|---|---|
| `unit`, `never`, `bool`, `char`, `u8` … `u64`, `i8` … `i64`, `f32`, `f64`, `string` | none |
| `list` (`Bytes` is `list` over `u8`), `option`, `desc` | the element's reference |
| `map` | the key's reference, then the value's |
| `result` | the `Ok` type's reference, then the `Error` type's |
| `tuple` | the component count `n`, then `n` references in order |
| `record` | the definition path (PROG-1) as a text, the entry count, then each entry in declared order, tombstones included: `0` for a field or `1` for a tombstone, its name as a text, its type's reference, then `0`, or `1`, a `u32` byte count and the default's bytes: the literal encoded by the field type's codec |
| `sum` | the definition path, the variant count, then each variant in declared order, so its tag is its position: `0` or `1` for removed, its name, the payload count and the payload references |
| `via` | the definition path, then the reference of `R`, then the code revisions of `to` and of `from`, 32 bytes each |

`Desc` is the one `via` type std declares, and it takes the `desc` family, not a `via` node:
its `to` and `from` are part of std's rule. A key codec's node uses the same items under key
families, which name the key byte rules: `key.unit`, `key.bool`, `key.u8` … `key.i64`,
`key.bytes`, `key.string`, `key.desc` with its element's reference, and `key.tuple` with its
count and references, the forms `key_of` selects. A key form std adds later gets a key family
of its own. A family's
inputs are exactly what changes its stored behaviour beyond its rule: its children, its
declaration and every parameter a value depends on. Nothing else enters. The `least` hint,
the `empty` flag, the order in which parts are called, steps, charges and allocation do not.

A **reference** is `0` and the child's fingerprint, or `1` and a `u32` `k` for member `k` of
the node's own group; a node of any family may hold either form. A group is how a recursive
codec gets a canonical form. The **descriptor graph** of one fingerprint has a vertex for each
concrete node it reaches, one per closed instance (PROG-5), built-in constructor instances
such as `List[DTree]` included, and an edge from a node to each child it references. Its
strongly connected components are found as PROG-10 partitions functions. A built-in node's
children are its type arguments (for `Bytes`, `u8`), whose type texts are shorter than its
own, so every cycle passes through a nominal instance (a record, a sum or a `via` type). Each
component that holds
a nominal instance is a group: a type that refers to itself, directly or through other types
or built-in constructors, is in a group with every node on those cycles, and a nominal type on
no cycle is a group of one. Every other component is a single built-in node on no cycle, which
belongs to no group. A group lists its members, built-in nodes included, in ascending byte
order of their type text, the definition path and type arguments as PROG-9's identity text
writes them, which never ties. A member's node refers to a member of its own group by `1` and
`k`, and to anything else by `0` and its fingerprint, so groups are hashed dependency-first.
With `H` as in PROG-10, a node in no group has fingerprint `H("hirsel.codec-storage.v1", 0 ‖
node)`. A group's hash is `H("hirsel.codec-group.v1", n ‖ nodes)` with `n` its member count as
a `u32` and the members' nodes in order, and member `k`'s fingerprint is
`H("hirsel.codec-storage.v1", 1 ‖ group hash ‖ k)`, `k` a `u32`. A key codec's are the same
under `hirsel.key-storage.v1` and `hirsel.key-group.v1`. A node's component is fixed by the
nodes it reaches, so its fingerprint is the same whichever codec reached it, and neither the
order nor the numbering depends on how the instances were found: the same declarations give
the same fingerprints in every program, on every engine and at every optimisation level. A
codec that reaches no cycle has the fingerprint it would have if groups held only nominal
instances.

For example, in a package `app@1` whose module `main` declares

```hirsel
type DTree: codec { label: u64, kids: List[DTree] }

type DExpr: codec = Lit(i64) | Call(String, List[DArg])
type DArg: codec = Pos(DExpr) | Named(String, Option[DExpr])
```

`DTree` and `List[DTree]` reach each other, so they are one group. Ordered by type text,
member 0 is `app@1/main/DTree` and member 1 is `core@1/core/List[app@1/main/DTree]`. Member
0's node is the `record` family and version, the path, the entry count 2, then `0`, `label`,
`0 ‖ fingerprint(u64)`, `0` (no default), then `0`, `kids`, `1 ‖ 1`, `0`. Member 1's node is
the `list` family and version, then `1 ‖ 0`. With `G = H("hirsel.codec-group.v1", 2 ‖ node0 ‖
node1)`, `fingerprint(DTree)` is `H("hirsel.codec-storage.v1", 1 ‖ G ‖ 0)`, and a table whose
values are `List[DTree]` stores under member 1's, `H("hirsel.codec-storage.v1", 1 ‖ G ‖ 1)`.
`u64` is on no cycle and is referenced by its fingerprint. `DExpr` reaches `DArg` through
`List[DArg]`, and `DArg` reaches `DExpr` directly and through `Option[DExpr]`, so the group has
four members: 0 `app@1/main/DArg`, 1 `app@1/main/DExpr`, 2
`core@1/core/List[app@1/main/DArg]` and 3 `core@1/core/Option[app@1/main/DExpr]`. `DArg`'s
`Pos` payload is `1 ‖ 1` and its `Named` payloads are `0 ‖ fingerprint(String)` and `1 ‖ 3`;
`DExpr`'s `Call` payloads are `0 ‖ fingerprint(String)` and `1 ‖ 2`; `List[DArg]` holds `1 ‖ 0`
and `Option[DExpr]` holds `1 ‖ 1`. `i64` and `String` are on no cycle. A record `Forest {
trees: List[DTree] }` is a group of one whose `trees` entry is `0 ‖` member 1's fingerprint
above.

An opaque codec's fingerprint is `H("hirsel.codec-opaque.v1", e ‖ d ‖ w ‖ least ‖ empty)`:
its three parts' code revisions, `least` as a `u64` and `empty` as a `u8`. An opaque key
codec's is `H("hirsel.key-opaque.v1", e ‖ d ‖ w)`. A `via` node holds the revisions of `to` and
`from`, so the `via` part of a fingerprint is never structural: an edit to `from`, or to a
helper that `from` reaches, changes it, and the reference to `R` gives `R`'s own form. A
fingerprint is None exactly when the program was assembled without revisions and the codec
reaches an opaque part or a `via` node; a purely structural fingerprint is always available.

**Rule versions.** Std owns one rule version per family, each starting at 1 and recorded
beside the rule in `std.codec`. A family's rule is its byte format, its framing, its order of
key bytes, its schema resolution (defaults, tombstones, appended variants, tags) and the set
of byte strings it refuses. A change to any of these, for any value or any writer schema,
increments that family's version in the same change, and a version is never reused; no other
change does, so a lowering, a charge, a step count or an allocation pattern never does. A
version only ever adds an input to a hash. It never replaces a revision, so it can make a
fingerprint change but can never keep an opaque fingerprint equal across a changed body.
Programs declare no storage version of their own. Store's frozen format corpus
(`test_frozen_format_corpus`) opens and decodes every stored value and key after an upgrade,
so a rule change without its version increment shows up there.

Version note (2026-11-27, storage fingerprint; not yet built): stores keyed indexes on
`codec.revision` and `codec.key_revision`, so every toolchain change to a reached std body
forced a rebuild although no stored byte changed (STORE-SPEC v2.172). The revisions stay, with
their 96 bytes, for consumers that name code; `fingerprint` and `key_fingerprint` are added.
The same day, before any build, groups were corrected from components over nominal instances
alone to components over every descriptor node: a recursion through `List` or another
built-in constructor had no canonical form, since the constructor's node needed the group's
hash. Fingerprints of codecs that reach no cycle are unchanged by the correction.

**Old data.** Bytes are decoded against the **writer's schema**, the schema of the
declaration that encoded them, which the library keeps with them. Decoding resolves the
writer's schema against the reading declaration's, matching fields and variants by name,
so the order of fields may differ, and resolving each matched type in the same way at every
depth. Exactly three changes survive old data:

- a field added with a default: a field the writer's schema lacks, or records as removed,
  takes its default;
- a field or variant removed by a **tombstone**, `removed note: String` among the fields or
  `removed Old(u64)` among the variants, written in place of what it removes: a writer's
  field that matches a removed field is skipped, and a writer's variant that matches a
  removed variant resolves, while a value that actually holds it is `Schema`;
- a variant appended after the last: old bytes carry only earlier tags, which are
  unchanged.

Every other difference is `Schema`: a field without a default that the writer lacks, a
writer's field or variant that the reader neither has nor removed, a matched variant at
another position, and a matched type that does not resolve. So renaming or retyping a
field, removing one without a tombstone, and reordering variants or inserting one before
the last need an explicit migration program that decodes with the old declaration and
builds values of the new one.

A removed field or variant is not a field or variant of the type: construction, patterns,
field access and layout never see it, and it keeps its name taken. At the declaration the
checker verifies that `removed` appears only in a definition that declares `codec` without
`via`, that a tombstone's name is the name of no field, variant or other tombstone of the
declaration, so a field of that name is refused there, that a removed field has no
default, and that a tombstone's types are `codec`. The checker sees one declaration, so a
tombstone stays for as long as old data may hold what it removed; deleting it frees the
name. At resolution the reader verifies what the writer's schema shows: each writer's field
and variant matches a field, variant or tombstone of the reader by name, the writer's types
resolve against the matched ones, a tombstone's included, each matched variant has the
writer's tag, and each field the writer lacks or removed has a default. A schema records its
tombstones, so data whose writer removed a variant resolves against a reader that still has
it.

**Encoding.** A count that exceeds the format's u32 field returns
`Error(CountOverflow(count))` before writing the count. Refused admission raises
`Capacity` ([MEM-21](budgets.md#mem-21-admission)). Allocation refusal on append
leaves the prefix whose writes completed.

**Decoding.** A decode returns `Result[T, DecodeError]`, where
`DecodeError = Truncated(u64) | Invalid(u64) | Utf8(u64) | Duplicate(u64) | Trailing(u64) |
Schema(u64) | Value(u64)`. Each `u64` is a byte offset, into the writer's schema for `Schema` and into
the decoded bytes otherwise:

- `Truncated(i)`: the item that starts at `i` runs past the end of the bytes;
- `Invalid(i)`: the item at `i` encodes no value of its type: a variant tag or `bool` past
  its values, a tag whose variant carries a `Never`, a float NaN other than SEM-2's canonical one, or a `u32` that is not a
  Unicode scalar, so not a `char` (`char.from_u32`);
- `Utf8(i)`: a `String`'s bytes stop being UTF-8 at `i`, the first byte not part of a
  scalar;
- `Duplicate(i)`: the `Map` key that starts at `i` equals an earlier key of the same map;
- `Trailing(i)`: the value ends at `i`, before the bytes do;
- `Schema(i)`: the part of the writer's schema at `i` does not resolve, or is a removed
  variant that the value holds, as above;
- `Value(i)`: the owner's `from` refused the `R` that starts at `i`, as above.

Decoding accepts exactly the bytes that encoding produces: against one schema, bytes that
no value encodes to are an error, and no two accepted byte strings decode to the same
value. So a decoder never canonicalizes, and `Invalid`, `Utf8`, `Duplicate` and `Trailing`
are each a way that bytes fall outside that image. For a `via` type the image is the `R`s
that `from` accepts, and the law holds when `from` accepts exactly the `R`s that `to`
returns and inverts it; the checker cannot verify that, so it is the owner's obligation.

Every decode, whatever its bytes:

- never fails ([FAIL-1](failure.md#fail-1-errors-failures-and-run-stops)) unless an owner's
  `from` does or an admission is refused: every malformed input is a `DecodeError`;
- has no fixed limit on nesting: it keeps the values still being built on a work stack in
  storage it admits from the current limit ([MEM-21](budgets.md#mem-21-admission)), never
  on the call stack, so depth costs storage and never exhausts the stack
  ([FAIL-15](failure.md#fail-15-run-stops)), and the one resource failure is `Capacity`
  ([MEM-21](budgets.md#mem-21-admission));
- leaves nothing behind on an error or a refusal: before it returns an `Error` or raises
  `Capacity`, it has released and refunded every block it admitted, its work stack included,
  and the bytes, which it only views, are unchanged;
- builds fresh ownership: the value shares no storage with the bytes and, as no codec type
  holds one, no capability, resource, view or region reference.

```hirsel
pub type Row: codec { id: u64, name: String, pinned: bool = false }
```
```hirsel unbuilt
fn rows() -> codec.Codec[Row] { codec.of() }   // bytes written before `pinned` decode with false
```
```hirsel
type Pair[T]: codec { a: T, b: T }
```
```hirsel unbuilt
fn files() -> codec.Codec[Pair[HostFile]] { codec.of() }   // refused: LANG-12, HostFile is not codec
```
```hirsel
type Note { pinned: bool = false }                    // refused: SEM-10, Note does not declare codec
```
```hirsel
pub opaque type Email: codec via String (to: text, from: parse) { text: String }
fn text(e: Email) -> View[String] in e { view e.text }
fn parse(take s: String) -> Result[Email, String] {
  match string.next_scalar(s, 0) { Some(_) => Ok(Email(text: s)), None => Error(s) }   // "" is Value(i)
}
```
```hirsel
type RangeWire: codec { lo: u64, hi: u64 }
pub opaque type Range: codec via RangeWire (to: wire, from: check) { w: RangeWire }
fn wire(r: Range) -> View[RangeWire] in r { view r.w }
fn check(take w: RangeWire) -> Result[Range, ()] { if w.lo <= w.hi { Ok(Range(w: w)) } else { Error(()) } }
```
```hirsel
pub type Row: codec { id: u64, removed note: String, name: String, pinned: bool = false }
pub type Shape: codec = Circle(u64) | removed Square(u64) | Line(u64, u64)   // Line has tag 2
```
```hirsel
pub type Tag: codec { id: u64, removed note: String, note: u64 }   // refused: SEM-10, note is removed
```

### SEM-11: Vectors

`Simd[T, n]`, where `T` is an integer type, `n` is an integer literal and n × size(`T`) is
16 or 32, is a **vector** of `n` **lanes** of type `T`, numbered from 0. It is plain and
copyable, `shareable` and `send` and nothing else, and its size is n × size(`T`). Any other
`T` or `n` is refused at the type. A vector type is the type of a local, a parameter, a
result or an expression and of nothing else: in a field, an element, a tuple, a type
argument, a callable type or a capture it is refused.

Each operation below is typed by its own rule at the call, as `int.widen` is
([SEM-1](#sem-1-integers)): none is a generic function, and `n`, where the rule says so, is
taken from the expected type. Each computes each lane by SEM-1 at `T`'s width, and none
depends on the engine, the build or the CPU.

- **Bitwise.** `& | ^` and unary `~` work lane by lane. `<<` and `>>` shift every lane by
  one count of type `T`, which fails with `Overflow` once, at the operator, when it is
  negative or at least `T`'s width ([LANG-21](language.md#lang-21-operators)).
- **Arithmetic** is named and never fails: `simd.wrapping_add`, `simd.wrapping_sub`,
  `simd.wrapping_mul`, `simd.saturating_add` and `simd.saturating_sub` take two vectors of
  one type and apply their `int` namesakes to each lane. `+`, `-`, `*`, `/`, `%`, unary `-`
  and the comparison operators are refused on vectors.
- **Widening and narrowing.** Each product below is exact: an `i16` times an `i16` always
  fits in an `i32`.
  - `simd.mulhi_i16(a, b)` takes two `Simd[i16, n]` and returns a `Simd[i16, n]` whose
    lane i is the top 16 bits of the 32-bit product of lane i of `a` and lane i of `b`,
    that is the product shifted right by 16 with its sign kept (rounding toward negative
    infinity). Its low 16 bits are lane i of `simd.wrapping_mul(a, b)`.
  - `simd.madd_i16(a, b)` takes two `Simd[i16, n]` and returns a `Simd[i32, n/2]` whose
    lane i is `a[2i] × b[2i] + a[2i+1] × b[2i+1]` modulo 2^32 as an `i32`. The sum
    leaves the `i32` range only when all four lanes are −32768, and gives −2^31 there.
  - `simd.pack_saturating(a, b)` takes two `Simd[S, n]` and returns a `Simd[T, 2n]`, `T`
    taken from the expected type: `T` is `i8` or `u8` with `S` = `i16`, or `i16` or `u16`
    with `S` = `i32`, and any other `T` is refused. Lane i comes from lane i of `a` when
    i < n and from lane i − n of `b` otherwise, across the whole vector, and is clamped to
    `T`'s range: a negative lane packs to 0 when `T` is unsigned.

  On x86-64 these are `pmulhw`, `pmaddwd`, `packsswb`, `packuswb`, `packssdw` and
  `packusdw`; at v3 the 32-byte forms are the `v` forms, whose results lie in two 16-byte
  halves, so a pack's lanes are put in order by a `vpermq` after it. On ARM64 `mulhi_i16`
  is `smull` and `smull2` followed by `uzp2` of their 16-bit halves (not `sqdmulh`, which
  doubles and saturates), `madd_i16` is `smull` and `smull2` followed by `addp`, which
  adds modulo 2^32, and the packs are `sqxtn` and `sqxtn2` for a signed `T` and `sqxtun`
  and `sqxtun2` for an unsigned one.
- **Masks.** `simd.eq(a, b)` and `simd.lt(a, b)` take two vectors of one type and return a
  **mask** of that type, whose lanes are all ones where the lanes compare true and 0 where
  they do not; `lt` compares as `T`, signed or unsigned.
- **Making.** `simd.splat(x)` puts the `T` `x` in every lane of the expected vector type, and
  `simd.from(a)` turns an `Inline[T, n]` into a `Simd[T, n]`, lane i from element i.
- **Swizzle.** `simd.swizzle(t, i)` takes a table `t: Simd[T, m]`, where `T` is `u8` or `i8`,
  and indices `i: Simd[u8, n]`, where `m` and `n` are each 16 or 32, and returns a
  `Simd[T, n]` whose lane k is lane `i[k]` of `t` when `i[k] < m`, and 0 otherwise.
- **Cast.** `simd.cast(v)` reinterprets a vector as the expected vector type, which has the
  same size: the lanes are laid out from byte 0 upward, each lane's bytes little-endian, on
  every target.
- **Reductions.** `simd.any(v)` is `true` when some lane is not 0. `simd.bitmask(v)` is a
  `u64` whose bit k is the top bit of lane k; the bits from `n` upward are 0.
- **Loads and stores.** `list.lanes(xs, at)`, with `xs: List[T]` and `T` an integer type,
  reads the `n` elements of `xs` from `at` as a `Simd[T, n]`, lane i from element `at + i`,
  `n` taken from the expected type, and fails with `Bounds` unless `at + n ≤ len` with no
  overflow, checked once for the whole read. `at` counts elements, so lanes are elements and
  no byte order enters. `list.put_lanes(mut xs, v, k)` appends lanes 0 to `k − 1` of a
  `Simd[T, n]`. It fails with `Bounds` when `k > n`, before changing anything; otherwise it
  grows `xs` as [MEM-24](budgets.md#mem-24-logical-charges) says with `needed` = `len + k`,
  and a refused growth leaves `xs` unchanged.
  `list.set_lanes(mut xs, at, v)` overwrites the `n` elements of `xs` from `at` with the
  lanes of a `Simd[T, n]` under the same check, made before any write, and keeps the length.
  `inline.lanes(a, at)` and `inline.set_lanes(mut a, at, v)` do the same on an
  `Inline[T, N]`, failing with `Bounds` unless `at + n ≤ N` with no overflow. A vector whose
  lane type is not the element type, or an element type that is not an integer, is refused
  at compile time. No load or store assumes an alignment: an engine reads and writes the
  elements wherever the List's backing or the array lies.

Each `simd` and `list` operation is a core call and takes one step, and the operators take
none ([MEM-30](budgets.md#mem-30-step-bounds)).

A function is **versioned** when, after it is type-checked, any of its parameters, its
result, a binding or an expression in it has a vector type. That is decided one function at
a time. A closure literal is a function of its own ([LANG-14](language.md#lang-14-captures)):
its own vector values version it, and it does not version the function that encloses it. A
function with a vector in its signature is called only from versioned functions. An engine
may build a versioned function more than once, one **version** for each instruction-set
level it supports. A call between versioned functions runs the callee's version at the
caller's level. Every other call, through a function value, an export or a task entry
included, enters through the function's **stub**, which has no vector in its signature and
runs the version for the widest level the CPU allows. On x86-64 the native baseline is
x86-64-v2, and the available versions are v2 and v3 of the x86-64 psABI. Native release
code uses v2 for unversioned functions. Before any program code runs, the launcher checks
the full v2 feature set and reports that the CPU is unsupported if a feature is unavailable.
An engine selects level k only when the CPU has every feature the psABI names for level k,
with the operating system's state for them enabled. It builds level k's version with
exactly those features. The launcher and the code generator use the same level table.

A version has no meaning of its own. By [SEM-8](#sem-8-determinism) every version gives the
same values, failures and sites, and one build's versions take the same steps and charges, so a
run on a CPU with other levels behaves the same. Nothing a program can observe names a version, and every version has
the function's revision ([PROG-9](programs.md#prog-9-code-descriptors)).

```hirsel
const LO: Inline[u8, 16] = [0x15, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x13, 0x1A, 0x1B, 0x1B, 0x1B, 0x1A]
const HI: Inline[u8, 16] = [0x10, 0x10, 0x01, 0x02, 0x04, 0x08, 0x04, 0x08, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10]
// true when some byte of v is outside the Base64 alphabet
fn invalid(v: Simd[u8, 32]) -> bool {
    simd.any(simd.swizzle(simd.from(LO), v & simd.splat(15)) & simd.swizzle(simd.from(HI), v >> 4))
}
// versioned, though it writes no vector type: list.lanes(b, at) is a Simd[u8, 32]
fn invalid_at(b: Bytes, at: u64) -> bool { invalid(list.lanes(b, at)) }
```
```hirsel
fn sum(a: Simd[u8, 16], b: Simd[u8, 16]) -> Simd[u8, 16] { a + b }     // refused: LANG-21, use simd.wrapping_add
```
```hirsel
type Block { lanes: Simd[u8, 16] }                                       // refused: SEM-11, a vector in a field
```
```hirsel
fn wide(v: Simd[u64, 8]) -> Simd[u64, 8] { v }                          // refused: SEM-11, 64 bytes
```
```hirsel
fn pick(t: Simd[u16, 16], i: Simd[u8, 16]) -> Simd[u16, 16] { simd.swizzle(t, i) }   // refused: SEM-11, a swizzle table of u16
```
