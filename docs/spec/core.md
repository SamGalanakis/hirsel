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
| `core` (the prelude: every core type, `fail`, `drop`, `isolate`, `replace`, `clone`, `clone_in`, `build_in`, `build_stream_in`, `emit`, `finish`, `finish_trim`) | [core.hirsel](../../core/core/core.hirsel) | LANG-7, MEM-4, MEM-17, OWN-13, FAIL-2, FAIL-16 |
| `option`, `result` | [option](../../core/core/option.hirsel), [result](../../core/core/result.hirsel) | FAIL-2 |
| `int`, `float`, `char` | [int](../../core/core/int.hirsel), [float](../../core/core/float.hirsel), [char](../../core/core/char.hirsel) | SEM-1 to SEM-3 |
| `simd` | [simd](../../core/core/simd.hirsel) | SEM-11 |
| `check` | [check](../../core/core/check.hirsel) | SEM-8, SEM-11, MEM-30 |
| `sha1` | [sha1](../../core/core/sha1.hirsel) | SEM-8, MEM-30 |
| `sha256` | [sha256](../../core/core/sha256.hirsel) | SEM-8, MEM-30 |
| `ed25519`, `p256` | [ed25519](../../core/core/ed25519.hirsel), [p256](../../core/core/p256.hirsel) | SEM-8, MEM-30; [Signature verification](#signature-verification) |
| `box`, `rc`, `arc`, `unique` | [box](../../core/core/box.hirsel), [rc](../../core/core/rc.hirsel), [arc](../../core/core/arc.hirsel), [unique](../../core/core/unique.hirsel) | MEM-13 to MEM-18 |
| `list`, `string`, `text`, `map`, `span`, `dest` | [list](../../core/core/list.hirsel), [string](../../core/core/string.hirsel), [text](../../core/core/text.hirsel), [map](../../core/core/map.hirsel), [span](../../core/core/span.hirsel), [dest](../../core/core/dest.hirsel) | SEM-3 to SEM-6, MEM-3, MEM-4, MEM-12 |
| `bytebuf` | [bytebuf](../../core/core/bytebuf.hirsel) | MEM-21, MEM-28 |
| `deque` | [deque](../../core/core/deque.hirsel) | Deques and heaps, MEM-24 |
| `heap` | [heap](../../core/core/heap.hirsel) | Deques and heaps, MEM-24 |
| `seq`, `indexed`, `writer` | [seq](../../core/core/seq.hirsel), [indexed](../../core/core/indexed.hirsel), [writer](../../core/core/writer.hirsel) | MEM-4, MEM-12 |
| `task`, `channel` | [task](../../core/core/task.hirsel), [channel](../../core/core/channel.hirsel) | CONC-1 to CONC-11, CONC-15 |
| `conn` | [conn](../../core/core/conn.hirsel) | CONC-17 |
| `latest` | [latest](../../core/core/latest.hirsel) | CONC-13 |
| `log` | [log](../../core/core/log.hirsel) | MEM-29 |
| `code` | [code](../../core/core/code.hirsel) | PROG-9 |
| `flow` | [flow](../../core/core/flow.hirsel) | CONC-14 |
| `provide`, `clock`, `process` | [provide](../../core/core/provide.hirsel) | CAP-2, CAP-5, CONC-7, FAIL-13, FAIL-15, MEM-24, TLS-5; [Provide](#provide) |
| `foreign` | the raw handle type `Ptr` | FFI-4 to FFI-7 |
| `exec` | unbuilt: `Exec`, `Proc` and their operations | EXEC-2 to EXEC-8 |
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
`cross`, which [CONC-2](tasks.md#conc-2-the-send-bound) gives, nor
`integer` and `float`, which hold exactly for the integer and the float types; a holds
entry "T's" is the union of its type arguments' holds sets. Tuples, `Option` and `Result`
take their kind and copyability from OWN-3 and their other bounds componentwise; `Inline`
takes its kind from `T`. Sizes are logical ([MEM-24](budgets.md#mem-24-logical-charges)). A
scoped value is never stored in a field of an own or resource type, a collection or
an environment (OWN-19), so it has no heap size; a scoped type's own fields
([OWN-22](ownership.md#own-22-scoped-types)) and a callable's logical width
([LANG-13](language.md#lang-13-callables)) are unaffected.

The [standard codec](../../core/std/api/codec.md) raises Capacity on refused
admission. Its value and schema writers report `EncodeError` for a count beyond
the format's u32 field. Fixed-width scalar and key writers return unit.
`decode` and `read` return `Result[T, DecodeError]`.

| type | kind | bounds | size | holds |
|---|---|---|---|---|
| integers, floats | plain | `shareable`, `send`, `codec`; integers `key`, `ordered` | width | nothing |
| `bool`, `char`, `()` | plain | `shareable`, `send`, `key`, `ordered`, `codec` | 1, 4, 0 | nothing |
| tuples, `Option[T]`, `Result[T, E]` | OWN-3 | `shareable`, `send`, `key`, `ordered`, `codec` when T | tuple: sum; variant rule (MEM-24) | T's |
| `Inline[T, n]` | T's kind (plain or own) | `shareable`, `send`, `key`, `ordered` when T | n × size(T) | T's |
| `Simd[T, n]` | plain | `shareable`, `send` | n × size(T) | nothing |
| `Never` | plain | `shareable`, `send`, `key`, `ordered`, `codec` (SEM-10: no valid encoding) | 0 | nothing |
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
| `Client[T]`, `Inbox[T]` | resource, mechanical | `send` | 8 | resource |
| `Flow` | resource, mechanical | none | 8 | resource |
| `Sink[T]`, `Into[T]`, `Port[T]` | plain | `shareable`, `send` | 8 | nothing |
| `HostFile` | resource, mechanical | `send` | 8 | resource |
| `Socket`, `ReadHalf`, `WriteHalf`, `Listener` | resource, mechanical | `send` | 8 | resource |
| `UnixListener`, `UnixDatagram` (unbuilt, [NET-13](net.md#net-13-unix-streams), [NET-14](net.md#net-14-unix-datagrams)), `Proc` (unbuilt, [EXEC-3](exec.md#exec-3-spawn)) | resource, mechanical | `send` | 8 | resource |
| `Dir`, `Net`, `Clock`, `Random`, `Stdout`, `Stderr`, `Stdin`, `Env`, `Process`, `Tasks` | plain | `shareable`, `send` | 8 | capability |
| `Secret` | plain | `shareable`, `send` | 8 | capability |
| `Foreign[I]` ([FFI-4](foreign.md#ffi-4-the-foreign-capability)) | plain | `shareable`, `send` | 8 | capability |
| `Exec` (unbuilt, [EXEC-2](exec.md#exec-2-launch-profiles-and-narrowing)) | plain | `shareable`, `send` | 8 | capability |
| `foreign.Ptr[K]` ([FFI-6](foreign.md#ffi-6-handles)) | plain | `send` | 8 | a handle-table ordinal of kind `K`; holds a raw handle (OWN-2) |
| `Args` | plain | `shareable`, `send` | 8 | nothing |
| `Site` | plain | `shareable`, `send`, `key` | 8 | nothing |
| `Causes` | own | `send` | 8 | opaque owner |
| `Log[E]` | own | none | 8 | opaque owner, and E's |
| `StaticFn[F]` | plain | `shareable`, `send`, `key` | 8 | nothing |

Flow endpoints are usable with the flow that created them. `Wire` has cases
`Foreign`, `Started` and `Zero`. `Push[T]` has cases `Ok`,
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

- **Errors.** An allocating operation whose admission is refused raises the failure
  `Capacity(c)` ([MEM-21](budgets.md#mem-21-admission)) at its commit point: it has taken
  none of its arguments, which the failure's cleanup drops with the rest of the statement,
  and it leaves its other arguments unchanged. An operation named `try_` returns the
  refusal instead: its error is `(Capacity, X)`, `X` the one argument it would have taken
  or the tuple of them in parameter order, as a resource literal's error holds its fields,
  and bare `Capacity` when it takes none; it changes nothing else. The `try_` operations
  that admit are `list.try_reserve`, `map.try_reserve` and `task.try_spawn`. Only the
  failures named here or by a rule occur: `Bounds`, `Overflow`, `Division`, `Unwrap` and
  `Capacity` are failures ([FAIL-2](failure.md#fail-2-causes)), every other error is a
  `Result`. `fail(take msg)` takes custody of a message the program already holds and
  admits nothing, so raising `Fail` cannot be refused
  ([FAIL-10](failure.md#fail-10-cause-chain)); a built message's allocation is refused, if
  at all, at its own admission, before `fail`.
- **Drop and copied.** `drop[T](take x: T)` ends `x` where it is called, by the implicit drop
  the end of its scope would perform ([T6.2](ownership.md#own-26-the-transition-table)), a
  resource's finalizer included ([FAIL-11](failure.md#fail-11-resources)), and returns `()`;
  a failure of that finalizer is this call's. `option.copied[T: plain](x: Option[View[T]]) -> Option[T]`
  returns `Some` of a copy of the viewed `T`, or `None`, so `option.copied(list.get(xs, i))`
  outlives `xs`; it admits nothing. Both are headers of the prelude and of `option`, written
  once in those files.
- **Unwrap.** `option.unwrap(take x)` returns the `Some` payload and fails with `Unwrap` on
  `None`. `result.unwrap(take x)` returns the `Ok` payload; on `Error(e)` it fails with
  `Unwrap`, and `e` is cleaned by the failure's cleanup as an ordinary owner.
- **Allocation.** `list.new`, `string.new`, `map.new`, `list.with_capacity(0)` and an empty
  `span.builder` allocate nothing; `list.with_capacity(n)` admits exactly `n` elements and
  `span.with_capacity(r, n)` exactly `8 + n × size(T)`, rounded up to 8, its count header
  even when `n` is 0; `text.copy_in` admits its count header even for empty text.
  `build_stream_in` admits `n` bytes once, and `finish_trim` refunds all of them past the
  used bytes, rounded up to 8; `finish` admits nothing and refunds nothing.
  `list.reserve(mut xs, n)` and `list.try_reserve(mut xs, n)` leave capacity at least
  `len + n`, `try_reserve` returning `Error(c)` and leaving `xs` unchanged where `reserve`
  raises; every other allocating operation grows by
  [MEM-24](budgets.md#mem-24-logical-charges). `string.from` copies a literal or a view.
- **Literals.** A string literal has type `String`, is static, allocates nothing and is never
  charged; it has capacity 0, so the first push onto it grows it into new storage. This is
  sound because no operation writes a static `String`'s bytes: growth moves them into new
  storage, `truncate` and `clear` only shorten it, and the ASCII case changes copy it first. A `List` has in-place element writes
  (`list.set`, `xs[i] = x`), so an own `List` that has elements always owns its storage:
  capacity 0 means length 0, and no `List` is static. A byte-string literal (`b".."`,
  `br#".."#`) therefore has type `Bytes`: it admits its length as `List[u8]` storage and
  copies its bytes, and a refusal raises `Capacity`. When the expected type is
  `FrozenBytes`, the literal uses read-only static storage.
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
  equal to its length; a refused copy has not taken `s`. `string.from_utf8` returns its
  argument when it is not UTF-8. `string.push_u64` appends `n` in ASCII decimal, with no sign
  and no leading zeros (`0` for zero). `string.make_ascii_upper(mut s)` and
  `string.make_ascii_lower(mut s)` change `a`–`z` to `A`–`Z`, or the reverse, in place, and
  leave every other byte as it is, so `s` keeps its length and stays UTF-8 with no second
  check. For an owned `s` they admit nothing and cannot fail. A static `s` is first copied
  as `into_bytes` copies it, and a refused copy leaves `s` unchanged. `string.encode_base64(b)` constructs a new `String` with the standard
  RFC 4648 alphabet and `=` padding. `string.append_base64_bytes(b, mut out)` appends
  the same encoding to a byte list and leaves `out` unchanged when its growth is refused.
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
  `list.fill_slice(mut dst, at, count, x)` overwrites the `count` elements of `dst`
  from `at` with copies of the plain `x`. It raises `Bounds` before any write unless
  `at ≤ len` and `count ≤ len − at`, so an `at + count` past `u64` is refused rather
  than wrapped, and it checks the range even when `T` has no storage. The list keeps
  its length and capacity; the call admits no bytes and takes one Step whatever
  `count` is. A fill writes the same bytes on every engine
  ([SEM-8](semantics.md#sem-8-determinism)).
- **Words.** `list.word_le(b, at)` and `list.word_be(b, at)` read a little- or big-endian
  word from `b` at a byte offset. The expected `u16`, `u32` or `u64` type fixes the width.
  Without an expected type the result is `u64`. An invalid range, including an overflowing
  end, fails with `Bounds`. `list.put_word_le(mut b, x)` and `list.put_word_be(mut b, x)`
  append the two, four or eight bytes of `x` in the specified order. Growth follows
  [MEM-24](budgets.md#mem-24-logical-charges). A refused growth leaves the list unchanged.
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
  returns; a refused scratch leaves `xs` unchanged. A failure inside `before` leaves `xs` holding
  every element it had, in an unspecified order, refunds the scratch and propagates. The
  number and order of calls to `before` is unspecified.
- **Deques and heaps.** `deque.push_back` and `deque.push_front` add at either end of a
  `Deque[T]`, and `heap.push` adds to a `Heap[T]`; a refused growth leaves the collection
  unchanged and `x` untaken. `deque.pop_front`, `deque.pop_back` and `heap.pop` remove and
  return an element, `None` when empty, and admit nothing; `heap.pop` returns a least element
  by `<` ([SEM-7](semantics.md#sem-7-equality-and-order)), and `heap.peek` views the
  element the next `pop` would return. `new` admits nothing and `len` counts elements. Both
  grow by MEM-24 and never shrink. A `Deque` is cleaned from front to back, a `Heap` in an
  unspecified order. A heap compares with `<` on an `ordered` type, which never fails, so
  only `sort_stable` calls a caller's comparator. Both are core Hirsel code over private
  bodiless functions, as `Map` is; no library module provides a deque or a heap.
- **Maps.** `map.insert` inserts an absent key or replaces a present key's value; a
  present key admits nothing, and a refused growth takes neither the key nor the value. `map.update(mut m, k, f)`, where
  `f: scoped fn(mut V) -> R`, runs `f` once on the value of `k` in place and returns
  `Some(r)`, or returns `None` without calling `f` when `k` is absent; it makes one lookup,
  never inserts and admits nothing. `map.update_or_insert(mut m, take k, take initial, f)`
  makes one lookup. It inserts `initial` only if `k` is absent, then calls `f` once on
  the entry and returns `r`. A refused growth takes neither `k` nor `initial`, leaves `m`
  unchanged and does not call `f`. Both operations lend `m` for the call,
  so `f` cannot name `m` ([OWN-9](ownership.md#own-9-overlap)). `map.remove` drops the stored key
  and returns the value. `map.reserve(mut m, n)` makes room for `n` absent keys: after it
  returns, the next `n` insertions admit nothing, whatever removals, replacements and updates
  come between. It grows by [MEM-24](budgets.md#mem-24-logical-charges) from a need of
  `len + n`, admits nothing when the capacity already holds `len + n` (it drops removed
  entries in place), and a refusal leaves `m` unchanged. `map.try_reserve(mut m, n)` does
  the same and returns a refusal as `Error(c)`. A `Map` is cleaned entry by entry in insertion order, each key
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
  returns `Exists` when one is present; `ReadWrite` opens for reading and writing, creating
  the file when absent and never emptying it. `host.read` appends to `buf` at most its spare
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
  `host.open_dir(d, p)` returns a `Dir` rooted at the existing directory `p`, with the same
  confinement ([CAP-7](capabilities.md#cap-7-attenuation)). It gives `Denied` when `p` names
  anything other than a directory, without following a final symbolic link.
  `host.entries(d)` opens a listing of `d`'s root as a `HostEntries`, a mechanical resource
  whose finalizer closes it. `host.next_entries(mut h, mut buf)` appends whole records
  (kind byte, `u16` little-endian name length, name), never `.` or `..`, in host order, and
  `Ok(0)` when exhausted. It fails with `Bounds` before issue when `buf` has less than 258
  bytes spare. `host.stat(d, p)` and `host.stat_file(mut f)` report kind, size and
  modification nanoseconds without following a final symbolic link. `host.read_at` and
  `host.write_at` read and write at an offset without moving the cursor, with `host.read`'s
  and `host.write`'s partial results. `read_at` needs a readable handle and `write_at` a
  writable one that is not `Append`, or they give `Denied` with no host effect
  ([dir](../../core/std/api/dir.md), [file](../../core/std/api/file.md)).
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
  ([stdin](../../core/std/api/stdin.md)). `stdin.is_terminal(i)`, `stdout.is_terminal(o)`
  and `stderr.is_terminal(e)` return the stream's **terminal bit**: whether its descriptor
  referred to a terminal when the run started. The launcher takes the bit once per granted
  stream, before `main`, and it enters with the entry's grants
  ([CAP-8](capabilities.md#cap-8-the-replay-point)), so these calls are not provider
  operations, never wait or fail, admit nothing and take one step. The bit never changes
  during the run, whatever happens to the descriptor
  ([stdin](../../core/std/api/stdin.md#terminals)). An `Env` designates name patterns over a
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

## Signature verification

The modules `ed25519` and `p256` check signatures. Each provides functions of type
`(key: Bytes, msg: Bytes, sig: Bytes) -> bool`: `ed25519.verify`, `p256.verify` and
`p256.verify_der`. Each returns `true` exactly when `sig` is a valid signature of all of
`msg` under `key` by the rule below, and `false` otherwise. Every input that is not well
formed gives `false`, including a key or signature of the wrong length, and no input
makes the call fail. The answer does not say which check failed. That is how ring
reports it, and a caller checking untrusted bytes needs no failure path for them.
Signing and key agreement are not provided: they need private-key custody, and the
`Secret` contract ([TLS-5](tls.md#tls-5-secrets)) does not cover keys.

- **Ed25519.** `ed25519.verify` is pure Ed25519 ([RFC 8032 §5.1.7](https://www.rfc-editor.org/rfc/rfc8032#section-5.1.7)),
  without context or prehash. `key` is the 32-byte encoding of the point `A`, and `sig` is
  64 bytes `R || S`. It returns `false` unless all of these hold:
  - `S`, read as a little-endian integer, is below the group order `L`. It is not reduced.
  - `key` decodes to a point. `y` is the low 255 bits taken mod `p`, so an encoding of
    `y ≥ p` is accepted. `x` is the root that the top bit selects, and `false` results when
    there is none. When `x = 0` the top bit is ignored.
  - The encoding of `[S]B − [k]A` equals the 32 bytes of `R`, where
    `k = SHA-512(R || key || msg) mod L` hashes `R` and `key` exactly as given.

  This is the cofactorless equation. `R` is never decoded, so a non-canonical or
  off-curve `R` never matches. `A`'s order is not checked, so small-order and mixed-order
  keys are accepted whenever the equation holds. That is ring's `ED25519`, and RFC 8032
  permits it.
- **P-256.** `p256.verify` and `p256.verify_der` are ECDSA over NIST P-256 with SHA-256
  ([FIPS 186-5 §6.4.2](https://csrc.nist.gov/pubs/fips/186-5/final)). The kernel hashes
  `msg`, and `e` is its SHA-256 digest mod `n`.
  - `key` is an uncompressed SEC 1 point: 65 bytes `0x04 || X || Y` with `X` and `Y` below
    `p`, on the curve. A compressed (`0x02`, `0x03`) or hybrid (`0x06`, `0x07`) point, a
    bare 64-byte `X || Y` and the one-byte point at infinity give `false`. ring parses only
    the uncompressed form, and JWK and COSE keys carry `X` and `Y` whole.
  - `p256.verify` takes `sig` as 64 bytes `r || s`, each 32 bytes big-endian. That is the
    IEEE P1363 form that JWS `ES256` uses ([RFC 7518 §3.4](https://www.rfc-editor.org/rfc/rfc7518#section-3.4)).
  - `p256.verify_der` takes the DER `ECDSA-Sig-Value`, `SEQUENCE { r INTEGER, s INTEGER }`
    ([RFC 3279 §2.2.3](https://www.rfc-editor.org/rfc/rfc3279#section-2.2.3)). That is the
    form X.509, WebAuthn and most package signatures use. Only strict DER is accepted:
    minimal definite lengths, minimal non-negative integers and no trailing bytes. Any BER
    variation gives `false`.
  - Both forms need `1 ≤ r, s < n`. The result is `true` exactly when
    `u1·G + u2·Q` is not the point at infinity and its `x` is `r` mod `n`, where
    `w = s⁻¹ mod n`, `u1 = e·w` and `u2 = r·w`. A high `s` (`s > n/2`) is accepted, as
    FIPS 186-5 accepts it, so `(r, s)` and `(r, n − s)` both verify. A caller that needs one
    signature per message must enforce that itself.

  Both forms are kernels because the conversion between them is where malleability bugs
  live: Wycheproof's BER cases test exactly that conversion. ring parses strict DER itself
  (`ECDSA_P256_SHA256_ASN1`), and its `r || s` path is `ECDSA_P256_SHA256_FIXED`.
- **Charges.** Each call takes one step ([MEM-30](budgets.md#mem-30-step-bounds)) and admits
  no bytes, whatever the lengths of its arguments. The charge is 1 step per call plus
  0 steps per message byte, on every engine, as `sha256.blocks` charges. The work is bounded
  by the length of `msg`, which a limit bounds. MEM-30's one per-unit exception,
  `secret.scram_sha256`, covers work that a peer chooses.
- **Determinism.** The answer is a function of the three byte strings alone. Verification
  is not a host or provider operation, and nothing about it is recorded: replay computes it
  again ([recording](../recording.md#what-a-recording-holds)). An engine may use ring's
  CPU-specific code. Every path gives the same answer, so by
  [SEM-8](semantics.md#sem-8-determinism) a run behaves the same on every CPU. The inputs
  are public, so the time a call takes may depend on them.

### Laws

Each law names the bug it catches and the oracle its expected value comes from. The
vectors are copied from their sources byte for byte. The Wycheproof files are `testvectors_v1/`
at [C2SP/wycheproof](https://github.com/C2SP/wycheproof) commit
`65ca494727a52944c204441c860b039e6786b99b`, cited by `tcId`. Keys come from the test's
group: `publicKey.pk` for Ed25519, `publicKey.uncompressed` for ECDSA. NIST records are
counted from 1 within the `[P-256,SHA-256]` section of `SigVer.rsp` in
[186-4ecdsatestvectors.zip](https://csrc.nist.gov/CSRC/media/Projects/Cryptographic-Algorithm-Validation-Program/documents/dss/186-4ecdsatestvectors.zip),
with key `0x04 || Qx || Qy` and signature `R || S`.

| law | bug | oracle and setup |
|---|---|---|
| **SV1** Ed25519 accepts | the wrong hash input order, a prehash or context variant, or a wrong base point | RFC 8032 §7.1 TEST 1 (empty message), TEST 2 (`72`) and TEST 3 (`af82`), with their public keys and signatures: each gives `true`. |
| **SV2** message bound | `msg` not hashed, or hashed only in part | TEST 3's key and signature over `af83`, TEST 3's message with the low bit of its last byte flipped, give `false`. The pinned file has no message-change case. By hand: changing `msg` changes `k`, so `[S]B − [k]A` no longer encodes `R`. |
| **SV3** S below L | `S` reduced mod `L` instead of checked | `ed25519_test.json` tcId 63 (`S + L`) gives `false`. tcId 3, the same signature with the original `S`, gives `true`. |
| **SV4** small-order key | a key-order check that ring and this rule do not make | `ed25519_test.json` tcId 152 gives `true`. The key is the identity, `R` is the encoding of `B` and `S = 1`, so `[1]B − [k]·identity = B`, whose encoding is `R`. Wycheproof marks this `acceptable`, so the expected value comes from the equation. |
| **SV5** Ed25519 lengths | a wrong length that fails the call, reads past the carrier, or is accepted | Each of these gives `false` and the call does not fail: TEST 1's key cut to 31 bytes, TEST 1's key with `00` appended, TEST 1's signature cut to 63 bytes, and `ed25519_test.json` tcIds 30 (empty signature) and 33 (signature too long). |
| **SV6** P-256 SigVer | a wrong curve, a truncated digest, or a message passed where the digest belongs | `p256.verify` gives `true` for NIST record 4 (`Result = P`) and `false` for record 1 (`F (3 - S changed)`). |
| **SV7** high S | low-S normalisation that rejects a valid signature | `p256.verify` gives `true` for `ecdsa_secp256r1_sha256_p1363_test.json` tcId 64, which is valid with `s > n/2`. `p256.verify_der` gives `true` for `ecdsa_secp256r1_sha256_test.json` tcId 7, the high-S twin of tcId 5. |
| **SV8** strict DER | a BER-lenient parser, or a sign bit read as magnitude | In `ecdsa_secp256r1_sha256_test.json`, `p256.verify_der` gives `true` for tcId 5. It gives `false` for tcId 8 (the sequence length in long form) and for tcId 6 (`s` without its leading zero, so negative). |
| **SV9** P-256 forms and lengths | a compressed or bare key decoded on one engine only, or a wrong length that fails the call | Each of these gives `false` and the call does not fail. With NIST record 4's message and signature: the key compressed (`0x02` or `0x03` by `Qy`'s parity, then `Qx`) and the bare 64-byte `Qx \|\| Qy`. With record 4's key and message: the signature cut to 63 bytes. And `ecdsa_secp256r1_sha256_p1363_test.json` tcId 121, a 2-byte signature. |
| **SV10** charge | a kernel lowered as library code that loops, or a charge per byte or per block | `bound bytes 0 { bound steps 1 { ed25519.verify(k, m, s) } }` gives `Ok(Ok(true))` for TEST 1. The same scope over `p256.verify` gives `Ok(Ok(true))` for NIST record 4, whose message is 128 bytes, and over `p256.verify_der` gives `Ok(Ok(true))` for tcId 5. The arguments are locals bound outside the scopes. The expected value follows by hand from MEM-30: a core call is one step and admits nothing. |

The laws are cases in the core contract harness, in `core/core/tests/ed25519.hirsel` and
`core/core/tests/p256.hirsel`. They take no capability, so the shared walker's `--backend all`
runs each on the evaluator, Cranelift dev and LLVM release ([README](../../core/std/tests/README.md)).

### Build slice

| slice | size | depends on | contents | laws |
|---|---|---|---|---|
| **SV** signature verification | S | — | `core/core/ed25519.hirsel` and `core/core/p256.hirsel` with the three bodiless headers, and their rows in `core_ops.rs` as `Lane::Value` kernels. One function in `hirsel-core-net` wraps ring 0.17.14's `ED25519`, `ECDSA_P256_SHA256_FIXED` and `ECDSA_P256_SHA256_ASN1`. The `RtOp`s `Ed25519Verify`, `P256Verify` and `P256VerifyDer` take key, message and signature pointer and length pairs and return a `u64` 0 or 1. The C entries `hirsel_core_ed25519_verify`, `hirsel_core_p256_verify` and `hirsel_core_p256_verify_der` in `hirsel-core-rt` call that function, and so does the evaluator's `RtOp` arm (its `eval` feature already links `hirsel-core-net`), so compiled code and the evaluator answer from the same code. Cranelift and LLVM lower these as native calls, as `Sha256Blocks` is lowered. There is no `HostOp`, provider path or recording change. The module row's `unbuilt` mark is removed when the slice lands. | SV1–SV10 |

## Provide

`provide` makes capabilities of virtual construction
([CAP-2](capabilities.md#cap-2-capabilities-are-opaque)) and answers their operations in the
program's own code ([CAP-5](capabilities.md#cap-5-provider-operations)). Callers see nothing
new: a `Dir`, `Net`, `Stdout`, `Stderr`, `Clock`, `Random` or `Process` from `provide` has its
ordinary type and every `host`, `net`, `tls`, `stdout` and `stderr` operation takes it.
Unbuilt: `provide.exec` makes an `Exec` answered on a `Served[ExecOp]` port
([EXEC-8](exec.md#exec-8-exec-at-the-replay-point-and-served-exec)), and a `provide.net` port
also answers the Unix operations ([NET-14](net.md#net-14-unix-datagrams)).

```hirsel core
pub opaque resource type Served[Op]
pub opaque resource type Call[Op]
pub opaque type Reply[Op]
pub opaque resource type ClockHand
pub opaque resource type StopTrigger
pub type Withdrawn = Withdrawn()
pub type Part = Whole | Read | Write
pub type DirOp = Open(dir: u64, path: u64, mode: OpenMode) | OpenDir(dir: u64, path: u64) | Mkdtemp(dir: u64, prefix: u64, room: u64) | Stat(dir: u64, path: u64) | Mkdir(dir: u64, path: u64) | Remove(dir: u64, path: u64) | RemoveTree(dir: u64, path: u64) | Rename(dir: u64, from: u64, to: u64) | SyncDir(dir: u64) | Entries(dir: u64) | Read(file: u64, room: u64) | ReadAt(file: u64, at: u64, room: u64) | Write(file: u64, len: u64) | WriteAt(file: u64, at: u64, len: u64) | StatFile(file: u64) | Sync(file: u64) | TryLock(file: u64) | Unlock(file: u64) | Close(file: u64) | NextEntries(entries: u64, room: u64) | Released(id: u64)
pub type DirReply = Done | Failed(IoError) | WriteFailed(WriteError) | Count(u64) | Locked(bool) | Stat(Stat) | File(u64) | Dir(u64) | Entries(u64)
pub type NetOp = Connect(addr: Addr) | Resolve(name: u64, room: u64) | Listen(addr: Addr, backlog: u16) | Accept(listener: u64) | Recv(socket: u64, room: u64, max: u64) | Send(socket: u64, prefix: u64, tail: u64) | Handshake(socket: u64, name: u64, alpn: u64, system: bool) | SendSecret(socket: u64, before: u64, after: u64) | Released(id: u64, part: Part)
pub type NetReply = Done | Failed(NetError) | Count(u64) | Socket(u64, Addr) | Listener(u64, Addr) | Secured(Handshake)
pub type StreamOp = Write(len: u64)
pub type StreamReply = Done | WriteFailed(WriteError)

pub fn dir(bytes n: u64) -> (Dir, Served[DirOp])
pub fn net(bytes n: u64) -> (Net, Served[NetOp])
pub fn stdout(bytes n: u64, terminal: bool) -> (Stdout, Served[StreamOp])
pub fn stderr(bytes n: u64, terminal: bool) -> (Stderr, Served[StreamOp])
pub fn next[Op](mut s: Served[Op]) -> Option[Call[Op]] waits
pub fn dir_op(c: Call[DirOp]) -> DirOp
pub fn net_op(c: Call[NetOp]) -> NetOp
pub fn stream_op(c: Call[StreamOp]) -> StreamOp
pub fn payload[Op](c: Call[Op], field: u64, mut out: Bytes)
pub fn fill[Op](mut s: Served[Op], mut c: Call[Op], b: Bytes) -> u64
pub fn secret(c: Call[NetOp]) -> Secret
pub fn reply[Op](mut s: Served[Op], take c: Call[Op], take r: Reply[Op]) -> Result[(), Withdrawn]
pub fn dir_reply(r: DirReply) -> Reply[DirOp]
pub fn net_reply(r: NetReply) -> Reply[NetOp]
pub fn stream_reply(r: StreamReply) -> Reply[StreamOp]
pub fn clock(start_mono_ns: u64, start_wall_ms: u64) -> (Clock, ClockHand)
pub fn random(seed: u64) -> Random
pub fn process() -> (Process, StopTrigger)
pub fn advance_to(mut h: ClockHand, mono_ns: u64)
pub fn set_wall(mut h: ClockHand, wall_ms: u64)
pub fn idle(mut h: ClockHand) -> Option[u64] waits
pub fn trigger(take t: StopTrigger)
```

`advance_to`, `set_wall` and `idle` are in module `clock`, and `trigger` in module `process`.
- **Served ports.** `provide.dir`, `net`, `stdout` and `stderr` admit the port's room of `n`
  bytes, with a cell's 32, from the current limit and raise `Capacity` when it is refused;
  as a cell's, the room is held until the run ends, since the capability is plain and
  outlives the `Served` end. Each operation on the capability waits after issue as a `Call`
  queued on the port, in the order the operations were issued. `provide.next` returns the
  first queued call, and `provide.dir_op`, `net_op` and `stream_op` read its `Op`, which holds
  only scalars: a path, name or data field is its byte length, which `provide.payload(c,
  field, mut out)` appends to `out` (fields numbered from 0 in declaration order;
  `Bounds` when `out`'s spare capacity is short), and `room` is the spare capacity the
  caller offers for a result. `provide.fill` stages result bytes in the room, at most the
  call's `room`, and returns how many it took. No caller storage reaches the server, and
  no server storage reaches the caller until the reply.
- **Handles.** Ids name the server's own objects: 0 is the capability `provide` returned,
  and every other id is one the server issued in a reply (`File`, `Dir`, `Entries`,
  `Socket`, `Listener`). The caller receives a fresh handle of the ordinary type bound to
  the port, the kind and the id, so it reaches only that port's object of that kind, and a
  handle of another provider or another port given to an operation is refused before it
  reaches the server (`Denied`). A port's ids are one space, since `Released(id)` names no
  kind. Each issued id holds 32 bytes of the port's room until the server receives its
  `Released`; a `Dir` id, plain like the capability, holds them until the run ends. A reply
  that issues id 0, an id already live, or a variant that does not answer the call's
  operation fails the server with `Fail`, and a reply that issues an id when the room is
  short raises `Capacity` in the server; either way the call is settled as for a sealed
  port. `Close(file)` ends the caller's handle at issue, and its id once the server has
  taken the call; a `Close` cancelled before then queues `Released(file)` as a drop does.
  `NextEntries` bytes are whole records in `host.next_entries`'s format. `net.split` divides a socket in the caller
  and asks the server nothing; dropping a half queues `Released(id, Read)` or
  `Released(id, Write)` (FAIL-13).
- **Reply.** `provide.reply` is the commit point of the call: it copies the staged bytes into
  the caller's capacity and completes the operation with the reply's result. If
  cancellation or the call's deadline settled it first
  ([CONC-7](tasks.md#conc-7-cancellation-points)), it returns `Error(Withdrawn)`, discards
  the staged bytes and issues nothing. A `Call` given to the `reply` of another port is
  consumed as a dropped one and answers `Error(Withdrawn)`. A `Released` call needs no
  reply. A `Call` dropped without a reply settles its caller as a sealed port does.
- **Sealing.** Cleaning the `Served` end, or the end of its task, seals the port: every
  queued and later call completes with `Closed` (`Dir`, `Stdout`, `Stderr`) or `Reset`
  (`Net`), and drops of its handles still succeed. `provide.next` returns `None` only at
  quiescence ([FAIL-15](failure.md#fail-15-run-stops)).
- **Terminal bit.** `provide.stdout` and `provide.stderr` take the stream's terminal bit
  explicitly, and `stdout.is_terminal` and `stderr.is_terminal` on the result return it for
  the stream's whole life. It is answered by the capability itself, never queued on the port,
  and a sealed port still answers it. There is no virtual `Stdin`; a model-world stream
  answers `false`.
- **Net authority.** A `provide.net` `Net` permits every address, name and TLS name, and
  `net.narrow` narrows it. Its TLS handshakes are answered by its server, so it never
  authorizes a granted `Secret` ([TLS-5](tls.md#tls-5-secrets)). `net.join` reunites a
  virtual socket's halves in the caller and asks the server nothing
  ([NET-7](net.md#net-7-owned-connections)). `secret.scram_sha256` on a virtual TLS half
  reaches its server only as `Send` calls, and only for a sealed `Secret`
  ([TLS-6](tls.md#tls-6-scram-sha-256)).
- **Cells.** `host.mono_ns`, `host.wall_ms`, `host.random` and `host.stopped` on a cell's
  capability answer at issue from the cell, never switch task and never allocate.
  `provide.clock` starts at the given times. `clock.advance_to(mut h, t)` raises monotonic
  time to `t` (a smaller `t` changes nothing) and completes every sleep and expires every
  deadline at or before it, in deadline order and then issue order. `clock.set_wall` sets
  wall time alone. `clock.idle(mut h)` waits until every other task waits, then returns the
  earliest pending deadline on the clock, or `None` when there is none. `provide.random(seed)`
  answers each draw with the next bytes of a SplitMix64 stream from `seed`, eight
  little-endian bytes per step, so the same seed gives the same bytes on every engine.
  `provide.process()` gives a `Process` whose `host.stopped` completes once
  `process.trigger` has run, including a trigger before the call; dropping a `StopTrigger`
  without triggering never stops. Dropping a `ClockHand` freezes its clock: pending sleeps
  and deadlines on it never fire.
- **Replay.** A virtual provider's results are computed by the run, so they are not
  inputs of [CAP-8](capabilities.md#cap-8-the-replay-point): a replay recomputes them. The
  host operations a server makes pass the replay point as every provider operation does.
  A seeded simulation is a test-run configuration over today's task order, not an order
  promise.

