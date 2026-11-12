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
sign-extends signed and zero-extends unsigned values. Operands of a binary operator have one type; there are no
mixed-width operators. Conversions are named and take their target from the expected type: `int.widen` exists
only where every value fits; `int.narrow` fails with `Overflow`; `int.try_narrow` returns `Option[U]`;
`int.truncate` keeps the low bits.

```hirsel
fn add(x: u8) -> u8 { int.wrapping_add(x, 100) }
```
```hirsel
fn add(x: u8, y: u16) -> u16 { x + y }                    // refused: SEM-1, mixed widths
```

### SEM-2: Floats

`f32` and `f64` are IEEE 754 binary32 and binary64 with round-to-nearest-even and no mode
state. Expressions are evaluated as written: no contraction, reassociation or wider
intermediates; `float.mul_add` fuses explicitly. Subnormals are kept and zero is signed, as
IEEE 754 says. Every NaN an operation produces has one bit pattern: `0x7fc00000` for `f32`,
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
storage back, and `string.as_bytes` views it as bytes. Length is `byte_len`; scalars are
read with `string.next_scalar(s, at) -> Option[(char, u64)]`, which returns the scalar at
byte position `at` and the next position. `string.slice` fails with `Bounds` at a position
that would split a scalar. Equality, order and hashing are bytewise;
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
amortised expected constant time; removal keeps the order of the remaining entries.
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

`for x in e` iterates a range, or a view of a `List`, a `FixedSpan` or a `Causes`
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
are both. Tuples, `Option`, `Result`, `List` and `FixedSpan` have a bound when
their components do. `Unique` is `key` and not `ordered`
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
