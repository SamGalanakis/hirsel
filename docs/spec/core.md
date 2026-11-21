# Core library

The core library is source: one file per core module under
[`core/core/`](../../core/core/). Each file is a module ([LANG-7](language.md#lang-7-names))
of the package `core`, which differs from any other package in one power: a function there
may end at its header, and is then provided by the implementation. A core function with a
body is ordinary code under the ordinary rules. Either way its contract is its header and
this page, and nothing is inferred from its body ([LANG-8](language.md#lang-8-signatures)).
A bodiless function that is not `pub` is visible only in its module
([PROG-2](programs.md#prog-2-visibility)) and is not part of the contract. A type declared
there with no body is built in, so its summary is the one the table below gives it.

A core function's body still resolves ordinary names:

```hirsel core
fn unknown() -> u64 { missing() }                       // refused: LANG-7, missing is not declared
```

| module | file | rules |
|---|---|---|
| `core` (the prelude: every core type, `fail`, `isolate`, `replace`, `clone`, `clone_in`, `build_in`, `build_stream_in`, `emit`, `finish`, `finish_trim`) | [core.hirsel](../../core/core/core.hirsel) | LANG-7, MEM-4, MEM-17, OWN-13, FAIL-2, FAIL-16 |
| `option`, `result` | [option](../../core/core/option.hirsel), [result](../../core/core/result.hirsel) | FAIL-2 |
| `int`, `float`, `char` | [int](../../core/core/int.hirsel), [float](../../core/core/float.hirsel), [char](../../core/core/char.hirsel) | SEM-1 to SEM-3 |
| `simd` | [simd](../../core/core/simd.hirsel) | SEM-11 |
| `check` | [check](../../core/core/check.hirsel) | SEM-8, SEM-11, MEM-30 |
| `sha1` | [sha1](../../core/core/sha1.hirsel) | SEM-8, MEM-30 |
| `sha256` | [sha256](../../core/core/sha256.hirsel) | SEM-8, MEM-30 |
| `box`, `rc`, `arc`, `unique` | [box](../../core/core/box.hirsel), [rc](../../core/core/rc.hirsel), [arc](../../core/core/arc.hirsel), [unique](../../core/core/unique.hirsel) | MEM-13 to MEM-18 |
| `list`, `string`, `text`, `map`, `span`, `dest` | [list](../../core/core/list.hirsel), [string](../../core/core/string.hirsel), [text](../../core/core/text.hirsel), [map](../../core/core/map.hirsel), [span](../../core/core/span.hirsel), [dest](../../core/core/dest.hirsel) | SEM-3 to SEM-6, MEM-3, MEM-4, MEM-12 |
| `bytebuf` | [bytebuf](../../core/core/bytebuf.hirsel) | MEM-21, MEM-28 |
| `deque` | [deque](../../core/core/deque.hirsel) | Deques and heaps, MEM-24 |
| `heap` | [heap](../../core/core/heap.hirsel) | Deques and heaps, MEM-24 |
| `seq`, `indexed`, `writer` | [seq](../../core/core/seq.hirsel), [indexed](../../core/core/indexed.hirsel), [writer](../../core/core/writer.hirsel) | MEM-4, MEM-12 |
| `task`, `channel` | [task](../../core/core/task.hirsel), [channel](../../core/core/channel.hirsel) | CONC-1 to CONC-11 |
| `latest` | [latest](../../core/core/latest.hirsel) | CONC-13 |
| `log` | [log](../../core/core/log.hirsel) | MEM-29 |
| `code` | [code](../../core/core/code.hirsel) | PROG-9 |
| `flow` | [flow](../../core/core/flow.hirsel) | CONC-14 |
| `args`, `env`, `host`, `stdin`, `stdout`, `stderr` | [args](../../core/core/args.hirsel), [env](../../core/core/env.hirsel), [host](../../core/core/host.hirsel), [stdin](../../core/core/stdin.hirsel), [stdout](../../core/core/stdout.hirsel), [stderr](../../core/core/stderr.hirsel) | PROG-3, CAP-5, CAP-7 |

A library module may extend a core module by name (LANG-7).

## Built-in types

This table is the complete summary ([OWN-2](ownership.md#own-2-type-summaries)) of every
type that is built in: the builtin type names of [LANG-10](language.md#lang-10-built-in-types)
and the bodiless declarations of the core files. A bound column never lists `plain`:
`plain` holds exactly for a type of plain kind ([OWN-4](ownership.md#own-4-bounds)), so
`List[T]` and `Box[T]` are own and never `plain`, whatever `T` is. "When T" means the type
has each of the listed bounds exactly when every type argument has that bound; `packed`
and `streamable` are never listed either, since MEM-12 says which types have them, nor
`integer` and `float`, which hold exactly for the integer and the float types; a holds
entry "T's" is the union of its type arguments' holds sets. Tuples, `Option` and `Result`
take their kind and copyability from OWN-3 and their other bounds componentwise; `Inline`
takes its kind from `T`. Sizes are logical ([MEM-24](budgets.md#mem-24-logical-charges)). A
scoped value is never stored in a field of an own or resource type, a collection or
an environment (OWN-19), so it has no heap size; a scoped type's own fields
([OWN-22](ownership.md#own-22-scoped-types)) and a callable's logical width
([LANG-13](language.md#lang-13-callables)) are unaffected.

| type | kind | bounds | size | holds |
|---|---|---|---|---|
| integers, floats | plain | `shareable`, `send`, `codec`; integers `key`, `ordered` | width | nothing |
| `bool`, `char`, `()` | plain | `shareable`, `send`, `key`, `ordered`, `codec` | 1, 4, 0 | nothing |
| tuples, `Option[T]`, `Result[T, E]` | OWN-3 | `shareable`, `send`, `key`, `ordered`, `codec` when T | tuple: sum; variant rule (MEM-24) | T's |
| `Inline[T, n]` | T's kind (plain or own) | `shareable`, `send`, `key`, `ordered` when T | n × size(T) | T's |
| `Simd[T, n]` | plain | `shareable`, `send` | n × size(T) | nothing |
| `Never` | plain | `shareable`, `send`, `key`, `ordered` | 0 | nothing |
| `String` | own | `shareable`, `send`, `key`, `ordered`, `codec` | 24 | nothing |
| `FrozenBytes` | own | `shareable`, `send` | 24 | nothing |
| `List[T]` | own | `shareable`, `send`, `key`, `ordered`, `codec` when T | 24 | T's |
| `Map[K, V]` | own | `shareable`, `send`, `codec` when K and V | 24 | K's and V's |
| `Deque[T]` | own | `shareable`, `send` when T | 32 | T's |
| `Heap[T]` | own | `shareable`, `send` when T | 24 | T's |
| `Box[T]` | own | `shareable`, `send`, `key`, `ordered` when T | 8 | T's |
| `Rc[T]` | counted | `shareable`; `key`, `ordered` when T | 8 | T's |
| `Arc[T]` | counted | `shareable`, `send`; `key`, `ordered` when T | 8 | T's |
| `View[T]` | scoped, copyable | `key`, `ordered` when T | | scoped value, and T's |
| `Region`, `Dest[T]`, `Writer[T, in r]` | scoped; `Dest` and `Writer` move | none | | scoped value |
| `Text` | region | `key`, `ordered` | 16 | region reference |
| `FixedSpan[T]`, `SeqSpan[T]`, `IndexedSpan[T]` | region | `key`, `ordered` when T | 16 | region reference, and T's |
| `SeqIndex[T, in a]` | region | none | 16 | region reference, and T's |
| `Link[T]` | a compact field only (MEM-10) | none | its holder's `W` | nothing |
| `Child[T]` | plain | `shareable`, `send`, `key` | 16 | nothing |
| `SpanBuilder[T, in r]`, `Builder[T, in r]` | own, affine | none | 16 | region reference, opaque owner |
| `Unique` | plain | `shareable`, `send`, `key` | 8 | nothing |
| `Task[R]` | resource, mechanical | none | 8 | resource, and R's |
| `Sender[T]`, `Receiver[T]` | resource, mechanical | `send` | 24 | resource |
| `Latest[T]` | resource, mechanical | `send` | 8 | resource |
| `Flow` | resource, mechanical | none | 8 | resource |
| `Sink[T]`, `Into[T]`, `Port[T]` | plain | `shareable`, `send` | 8 | nothing |
| `HostFile` | resource, mechanical | `send` | 8 | resource |
| `Socket`, `ReadHalf`, `WriteHalf`, `Listener` | resource, mechanical | `send` | 8 | resource |
| `Dir`, `Net`, `Clock`, `Random`, `Stdout`, `Stderr`, `Stdin`, `Env`, `Process`, `Tasks` | plain | `shareable`, `send` | 8 | capability |
| `Secret` | plain | `shareable`, `send` | 8 | capability |
| `Args` | plain | `shareable`, `send` | 8 | nothing |
| `Site` | plain | `shareable`, `send`, `key` | 8 | nothing |
| `Causes` | own | `send` | 8 | opaque owner |
| `Log[E]` | own | none | 8 | opaque owner, and E's |
| `StaticFn[F]` | plain | `shareable`, `send`, `key` | 8 | nothing |

Flow endpoints are usable with the flow that created them. `Wire` has cases
`Foreign`, `Started`, `Zero` and `Refused(Capacity)`. `Push[T]` has cases `Ok`,
`Ended(T)`, `Order(T)`, `Closed(T)` and `Stalled(T)`. `Pull[T]` has cases
`Item(key, from, x)`, `Dropped(from, at)`, `End` and `Stalled`. `Halt` has cases
`OutOfTime` and `Failed`. These types are plain enums. `Failure` is a plain struct
with fields `stage`, `key` and `halt`.

`Args` designates the entry arguments, which are host memory outside every limit, immutable
and live for the run; `args.get` returns a view of one. `Causes` owns its chain's cause
cells, which were charged when each cause was raised
([FAIL-10](failure.md#fail-10-cause-chain)); dropping it refunds them. It is iterated by `for`
([SEM-6](semantics.md#sem-6-iteration)), primary cause first, binding a view of each cause as
a `Cause`, whose `kind` is a `CauseKind`. The core declarations
that have bodies (`Option`, `Result`, `Capacity`, `OutOfTime`, the error types) have the summaries OWN-2
computes, with `Option` and `Result` taking bounds as tuples do. `Task[R]` is never `send`
([CONC-2](tasks.md#conc-2-the-send-bound)); its parameter's `R: send` constrains the
result, not the handle. `Log[E]` is a handle to a separately admitted node that owns its
entries ([MEM-29](memory.md#mem-29-append-only-logs)), and `StaticFn[F]` designates static
identity bytes, a revision and a function ([PROG-9](programs.md#prog-9-code-descriptors)).

## Contracts the signatures do not state

Each item gives the domain, result, length and capacity, custody and partial effects of the
operations it names; nothing else about them may be inferred from an implementation.

- **Errors.** An operation that fails with `Capacity` returns in its error every argument it
  would have taken and leaves its other arguments unchanged. Its error is `(Capacity, X)`,
  `X` the one such argument or the tuple of them in parameter order, as an owning closure's
  or a resource literal's error holds its captures or fields, and bare `Capacity` when it
  takes none. Only the failures named here or
  by a rule occur: `Bounds`, `Overflow`, `Division` and `Unwrap` are failures
  ([FAIL-2](failure.md#fail-2-causes)), every other error is a `Result`. `fail(take msg)`
  takes custody of a message the program already holds and admits nothing, so raising `Fail`
  cannot be refused ([FAIL-10](failure.md#fail-10-cause-chain)); a built message's
  `Capacity` is an ordinary error at its own allocation, before `fail`.
- **Unwrap.** `option.unwrap(take x)` returns the `Some` payload and fails with `Unwrap` on
  `None`. `result.unwrap(take x)` returns the `Ok` payload; on `Error(e)` it fails with
  `Unwrap`, and `e` is cleaned by the failure's cleanup as an ordinary owner.
- **Allocation.** `list.new`, `string.new`, `map.new`, `list.with_capacity(0)` and an empty
  `span.builder` allocate nothing; `list.with_capacity(n)` admits exactly `n` elements and
  `span.with_capacity(r, n)` exactly `8 + n × size(T)`, rounded up to 8, its count header
  even when `n` is 0; `text.copy_in` admits its count header even for empty text.
  `build_stream_in` admits `n` bytes once, and `finish_trim` refunds all of them past the
  used bytes, rounded up to 8; `finish` admits nothing and refunds nothing.
  `list.reserve(mut xs, n)` leaves capacity at least `len + n`; every other allocating
  operation grows by
  [MEM-24](budgets.md#mem-24-logical-charges). `string.from` copies a literal or a view.
- **Literals.** A string literal has type `String`, is static, allocates nothing and is never
  charged; it has capacity 0, so the first push onto it grows it into new storage. This is
  sound because no operation writes a static `String`'s bytes: growth moves them into new
  storage, `truncate` and `clear` only shorten it, and the ASCII case changes copy it first. A `List` has in-place element writes
  (`list.set`, `xs[i] = x`), so an own `List` that has elements always owns its storage:
  capacity 0 means length 0, and no `List` is static. A byte-string literal (`b".."`,
  `br#".."#`) therefore has type `Result[Bytes, Capacity]`: it admits its length as
  `List[u8]` storage and copies its bytes. When the expected type is
  `Result[FrozenBytes, Capacity]`, the literal uses read-only static storage.
  It admits the same charge, in the same limit, and refunds it when the owner
  drops. `bytebuf.freeze(take b)` transfers the existing charge, length and
  capacity of an owned `Bytes`. `bytebuf.as_bytes(b)` borrows the bytes with
  that length and capacity. `""` is the empty `String` of capacity 0.
- **Bounded append.** `bytebuf.append_inline(mut out, src, start, end)` takes
  an `Inline[u8, n]` source. It copies the range `start .. end` when it is in
  bounds and fits in the destination's spare capacity. It returns false
  without writing if either check fails. It never grows the destination.
- **Strings.** `string.into_bytes(take s)` transfers `s`'s owned storage unchanged, with its
  length, capacity and charge; a static `s` is copied into newly admitted storage of capacity
  equal to its length, and on `Capacity` the error returns `s`. `string.from_utf8` returns its
  argument when it is not UTF-8. `string.push_u64` appends `n` in ASCII decimal, with no sign
  and no leading zeros (`0` for zero). `string.make_ascii_upper(mut s)` and
  `string.make_ascii_lower(mut s)` change `a`–`z` to `A`–`Z`, or the reverse, in place, and
  leave every other byte as it is, so `s` keeps its length and stays UTF-8 with no second
  check. For an owned `s` they admit nothing and cannot fail. A static `s` is first copied
  as `into_bytes` copies it, and on that copy's `Capacity` they return the error with `s`
  unchanged. `string.encode_base64(b)` constructs a new `String` with the standard
  RFC 4648 alphabet and `=` padding. `string.append_base64_bytes(b, mut out)` appends
  the same encoding to a byte list and leaves `out` unchanged on `Capacity`.
  `string.next_scalar(s, at)` returns `None` when `at`
  is `byte_len(s)`, and fails with `Bounds` when `at` is greater or falls inside a scalar.
- **Slices.** `list.slice(xs, lo, hi)` and `string.slice(s, lo, hi)` view the elements or
  bytes from `lo` up to `hi`; they fail with `Bounds` when `lo > hi` or `hi` exceeds the
  length, and `string.slice` also when `lo` or `hi` falls inside a scalar. A slice's
  capacity is its length; `string.as_bytes` reports its `String`'s capacity, 0 for static
  storage. No view grants spare capacity.
- **Bulk overwrite.** `list.copy_from_slice(mut dst, at, src)` copies all elements of
  the source List or List view over existing elements starting at `at`. The Inline
  counterpart `inline.copy_from_slice(mut dst, at, src, count)` takes the first
  `count` elements of an Inline array of the same type. Elements must be plain.
  A range outside either carrier raises `Bounds` before any writes. The destination
  keeps its length and capacity. Each copy admits no bytes and takes one Step
  ([MEM-30](budgets.md#mem-30-step-bounds)).
- **Words.** `list.word_le(b, at)` and `list.word_be(b, at)` read a little- or big-endian
  word from `b` at a byte offset. The expected `u16`, `u32` or `u64` type fixes the width.
  Without an expected type the result is `u64`. An invalid range, including an overflowing
  end, fails with `Bounds`. `list.put_word_le(mut b, x)` and `list.put_word_be(mut b, x)`
  append the two, four or eight bytes of `x` in the specified order. Growth follows
  [MEM-24](budgets.md#mem-24-logical-charges). On `Capacity` the list is unchanged.
  `inline.word_le` and `inline.word_be` read the same widths from an Inline byte
  array. Its expected integer type fixes the width and an invalid range fails with `Bounds`.
  The word operations and [SEM-11](semantics.md#sem-11-vectors) lane operations are the
  multi-byte loads and stores. Library scans use them and the [SEM-1](semantics.md#sem-1-integers)
  integer operations.
- **XXH3 kernels.** `check.xxh3_short(b)` and `check.xxh3_short_inline(b, n)`
  return seed-zero XXH3-64 with the default secret over the byte span. Lengths
  above 240 raise `Bounds`. `check.xxh3_stripes(mut acc, b, key, offset)`
  updates the eight accumulator words from complete 64-byte stripes. Each
  stripe reads 64 bytes of `key`, starting at `offset` and advancing eight bytes
  per stripe. An incomplete stripe or a secret window outside the 192-byte key
  raises `Bounds` before any write. `check.xxh3_stripe` reads one inline stripe.
  These calls each take one step and admit no bytes. Vector and portable
  kernels compute the same words ([SEM-8](semantics.md#sem-8-determinism)).
- **SHA-1 blocks.** `sha1.blocks(mut h, b)` applies RFC 3174 compression to the
  64-byte blocks of `b` and its RFC 3174 padding, updating the five state words
  `h`. This call takes one step and admits no bytes. The engine can use the CPU's
  SHA-1 instructions with identical state words and accounting on every CPU
  ([SEM-8](semantics.md#sem-8-determinism), [MEM-30](budgets.md#mem-30-step-bounds)).
  `std.sha1.hash` supplies the RFC initial state and returns the twenty digest bytes.
- **SHA-256 blocks.** `sha256.blocks(mut h, b, at, count)` applies the FIPS 180-4
  SHA-256 compression function to each of the `count` 64-byte blocks of `b` from `at`,
  in order, updating the eight state words `h`. A range outside `b`, including an end
  past `u64`, raises `Bounds` before `h` changes. `sha256.block(mut h, b)` compresses
  the one block of an `Inline[u8, 64]`. Each call admits no bytes and takes one Step
  whatever `count` is ([MEM-30](budgets.md#mem-30-step-bounds)), as a bulk copy does. An
  engine may use the CPU's SHA-256 instructions; the state words are the same on every
  CPU, so by [SEM-8](semantics.md#sem-8-determinism) a run behaves the same on every CPU.
  `std.sha256` extends this module with the streaming hash.
- **Lists.** `list.set` returns the old element and `list.get` returns `None` out of range,
  where `xs[i]` and `list.set` fail with `Bounds`. An element is mutated in place through
  its index place (`xs[i] = x`, or `mut xs[i]` as an argument), including nested places
  such as `rows[i][j] = x` and `list.push(mut rows[i], x)`; `list.get` gives a view.
  Maps have no index place; use `map.update(mut m, k, f)` to mutate a value in place.
  `list.fill(mut xs, n, x)` appends `n`
  copies of `x`, growing once to at least `len + n`. `list.sum` adds the elements from index
  0, returns 0 for an empty list, and fails with `Overflow` at the first sum that does not
  fit. A `List` is cleaned element by element from index 0. `list.swap(mut xs, i, j)`
  exchanges elements `i` and `j`, does nothing when `i == j`, admits nothing, and fails with
  `Bounds` when either index is not below the length, as `list.set` does.
  `list.sort_stable(mut xs, before)` permutes `xs` so that no element is followed by one
  that `before` orders `Less` than it, keeping the original order of elements it orders
  `Equal`; when `before` is not a total preorder the result is still a permutation of `xs`.
  For two or more elements it admits one scratch block of `len` elements as a `List[T]`
  ([MEM-24](budgets.md#mem-24-logical-charges)) before moving any, and refunds it before it
  returns; on `Capacity` `xs` is unchanged. A failure inside `before` leaves `xs` holding
  every element it had, in an unspecified order, refunds the scratch and propagates. The
  number and order of calls to `before` is unspecified.
- **Deques and heaps.** `deque.push_back` and `deque.push_front` add at either end of a
  `Deque[T]`, and `heap.push` adds to a `Heap[T]`; on `Capacity` each returns `x` and leaves
  the collection unchanged. `deque.pop_front`, `deque.pop_back` and `heap.pop` remove and
  return an element, `None` when empty, and admit nothing; `heap.pop` returns a least element
  by `<` ([SEM-7](semantics.md#sem-7-equality-and-order)), and `heap.peek` views the
  element the next `pop` would return. `new` admits nothing and `len` counts elements. Both
  grow by MEM-24 and never shrink. A `Deque` is cleaned from front to back, a `Heap` in an
  unspecified order. A heap compares with `<` on an `ordered` type, which never fails, so
  only `sort_stable` calls a caller's comparator. Both are core Hirsel code over private
  bodiless functions, as `Map` is; no library module provides a deque or a heap.
- **Maps.** `map.insert` inserts an absent key or replaces a present key's value.
  It returns its key and value with `Capacity`, as
  `(Capacity, (K, V))`, and a present key admits nothing. `map.update(mut m, k, f)`, where
  `f: scoped fn(mut V) -> R`, runs `f` once on the value of `k` in place and returns
  `Some(r)`, or returns `None` without calling `f` when `k` is absent; it makes one lookup,
  never inserts and admits nothing. `map.update_or_insert(mut m, take k, take initial, f)`
  makes one lookup. It inserts `initial` only if `k` is absent, then calls `f` once on
  the entry and returns `Ok(r)`. On `Capacity` it returns `(k, initial)` untouched,
  leaves `m` unchanged and does not call `f`. Both operations lend `m` for the call,
  so `f` cannot name `m` ([OWN-9](ownership.md#own-9-overlap)). `map.remove` drops the stored key
  and returns the value. `map.reserve(mut m, n)` makes room for `n` absent keys: after it
  returns `Ok`, the next `n` insertions admit nothing, whatever removals, replacements and updates
  come between. It grows by [MEM-24](budgets.md#mem-24-logical-charges) from a need of
  `len + n`, admits nothing when the capacity already holds `len + n` (it drops removed
  entries in place), and on `Capacity` leaves `m` unchanged. A `Map` is cleaned entry by entry in insertion order, each key
  before its value, and `for take (k, v) in m` consumes one entry by entry
  ([SEM-6](semantics.md#sem-6-iteration)).
- **Integers.** The `integer` and `float` bounds ([OWN-4](ownership.md#own-4-bounds)) say
  which numeric types a core parameter ranges over. `int.widen` is accepted only where
  every `T` fits in `U`, and `int.truncate` only where `U` is no wider than `T`
  ([SEM-1](semantics.md#sem-1-integers)); `int.narrow` fails with `Overflow`; `int.truncate` keeps
  the low bits of the destination's width. `wrapping_neg` and `wrapping_shl` work modulo 2^width, `wrapping_shl` taking
  its count modulo the width. `int.abs(a)` is `a` on an unsigned type; on a signed type it is `-a` for
  a negative `a` and `a` otherwise, so `int.abs(MIN)` fails with `Overflow` at the call, as `-MIN` does. `checked_div` returns `None` for a zero divisor and for `MIN /
  -1`. `int.mul_wide(a, b)` returns the high and low halves of the exact 128-bit product.
  `int.parse` reads ASCII decimal digits, with one leading `-` for a signed `T` and no other
  sign, space or underscore. It checks the whole grammar before the range: it returns
  `Empty` for an empty string, `Invalid(0)` for a lone `-`, `Invalid(i)` at the byte
  position `i` of the first byte that is not allowed, and `OutOfRange` only for a valid
  numeral whose value does not fit.
- **Floats.** `float.from_int` and `float.narrow` round to nearest, ties to even; `float.to_int` rounds
  toward zero and returns `NaN` or `OutOfRange`. `float.from_bits32` and `from_bits64` return
  the canonical NaN of [SEM-2](semantics.md#sem-2-floats) for every NaN pattern and the
  exact value for every other pattern, so a round trip through the bits is exact except that
  every NaN becomes canonical. These, with `char.from_u32` and `char.to_u32`, are all the
  numeric conversions. Core's other float operations are `+ - * /`, unary `-`,
  `float.mul_add`, `float.sqrt`, `float.abs`, `float.floor`, `float.ceil`, `float.trunc`,
  `float.round` (ties away from zero) and `float.round_even` (ties to even). Each is the IEEE
  754 operation, correctly rounded, so its result is exact or the nearest representable
  value. A NaN operand gives the canonical NaN; `sqrt` of a value below zero gives the
  canonical NaN, `sqrt(-0.0)` is `-0.0` and `sqrt` of `+∞` is `+∞`; `abs` clears the sign;
  the rounding operations return zeros and infinities unchanged and keep the sign of a zero
  result, so `float.floor(-0.5)` is `-1.0` and `float.ceil(-0.5)` is `-0.0`.
  The predicates `float.eq`, `float.lt`, `float.le`, `float.gt` and `float.ge` each take
  two operands of the same type `F: float` and return `bool`. They implement IEEE 754
  `compareQuietEqual`, `compareQuietLess`, `compareQuietLessEqual`, `compareQuietGreater`
  and `compareQuietGreaterEqual`, respectively. All five return `false` if either operand
  is NaN. Negative and positive zero compare equal, and negative and positive infinity
  are below and above every finite value, respectively.
  No other float operation is in core: `sin`, `cos`, `exp`, `ln`, `pow`, `min`, `max` and the rest are not
  correctly rounded on every target, or not one IEEE operation, and are library code over
  these.
- **Host.** A path is resolved relative to its `Dir` and may not leave it, through every
  traversal, symbolic links included. A **valid path** is one or more components joined by
  `/`, where no component is empty, `.` or `..`, and no byte is NUL; so it is never absolute
  and never names the `Dir` itself. Every Host operation that takes a path returns `Denied`
  for one that is not valid, or that leaves its `Dir`, and then has no host effect.
  A provider gives a `Dir` this confinement or does not exist; none opens a path any weaker
  way, and there is no fallback that resolves paths itself. On Linux the host's provider
  needs `openat2` (Linux 5.6 or later) permitted by every sandbox in force, which a version
  check alone cannot show, so a run granted a host `Dir` probes `openat2` once before
  `main`. Where it is unavailable or a sandbox denies it, the run does not start
  ([CAP-4](capabilities.md#cap-4-entry-grants)): it runs no program code, has no host
  effect, and its one line names the cause, `Dir requires openat2 (Linux 5.6 or later,
  permitted by the sandbox)`. A run granted no `Dir` does not probe.
  `OpenMode`:
  `Read` opens an existing file for reading (`NotFound` when absent); `Write` opens for
  writing, creating the file when absent and emptying it otherwise; `Append` opens for
  writing at the end, creating it when absent; `CreateNew` creates a file for writing and
  returns `Exists` when one is present. `host.read` appends to `buf` at most its spare
  capacity, never changes its capacity, advances the file's cursor by the count and returns it, `Ok(0)` at end of file
  or when `buf` has no spare capacity; a read that returns an error has appended nothing
  and moved no cursor, so a partial read is delivered as `Ok`. A successful `host.write`
  committed all of `b`; a `WriteError`'s `written` bytes stay written. Streams take bytes
  the same way: `stdout.write_bytes(out, b)` and `stderr.write_bytes` commit a prefix of
  `b`, all of it on `Ok`, and `written` counts the bytes of `b` committed, at any byte
  boundary; `stdout.write(out, s)` and `stderr.write` are those operations on
  `string.as_bytes(s)`. So the rest of a partial write is `list.slice(b, written, len)`,
  a view of bytes, and no caller has to form a `String` from a part of a scalar. `host.try_lock` tries
  an exclusive advisory lock without waiting: `Ok(true)` when the handle holds it (also when
  it already did), `Ok(false)` when another holder has it, and on an error it acquires
  nothing. `host.unlock` of an unlocked handle returns `Ok(())`; a failed unlock leaves the
  lock held. `host.close` consumes its file whatever it returns and always ends its lock;
  an error reports the host's close outcome. `host.mkdir` creates one directory, `Exists`
  when present; `host.remove` removes one file or empty directory. `host.mkdtemp(d, mut
  name)` checks before issue that `name` has at least 32 bytes of spare capacity, failing
  with `Bounds` otherwise and changing nothing. The bytes already in `name`, possibly none,
  are a prefix: `mkdtemp` creates a new empty directory in `d` named the prefix followed by
  a fresh suffix of 1 to 32 ASCII letters and digits, appends only the suffix, so `name` is
  then exactly the new directory's name, and returns a `Dir` rooted at it. A prefix with
  which that name is not a valid path of one component (it holds a `/` or a NUL) gives
  `Denied`. On an error it creates nothing and appends nothing. `host.remove_tree(d, name)` removes such a directory without
  following symbolic links; on an error it may have removed part of the tree, and nothing
  removed is restored. `host.rename(d, from, to)` gives the regular file `from` the name
  `to`, both in the one `Dir` `d`, replacing a regular file at `to`. The final component of
  either path is not followed as a symbolic link. It returns `NotFound` when `from` is
  absent, and `Denied` when `from` is not a regular file or `to` names anything other than
  a regular file; on an error it renames nothing. When both name the same file it succeeds
  and changes nothing. Its atomicity is visibility only: an `open` of `to` finds the old
  file or the new one, never neither, and `rename` makes nothing durable, so after a host
  crash either name may hold the file unless a later `host.sync_dir(d)` returned `Ok(())`.
  `host.sync(mut f)` and `host.sync_dir(d)` are the two sync barriers. `host.sync(f)` returns `Ok(())` once every byte of `f`'s file written
  before its issue, through any handle, survives a host crash, with the file's length and
  whatever else reading those bytes back needs; it makes no name durable. `host.sync_dir(d)`
  returns `Ok(())` once every name in the directory `d` is rooted at that was created,
  renamed or removed before its issue survives a host crash in that state; names in its
  subdirectories are not covered. A failed sync is an **unknown outcome**: which of those
  bytes or names survive a crash is not known, and a later success of the same barrier
  says nothing about them. Each is a cancellation point at its issue only, and the host
  provider completes it at its issue ([CONC-7](tasks.md#conc-7-cancellation-points)), so
  cancellation never interrupts one. `host.wall_ms` is milliseconds since 1970 and
  `host.mono_ns` never decreases in a run. `host.sleep_until(c, deadline_ns)` returns once
  `host.mono_ns(c) >= deadline_ns`, at once for a deadline already reached; it is a
  cancellation point at its entry and while it waits
  ([CONC-7](tasks.md#conc-7-cancellation-points)), and its completion passes the replay
  point ([CAP-8](capabilities.md#cap-8-the-replay-point)). A relative sleep is
  library code over it. No host operation admits; every byte it returns
  lands in capacity the caller already holds. `HostFile` is a mechanical resource
  ([MEM-24](budgets.md#mem-24-logical-charges)): its finalizer closes it and ignores the
  outcome. `host.random(r: Random, mut b: Bytes)` appends to `b` exactly its spare capacity of bytes from the host's cryptographic source, never
  changes its capacity, and never waits or fails. The bytes are unpredictable and
  independent of every other value the run observes; like every provider result they pass
  the replay point at issue and at delivery ([CAP-8](capabilities.md#cap-8-the-replay-point)),
  and a simulation supplies its own.
  `host.stopped(p: Process) waits` observes the first SIGTERM or SIGINT
  ([CAP-5](capabilities.md#cap-5-provider-operations)).
  `stdin.read(i, mut buf, until)` appends at most the spare capacity of `buf` from the
  run's standard input and returns the count, `Ok(0)` at end of input or when `buf` has no
  spare capacity. Each byte the host supplies is delivered by exactly one read, whichever
  copy of `Stdin` issued it. Like the socket operations of [NET-1](net.md#net-1-socket-operation-declarations), it
  waits after issue for readiness: it is a cancellation point at issue and while it waits, and gives `Aborted` when cancellation
  reaches it there and `TimedOut` when `until` passes, in both cases having appended
  nothing. Bytes the host supplies afterwards go to the next read. The provider never
  changes the flags of the inherited descriptor
  ([stdin](../../core/std/api/stdin.md)). An `Env` designates name patterns over a
  snapshot of the environment that the launcher takes before `main`. The snapshot holds
  the covered variables whose names are UTF-8, lives outside every limit and never
  changes. `env.get` gives `Denied` for a name no pattern covers, `None` for a covered
  name that is not set, and `NotUtf8` for a value that is not UTF-8. `env.len` and
  `env.name` list the covered names bytewise, and `env.narrow` intersects patterns
  ([CAP-7](capabilities.md#cap-7-attenuation), [env](../../core/std/api/env.md)). They
  are not provider operations: the snapshot enters with the entry's grants
  ([CAP-8](capabilities.md#cap-8-the-replay-point)).
- **Other results.** `box.unbox` returns the payload and refunds the node. `ParseError`'s
  `Invalid` holds a byte position. A
  `Box`, `Rc` or `Arc` is read through its payload where its own type does not fit
  ([OWN-8](ownership.md#own-8-views)).
  `task.check_cancel()` fails with `Cancelled` when a request is pending, and
  `channel.new(0)` fails with `Bounds`.
- **Built-in forms.** `dest.put(mut d, L)` and `emit(take w, L)` accept only a construction
  literal `L` ([MEM-4](memory.md#mem-4-sized-destinations)). `clone` and `clone_in` are
  typing forms whose result type selects what is copied ([MEM-17](memory.md#mem-17-clone)).
  `emit`'s operand for each writer type is MEM-4's. `writer.scalar` and `writer.push` take a
  writer of `Text`; `writer.close` one of `Text`, `SeqSpan[T]` or
  `IndexedSpan[T]`; the other `writer` operations one of `SeqSpan[T]` or `IndexedSpan[T]`,
  with `T` their element; any other writer is refused at the argument, MEM-4.
- **Sequences.** `seq.nth` walks `i` elements and returns `None` past the count;
  `indexed.get` and `seq.get` return `None` past it after one test.

*Not normative.* A key index over a JSON object is library code, not core. Its contract:
for a `u32` document, an opaque owner holds the object's immutable `IndexedSpan[Member]`
and a `List[u32]` of the `m` member ordinals, reserved exactly, so it admits
`round8(4 × m)`; its own logical size is 40. The ordinals are sorted by key bytes, then by
ordinal. A lookup finds the lower and upper bounds of the equal keys by binary search, so
it makes `O(log m)` key comparisons however many keys are equal, and takes the greatest
ordinal; rendering visits each key at its first ordinal with that last value. A wider
document needs `u64` ordinals or a width error. No operation replaces the source alone or
takes unchecked offsets, and nothing in the document moves.
