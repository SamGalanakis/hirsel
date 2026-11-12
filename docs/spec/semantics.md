# Semantics

The meaning of the built-in values and operations. Every operation here means the same
thing on every engine and build.

### SEM-1: Integers

The integer types are `i8` to `i64` and `u8` to `u64`. `+`, `-`, `*` and unary `-` fail
with `Overflow` when the result does not fit ([FAIL-2](failure.md#fail-2-causes)). Other
arithmetic is named: `int.wrapping_add`, `int.checked_add` (an `Option`),
`int.saturating_add` and their siblings. `/` truncates toward zero and `%` takes the
dividend's sign; division or remainder by zero fails with `Division`, and `MIN / -1` fails
with `Overflow`. A shift count has the left operand's type; a count that is negative or at
least the width fails with `Overflow`; `<<` discards high bits, `>>` sign-extends signed
and zero-extends unsigned values. Operands of a binary operator have one type; there are no
mixed-width operators. Conversions are named and take their target from the expected type: `int.widen` exists
only where every value fits; `int.narrow` fails with `Overflow`; `int.try_narrow` returns an
`Option`; `int.truncate` keeps the low bits.

```hirsel
fn add(x: u8) -> u8 { int.wrapping_add(x, 100) }
```
```hirsel
fn add(x: u8, y: u16) -> u16 { x + y }                    // refused: SEM-1, mixed widths
```

### SEM-2: Floats

`f32` and `f64` are IEEE 754 binary32 and binary64 with round-to-nearest-even and no mode
state. Expressions are evaluated as written: no contraction, reassociation or wider
intermediates; `float.mul_add` fuses explicitly. Every NaN an operation produces has one
canonical bit pattern per type, so a NaN's bits are the same on every engine.
`float.to_i64` and its siblings return a `Result`. Floats are neither `key` nor `ordered`;
`float.total_order` orders them for sorting.

### SEM-3: Text

`char` is a Unicode scalar value. `String` is own, growable and always valid UTF-8; `Text`
is its stable twin; `Bytes` is own and arbitrary. Length is `byte_len`; scalars and byte
positions are iterated through views (`string.scalars(s)`, `string.positions(s)`), and
slicing at a position never splits a scalar. Equality, order and hashing are bytewise;
normalisation is explicit. String formatting is ordinary calls; there are no format
strings.

### SEM-4: Lists

`List[T]` is own and growable, with the capacity-based layout of
[MEM-13](memory.md#mem-13-own-layout). `xs[i]` and `list.get(xs, i)` read through a view and
fail with `Bounds`, or return `None`, when `i` is out of range. `list.push` and the other
growing operations return `Result[_, (Capacity, T)]` or `Result[_, Capacity]` and grow by
doubling ([MEM-26](budgets.md#mem-26-logical-charges)). Removing elements never allocates. A view
returned by `list.get` or indexing is anchored to the list
([OWN-20](ownership.md#own-20-scoped-results-name-their-anchor)) and freezes it until the
view's extent ends ([OWN-10](ownership.md#own-10-freezes)); iteration freezes it for the
loop (SEM-6).

### SEM-5: Maps

`Map[K: key, V]` is own and iterates in insertion order. Insertion, lookup and removal take
amortised constant time; removal keeps the order of the remaining entries. Inserting an
existing key replaces its value in place, keeps the stored key, cleans the old value and
then drops the incoming key. `map.get(m, k)` returns `Option[View[V]] in m`
([OWN-20](ownership.md#own-20-scoped-results-name-their-anchor)). Each `key` type has exactly
one equality, the structural one of SEM-7, and one hash, which is private, has no seed and
cannot be observed; both are fixed by the type, so every map with the same key type uses the
same ones, and no program supplies or replaces them in v1 ([reserved](reserved.md), R11).
Views from `map.get` are anchored to the map and freeze it as list views do.

```hirsel
fn count(m: Map[String, u64], k: String) -> u64 { match map.get(m, k) { Some(v) => v, None => 0 } }
```

### SEM-6: Iteration

`for x in e` iterates a range or a view of a sequence, map or span; `for take x in e`
consumes an own sequence, moving each element out from the lowest index and cleaning the
rest if the loop exits early. A range `a..b` is half-open and `a..=b` closed; both are
plain and iterate upward. A sequence is frozen for the loop
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
([OWN-4](ownership.md#own-4-bounds)). Integers, `bool`, `char`, unit, `String`, `Text` and
`Bytes` are both. Tuples, `Option`, `Result`, `List` and spans have a bound when their
components do. A nominal type has a bound when it declares it and its fields have it.
Equality is structural. Order is numeric for integers and `char`, `false` before `true`,
bytewise for text, lexicographic for tuples, sequences and records (fields in declaration
order, a proper prefix first), and by declaration ordinal then payload for variants. A
counted value compares by payload. Floats, maps, callables, capabilities, resources and
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
policy, every run behaves identically on every engine, build and optimisation level: the
same values, outputs, `Capacity` results, failure causes and sites, cleanup order and run
outcome. Nothing in the language depends on addresses, timing, the host or unrecorded
scheduling.
