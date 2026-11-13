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
as its high and low halves, and `int.leading_zeros` the count of zero bits above the highest
one bit, the width for 0.

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
totalOrder.

### SEM-3: Text

`char` is a Unicode scalar value; `char.to_u32` and `char.from_u32` (an `Option`, `None` for
surrogates and values above `0x10FFFF`) convert it. `String` is an opaque own type over
UTF-8 storage that is always valid; `Text` is its stable twin; `Bytes` is `List[u8]`.
`string.from_utf8` validates a `Bytes` into a `String`, `string.into_bytes` gives its
owned storage back, copying a static literal into new storage and so returning `Capacity`
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
growing operations return `Result[_, (Capacity, T)]` or `Result[_, Capacity]` and grow as
[MEM-24](budgets.md#mem-24-logical-charges) says; `list.capacity` reads the capacity and
`list.reserve(mut xs, n)` grows it to at least `len + n`. Removing elements never
allocates or shrinks. A view
returned by `list.get` or indexing is anchored to the list
([OWN-20](ownership.md#own-20-scoped-results-name-their-anchor)) and freezes it until the
view's extent ends ([OWN-10](ownership.md#own-10-freezes)); iteration freezes it for the
loop (SEM-6).

### SEM-5: Maps

`Map[K: key, V]` is own and iterates in insertion order. Insertion, lookup and removal take
amortised expected constant time apart from hashing and comparing keys, which is linear in
a key's size, and from cleaning what they drop; removal keeps the order of the remaining entries.
Inserting an existing key installs the new value in place, keeps the stored key, cleans
the old value and then drops the incoming key; it admits nothing, so it never returns
`Capacity`. `map.get(m, k)` returns `Option[View[V]] in m`
([OWN-20](ownership.md#own-20-scoped-results-name-their-anchor)). `map.set` replaces the value of a present key and never inserts ([core](core.md)). Each `key`
type has exactly one equality, the structural one of SEM-7, and one hash, fixed by the type;
no program supplies or replaces them in v1 ([reserved](reserved.md), R11). The hash is keyed
by a per-run seed that enters at the replay point
([CAP-8](capabilities.md#cap-8-the-replay-point)); no operation reveals a hash and iteration
is in insertion order, so no value depends on the seed
([CAP-6](capabilities.md#cap-6-no-ambient-input)).
Views from `map.get` are anchored to the map and freeze it as list views do.

```hirsel
fn count(m: Map[String, u64], k: String) -> u64 { match map.get(m, k) { Some(v) => v, None => 0 } }
```

### SEM-6: Iteration

`for x in e` iterates a range, or a view of a `List`, an `Inline` array, a `FixedSpan`, a
`SeqSpan` or `IndexedSpan` ([MEM-12](memory.md#mem-12-stable-sequences)) or a `Causes`
([core](core.md)), binding a view of each element from the lowest index. Over a view of a
`Map` it binds `(View[K], View[V])` for each entry in insertion order. `for take x in e` consumes an own `List`, moving each
element out from the lowest index and cleaning the rest if the loop exits early. Nothing
else is iterable; scalars are read with `string.next_scalar` (SEM-3). A range `a..b` is
half-open and `a..=b` closed; both are plain, iterate upward, and stop at the type's
maximum without overflow. The source is frozen for the loop
([OWN-10](ownership.md#own-10-freezes)), so iteration never observes a change.

```hirsel
fn sum(xs: List[u64]) -> u64 { let mut t: u64 = 0; for x in xs { t += x }; t }
```
```hirsel
fn grow(mut xs: List[u64]) -> Result[(), Capacity] {
  for x in xs { list.push(mut xs, x)? }; Ok(())            // refused: OWN-10, xs frozen by the loop
}
```

### SEM-7: Equality and order

`==` and `!=` need `key` operands; `<`, `<=`, `>` and `>=` need `ordered` operands
([OWN-4](ownership.md#own-4-bounds)). Integers, `bool`, `char`, unit, `String` and `Text`
are both. Tuples, `Option`, `Result`, `List`, `Inline` and the stable sequences have a
bound when their components do. `Unique` is `key` and not `ordered`
([MEM-18](memory.md#mem-18-pools-are-library-code)). A nominal type has a bound when it declares it and its fields have it.
Equality is structural. Order is numeric for integers and `char`, `false` before `true`,
bytewise for text, lexicographic for tuples, sequences and records (fields in declaration
order, a proper prefix first), and by declaration ordinal then payload for variants. A
counted value and a view compare by payload. Floats, maps, callables, capabilities, resources and
scoped types have neither bound. Comparison never allocates.

```hirsel
type P: key + ordered { x: i64, y: i64 }
fn first(a: P, b: P) -> bool { a < b }
```
```hirsel
type Q: key { w: f64 }                                     // refused: OWN-4, f64 is not key
```

### SEM-8: Determinism

Given the same program, the same values at the replay point
([CAP-8](capabilities.md#cap-8-the-replay-point)), the same root limit and the same charge
policy, every run that no run stop ends ([FAIL-15](failure.md#fail-15-run-stops)) behaves
identically on every engine, build and optimisation level: the same values, outputs,
`Capacity` results, failure causes and sites, cleanup order and run outcome. Nothing
observable depends on addresses (of blocks or of functions), on the order of declarations
in a source, on timing, on the host or on unrecorded scheduling.

### SEM-9: Inline arrays

`Inline[T, n]`, where `n` is an integer literal, is `n` values of type `T` held inline, with
no block: its size is `n` × the size of `T` and it admits nothing. `n` is a value from 0 to
`2^64 − 1`; a suffix on it is not part of the type, so `Inline[u8, 2u8]` is `Inline[u8, 2]`.
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
array is cleaned from index 0. A plain array may be a field of a stable type that is not
compact (MEM-2); a compact type has no `Inline` field in v1 and uses `FixedSpan`
([MEM-9](memory.md#mem-9-compact-types)). The length is part of the type, and no code is
generic over it in v1.

```hirsel
const POW10: Inline[u64, 4] = [1, 10, 100, 1000]
fn scale(x: u64, k: u64) -> u64 { x * POW10[k] }
```
```hirsel
fn first(xs: Inline[u64, 3]) -> u64 { xs[3] }              // fails with Bounds when called
```
