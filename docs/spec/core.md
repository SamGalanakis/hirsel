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
Likewise, a `pub const` there may end at its type, `pub const os: Os`, and
the toolchain then supplies its value from the build's target; module `target`'s `os` and
`arch` are the only such constants ([LANG-20](language.md#lang-20-constants)).

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
| `order` | [order](../../core/core/order.hirsel) | SEM-7 |
| `check` | [check](../../core/core/check.hirsel) | SEM-8, SEM-11, MEM-30 |
| `sha1` | [sha1](../../core/core/sha1.hirsel) | SEM-8, MEM-30 |
| `sha256` | [sha256](../../core/core/sha256.hirsel) | SEM-8, MEM-30 |
| `ed25519`, `p256` | [ed25519](../../core/core/ed25519.hirsel), [p256](../../core/core/p256.hirsel) | SEM-8, MEM-30; [Signature verification](#signature-verification) |
| `sha512` | [sha512](../../core/core/sha512.hirsel) | SEM-8, MEM-30 |
| `rsa`, `p384` | [rsa](../../core/core/rsa.hirsel), [p384](../../core/core/p384.hirsel) | SEM-8, MEM-30; [Signature verification](#signature-verification) |
| `argon2` | [argon2](../../core/core/argon2.hirsel) | SEM-8, MEM-30; [Argon2id](#argon2id) |
| `box`, `rc`, `arc`, `unique` | [box](../../core/core/box.hirsel), [rc](../../core/core/rc.hirsel), [arc](../../core/core/arc.hirsel), [unique](../../core/core/unique.hirsel) | MEM-13 to MEM-18 |
| `list`, `string`, `text`, `map`, `span`, `dest` | [list](../../core/core/list.hirsel), [string](../../core/core/string.hirsel), [text](../../core/core/text.hirsel), [map](../../core/core/map.hirsel), [span](../../core/core/span.hirsel), [dest](../../core/core/dest.hirsel) | SEM-3 to SEM-6, MEM-3, MEM-4, MEM-12 |
| `charge` | [charge](../../core/core/charge.hirsel) | MEM-24, MEM-33 |
| `bytebuf` | [bytebuf](../../core/core/bytebuf.hirsel) | MEM-21, MEM-28 |
| `columns` | [columns](../../core/core/columns.hirsel) | Columns, SEM-6, OWN-9, MEM-24 |
| `deque` | [deque](../../core/core/deque.hirsel) | Deques and heaps, MEM-24 |
| `heap` | [heap](../../core/core/heap.hirsel) | Deques and heaps, MEM-24 |
| `seq`, `indexed`, `writer` | [seq](../../core/core/seq.hirsel), [indexed](../../core/core/indexed.hirsel), [writer](../../core/core/writer.hirsel) | MEM-4, MEM-12 |
| `task`, `channel` | [task](../../core/core/task.hirsel), [channel](../../core/core/channel.hirsel) | CONC-1 to CONC-11, CONC-15 |
| `conn` | [conn](../../core/core/conn.hirsel) | CONC-17 |
| `latest` | [latest](../../core/core/latest.hirsel) | CONC-13 |
| `shared` | [shared](../../core/core/shared.hirsel) | CONC-15 |
| `log`, `cell` | [log](../../core/core/log.hirsel), [cell](../../core/core/cell.hirsel) | MEM-29 |
| `code` | [code](../../core/core/code.hirsel) | PROG-9 |
| `flow` | [flow](../../core/core/flow.hirsel) | CONC-14 |
| `provide`, `clock`, `process` | [provide](../../core/core/provide.hirsel), [clock](../../core/core/clock.hirsel), [process](../../core/core/process.hirsel) | CAP-2, CAP-5, CONC-7, FAIL-13, FAIL-15, MEM-24, TLS-5; [Provide](#provide) |
| `net`, `tls`, `secret` | [net](../../core/core/net.hirsel), [tls](../../core/core/tls.hirsel), [secret](../../core/core/secret.hirsel) | NET-1 to NET-14, TLS-1 to TLS-6 |
| `foreign` | [foreign](../../core/core/foreign.hirsel) (raw handle type `Ptr`) | FFI-4 to FFI-7 |
| `exec` | [exec](../../core/core/exec.hirsel) | EXEC-2 to EXEC-8 |
| `cred` | [cred](../../core/core/cred.hirsel) | TLS-7 |
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
| `Columns[T]` | own | `shareable`, `send` when T | 24 | T's |
| `Deque[T]` | own | `shareable`, `send` when T | 32 | T's |
| `Heap[T]` | own | `shareable`, `send` when T | 24 | T's |
| `Box[T]` | own | `shareable`, `send`, `key`, `ordered` when T | 8 | T's |
| `Rc[T]` | counted | `shareable`; `key`, `ordered` when T | 8 | T's |
| `Arc[T]` | counted | `shareable`, `send`; `key`, `ordered` when T | 8 | T's |
| `Shared[T]` ([CONC-15](tasks.md#conc-15-units)) | counted | `shareable`, `send`, `cross`; `key`, `ordered` when T | 8 | T's |
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
| `UnixListener`, `UnixDatagram` ([NET-13](net.md#net-13-unix-streams), [NET-14](net.md#net-14-unix-datagrams)), `Proc` ([EXEC-3](exec.md#exec-3-spawn)) | resource, mechanical | `send` | 8 | resource |
| `Dir`, `Net`, `Clock`, `Random`, `Stdout`, `Stderr`, `Stdin`, `Env`, `Process`, `Tasks` | plain | `shareable`, `send` | 8 | capability |
| `Secret` ([TLS-5](tls.md#tls-5-secrets)) | resource, mechanical | `send` | 8 | resource |
| `KeyShare`, `Schedule`, `Sealer`, `Opener`, `HeaderMask` ([TLS-8](tls.md#tls-8-packet-ciphers-and-key-agreement)) | resource, mechanical | `send` | 8 | resource |
| `Foreign[I]` ([FFI-4](foreign.md#ffi-4-the-foreign-capability)) | plain | `shareable`, `send` | 8 | capability |
| `Exec` ([EXEC-2](exec.md#exec-2-launch-profiles-and-narrowing)) | plain | `shareable`, `send` | 8 | capability |
| `Cred` ([TLS-7](tls.md#tls-7-credential-grants)) | plain | `shareable`, `send` | 8 | capability |
| `foreign.Ptr[K]` ([FFI-6](foreign.md#ffi-6-handles)) | plain | `send` | 8 | a handle-table ordinal of kind `K`; holds a raw handle (OWN-2) |
| `Args` | plain | `shareable`, `send` | 8 | nothing |
| `Site` | plain | `shareable`, `send`, `key` | 8 | nothing |
| `Causes` | own | `send` | 8 | opaque owner |
| `Cell[T]` | own | none | 8 | opaque owner, and T's |
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
result, not the handle. `Cell[T]` is a handle to a separately admitted node that owns a
cell's payload, and `Log[E: inert]` is the `log` module's opaque library type over a
`Cell[List[E]]` ([MEM-29](memory.md#mem-29-append-only-logs-and-task-local-cells)), as
`Deque[T]` and `Heap[T]` are library types over `List`; and `StaticFn[F]` designates static
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
- **Aligned initial backing.** `list.with_capacity_aligned(n, A)` is `list.with_capacity(n)`
  whose initial backing starts at a multiple of `A`, a constant power of two (anything else
  is refused at compile time). The alignment applies to that backing only: growth and
  `clone` use the ordinary contract, so the storage of a list grown past `n`, or of a clone,
  need not be `A`-aligned. A caller that needs the alignment, such as a buffer for `Direct`
  writes, never grows the list past `n`. The alignment never enters a charge
  ([MEM-24](budgets.md#mem-24-logical-charges)): the charge is that of `with_capacity(n)`.
- **Literals.** A string literal has type `String`, is static, allocates nothing and is never
  charged; it has capacity 0, so the first push onto it grows it into new storage. This is
  sound because no operation writes a static `String`'s bytes: growth moves them into new
  storage, `truncate` and `clear` only shorten it, and the ASCII case changes copy it first. A `List` has in-place element writes
  (`list.set`, `xs[i] = x`), so an own `List` that has elements always owns its storage:
  capacity 0 means length 0, and no `List` is static. A byte-string literal (`b".."`,
  `br#".."#`) therefore has type `Bytes`: it admits its length as `List[u8]` storage and
  copies its bytes, and a refusal raises `Capacity`. When the expected type is
  `FrozenBytes`, the literal uses read-only static storage.
  The reference evaluator admits the same charge and refunds it on drop.
  An engine can omit that charge under [MEM-28](budgets.md#mem-28-charges-are-an-upper-bound).
  `bytebuf.freeze(take b)` transfers the existing charge, length and
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
  `list.copy_within(mut xs, src, dst, count)` copies the `count` elements of `xs`
  from `src` over those from `dst`, and `inline.copy_within(mut a, src, dst, count)`
  does the same inside an Inline array. The ranges may overlap: every destination
  element ends up holding the source element it had before the call, as if the
  source range were first copied to a temporary. Each raises `Bounds` before any
  write unless `src ≤ len`, `dst ≤ len` and `count ≤ len − max(src, dst)`, so an
  overflowing end is refused, and it checks both ranges even when `count` is 0 or
  the element type has no storage. Elements must be plain. The carrier keeps its
  length and capacity; the call admits no bytes and takes one Step whatever `count`
  is.
- **Shortening.** `list.truncate(mut xs, n)` keeps the first `n` elements and does
  nothing when `n ≥ len`. Otherwise the length becomes `n` first, and the elements
  from `n` up to the old length are then cleaned in ascending index order, the order
  in which `list.clear` cleans all of them. The capacity stays, so the call refunds
  only what the cleaned elements own, admits nothing, and takes one Step
  ([MEM-30](budgets.md#mem-30-step-bounds)); for plain elements it does no work per
  element.
- **Prefetch.** `list.prefetch(xs, i)` hints that element `i` of `xs` will be read
  soon, and `inline.prefetch(a, i)` does the same for an Inline array. Any index is
  allowed: one at or past the length, of an empty list, or near `2^64`, does nothing.
  Neither checks bounds, fails, admits bytes or takes a Step
  ([MEM-30](budgets.md#mem-30-step-bounds)), and neither has any observable effect, so
  an engine may emit nothing for it. Under LLVM each is one `llvm.prefetch` for a
  read with high locality at the element's address; the evaluator and Cranelift emit
  nothing. There are no locality or write parameters.
- **Inline lanes.** `inline.lanes(a, at)` reads bytes `at .. at + n` of an
  `Inline[u8, N]` as the expected `Simd[u8, n]`, `n` being 16 or 32, and
  `inline.set_lanes(mut a, at, v)` overwrites them with the lanes of `v`. Each raises
  `Bounds` unless `at + n ≤ N` with no overflow, checked once before the access; any
  other array or lane type is refused at compile time
  ([SEM-11](semantics.md#sem-11-vectors)). Neither admits bytes, and each takes one Step.
- **Words.** `list.word_le(b, at)` and `list.word_be(b, at)` read a little- or big-endian
  word from `b` at a byte offset. The expected `u16`, `u32` or `u64` type fixes the width.
  Without an expected type the result is `u64`. An invalid range, including an overflowing
  end, fails with `Bounds`. `list.put_word_le(mut b, x)` and `list.put_word_be(mut b, x)`
  append the two, four or eight bytes of `x` in the specified order. Growth follows
  [MEM-24](budgets.md#mem-24-logical-charges). A refused growth leaves the list unchanged.
  `list.set_word_le(mut b, at, x)` and `list.set_word_be(mut b, at, x)` overwrite the two,
  four or eight bytes of `b` from the byte offset `at` with `x` in that order, at any
  alignment; the type of `x`, which must be `u16`, `u32` or `u64`, fixes the width. Unless `at` plus the width is at most `len`,
  with no overflow, they fail with `Bounds` before any write. The list keeps its length and
  capacity; the call admits no bytes and takes one Step
  ([MEM-30](budgets.md#mem-30-step-bounds)).
  `inline.word_le` and `inline.word_be` read the same widths from an Inline byte
  array, and `inline.set_word_le(mut a, at, x)` and `inline.set_word_be(mut a, at, x)`
  overwrite them in one under the same rules. The expected type of a read, or the type of
  `x`, fixes the width, and an invalid range fails with `Bounds`.
  `list.compare_bytes(a, b)` returns the `Order` of `a` and `b` as `<` orders them: by the
  first differing byte read as unsigned, and otherwise the shorter first, so it is `Equal`
  only for equal bytes. It reads the common prefix once and fails in no case; it admits
  nothing and takes one Step, whatever the lengths, and returns the same `Order` on every
  engine ([SEM-8](semantics.md#sem-8-determinism)).
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
- **SHA-512 blocks.** `sha512.blocks(mut h, b, at, count)` applies the FIPS 180-4
  SHA-512 compression function to each of the `count` 128-byte blocks of `b` from `at`,
  in order, updating the eight `u64` state words `h` (an `Inline[u64, 8]`). A range outside
  `b`, including an end past `u64`, raises `Bounds` before `h` changes.
  `sha512.block(mut h, b)` compresses the one block of an `Inline[u8, 128]`. The charge is
  `sha256.blocks`' charge, unchanged: each call admits no bytes and takes one Step whatever
  `count` is ([MEM-30](budgets.md#mem-30-step-bounds)). SHA-384 uses the same compression
  from its own initial words, so there is one kernel for both. The kernel is Hirsel's own
  compression in `hirsel-core-rt`, beside the SHA-256 and SHA-1 ones, and not ring's:
  ring 0.17.14 offers SHA-384 and SHA-512 only as whole digests (`ring::digest::Context`),
  and its block function (`BlockContext`) is crate-private, so it cannot take given state
  words, as `start_from` and this kernel need. An engine may use the CPU's SHA-512
  instructions; the state words are the same on every CPU, so by
  [SEM-8](semantics.md#sem-8-determinism) a run behaves the same on every CPU.
  `std.sha512` extends this module with the streaming hash, shaped as `std.sha256` is:
  `Sha512` holds the eight words, a 128-byte pending block, its fill and the total byte
  count; `start`, `add(mut s, b, lo, hi)`, `add_byte`, `finish` (64 bytes, leaving the state
  usable) and `hash`, plus `start_from(h)`, which starts from given initial words. `add`
  completes the pending block first and compresses the remaining complete blocks in one
  core call. Its step count is bounded independently of the input length. The length field
  is the 128-bit big-endian bit count of the total, exact for every total below 2^64 bytes. `std.sha384` is the same streaming hash over a `Sha512` started from the
  FIPS 180-4 §5.3.4 SHA-384 initial words, and its `finish` and `hash` return the first
  48 bytes.
- **Lists.** `list.set` returns the old element and `list.get` returns `None` out of range,
  where `xs[i]` and `list.set` fail with `Bounds`. The `i` of `xs[i]` may be any unsigned
  integer, zero-extended (LANG-21); `list.get`, `list.set` and every other `u64` parameter
  still take a `u64`. An element is mutated in place through
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
  It may admit one scratch block of at most the list's length as a `List[T]`
  ([MEM-24](budgets.md#mem-24-logical-charges)) before moving any element, and refunds it
  before it returns; whether it does and how large it is are the build's
  ([MEM-28](budgets.md#mem-28-charges-are-an-upper-bound)). A refused scratch leaves `xs`
  unchanged. A failure inside `before` leaves `xs` holding every element it had, in an
  unspecified order, refunds the scratch and propagates. The number and order of calls to
  `before`, and so the steps they take, are the build's: fixed for one build and one input,
  and not the same across builds ([MEM-30](budgets.md#mem-30-step-bounds)).
- **Columns.** A `Columns[T]` holds a `List[T]`'s rows in one block stored by column: a
  record `T` has one column per field, named `c.f` wherever the field is visible, and any
  other `T` is one column. `columns.push`, `pop`, `set`, `swap_remove`, `clear`,
  `truncate`, `reserve` and `try_reserve` behave and charge as the `list` calls of the same
  name; `get` and `fill` copy rows and take a `plain` `T`. A row has no address and moves
  by value; an element is `c.f[i]`, and `c.f` is a view of a `List` of the field's type.
  Growth admits every column at once, so a refused growth leaves the length and the
  charge unchanged ([MEM-26](budgets.md#mem-26-growth-charges-the-current-limit)). A column is lent `mut` only to
  `list.set_lanes`, which keeps its length. A `Columns` is cleaned as a `List` is: rows in
  ascending index order, each row's owning columns in field order. It does not cross a unit
  boundary ([CONC-15](tasks.md#conc-15-units)), and a clone of one whose rows own storage
  is not built.
- **Deques and heaps.** `deque.push_back` and `deque.push_front` add at either end of a
  `Deque[T]`, and `heap.push` adds to a `Heap[T]`; a refused growth leaves the collection
  unchanged and `x` untaken. `deque.pop_front`, `deque.pop_back` and `heap.pop` remove and
  return an element, `None` when empty, and admit nothing; `heap.pop` returns a least element
  by `<` ([SEM-7](semantics.md#sem-7-equality-and-order)), and `heap.peek` views the
  element the next `pop` would return. A deque index counts from the front.
  `deque.get(d, i)` views element `i`, and `deque.front` and `deque.back` the first and last,
  each `None` past the end, as `list.get` does. `deque.set(mut d, i, take x)` replaces
  element `i` and returns the old one. `deque.remove(mut d, i)` removes and returns element
  `i`, keeping the others in order and moving at most `min(i, len - 1 - i)` of them.
  `deque.swap_remove_back` and `deque.swap_remove_front` remove and return element `i`,
  moving the back or front element into its place. Reads and these four admit nothing; the four fail with
  `Bounds` when `i` is not below the length, as `list.set` does, leaving `d` unchanged and `x`
  untaken. `new` admits nothing and `len` counts elements. Both
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
- **Order.** `order.compare(a, b)` returns the `Order` that `<` decides for every
  `ordered` type: `Less` when `a < b`, `Greater` when `b < a`, and `Equal` otherwise. It
  orders the two values once, leaving at the first component that differs, so a `String`,
  `Text` or `List[u8]` is read over its common prefix once. It fails in no case, admits
  nothing and takes one step whatever the values, as each core call does
  ([MEM-30](budgets.md#mem-30-step-bounds)). `std.order` extends this module with `min`, `max`
  and the `Order` combinators.
- **Integers.** The `integer` and `float` bounds ([OWN-4](ownership.md#own-4-bounds)) say
  which numeric types a core parameter ranges over. `int.widen` is accepted only where
  every `T` fits in `U`, and `int.truncate` only where `U` is no wider than `T`
  ([SEM-1](semantics.md#sem-1-integers)); `int.narrow` fails with `Overflow`; `int.truncate` keeps
  the low bits of the destination's width. `wrapping_neg` and `wrapping_shl` work modulo 2^width, `wrapping_shl` taking
  its count modulo the width. `int.rotate_left(x, n)` and `int.rotate_right(x, n)` rotate the
  width's bits of `x` (a signed `x` by its two's-complement bits) by the `u32` count `n` modulo
  the width, and `int.swap_bytes(x)` reverses the bytes of `x`, so it returns `x` on an 8-bit type.
  None of the three fails or admits bytes, and each call takes one Step
  ([MEM-30](budgets.md#mem-30-step-bounds)), whatever the count. `int.abs(a)` is `a` on an unsigned type; on a signed type it is `-a` for
  a negative `a` and `a` otherwise, so `int.abs(MIN)` fails with `Overflow` at the call, as `-MIN` does. `checked_div` returns `None` for a zero divisor and for `MIN /
  -1`. `int.mul_wide(a, b)` returns the high and low halves of the exact 128-bit product.
  `int.ilog2(x)` accepts only an unsigned `T` and returns `None` for 0, otherwise `Some` of
  floor(log2(x)) as a `u32`; it neither fails nor admits bytes and takes one Step. `int.parse` reads ASCII decimal digits, with one leading `-` for a signed `T` and no other
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
  the file when absent and never emptying it. `Durable` (Store v2.92, not yet built) is `Write` whose every
  completed write survives a host crash, as if `host.sync` had returned after it. `Direct`
  is `Durable` whose `write_at` and `host.zero_at(f, at, len)` (write `len` zeros at `at`)
  take offsets, lengths and buffer addresses that are multiples of 4,096, else `Other(22)`
  with no effect; the host checks this on every provider. A crash while such a write is
  issued keeps any subset of its 512-byte sectors old or new, in the sim fs and the crash
  bridge too. Completion of a `Durable` or `Direct` write means its bytes are durable only when it succeeds;
  a write that fails can leave bytes readable that a crash takes back. A process's exit does not wait for
  its in-flight direct writes, so another process can run while they land: a program that hands such a
  file to another process orders the handoff itself, as Store does with an exclusive `host.try_lock` on
  the writing handle, which the kernel holds until the last request issued through it completes
  (Store §3.3b, v2.98). A read never returns bytes the host failed to read: an unreadable page is an
  error for the read that reaches it, never zeros. `host.read` appends to `buf` at most its spare
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
  `host.entries(d, at: Option[String])` opens a `HostEntries` listing of `d`'s root for `None`, or
  of the directory at `p` for `Some(p)`, with `host.open_dir`'s confinement and refusals.
  The listing is a mechanical resource whose finalizer closes its descriptor. `host.next_entries(mut h, mut buf)` appends whole records
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
  cancellation never interrupts one. `host.mkstemp(d, mut name) ->
  Result[HostFile, IoError] waits` is `host.mkdtemp` for a regular file: the same 32-byte
  spare-capacity check (`Bounds` before issue), the same prefix rules and fresh suffix, one
  exclusive create that never opens an existing file, and a result open for writing as
  `CreateNew` opens one. It is a cancellation point at issue only, and a cancellation
  observed at delivery closes the handle and leaves the file created. The issue records
  `d`'s ordinal, the prefix and spare capacity, and delivery records the file's ordinal
  and suffix. The provider's `DirOp` carries `Mkstemp(dir: u64, prefix: u64, room: u64)`,
  answered `File(u64)` with the suffix staged by `provide.fill`, and `Entries(dir: u64, path:
  u64)`, where path length 0 means `None`.
  ([file](../../core/std/api/file.md#replace), [dir](../../core/std/api/dir.md#listing-below-a-path)).
  `host.wall_ms` is milliseconds since 1970 and
  `host.mono_ns` never decreases in a run. `host.sleep_until(c, deadline_ns)` returns once
  `host.mono_ns(c) >= deadline_ns`, at once for a deadline already reached; it is a
  cancellation point at its entry and while it waits
  ([CONC-7](tasks.md#conc-7-cancellation-points)), and its completion passes the replay
  point ([CAP-8](capabilities.md#cap-8-the-replay-point)). A relative sleep is
  library code over it. `host.sleep_until_wall(c, at_ms)` is the absolute wall-time wait:
  it returns once `host.wall_ms(c) >= at_ms`, at once for a reading already reached, and
  the run is not woken for it before then. The host provider measures what remains on its
  monotonic clock from a fresh pair of readings, and reads both clocks again when that
  measure runs out, so a wall clock set back while it waits lengthens the wait. A wall
  clock set forward does not shorten a measure already running. It is a cancellation
  point and passes the replay point as `host.sleep_until` does. No host operation admits; every byte it returns
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

## SHA-512

The kernel is the SHA-512 blocks rule above; `std.sha512` and `std.sha384` are library code
over it. This section holds their laws and build slice.

**One backend.** Every new kernel here and in [TLS-5](tls.md#tls-5-secrets) runs on ring
0.17.14, the crate the existing signature kernels, `wrap`, `sign` and rustls already link, through
wrappers in `hirsel-core-net` beside the existing ones: AES-GCM and ChaCha20-Poly1305
(`ring::aead`), HKDF-SHA256 (`ring::hkdf`), RSA and P-384 verification
(`ring::signature`), and the SHA-384 inside P-384 verification. Two things ring lacks are
not on ring. Argon2id is RustCrypto `argon2` 0.6.0 ([Argon2id](#argon2id)), the one
outside crypto crate for kernels; ring has no memory-hard function. (TLS-8's simulation
backend adds RustCrypto `p256` and `x25519-dalek` for virtual key draws only; open items,
below.) The SHA-512 block kernel is
Hirsel's own compression, as SHA-256's and SHA-1's are, for the reason above.

### SHA-512 laws

The CAVP files are `SHA384ShortMsg.rsp`, `SHA384LongMsg.rsp`, `SHA512ShortMsg.rsp` and
`SHA512LongMsg.rsp` in [shabytetestvectors.zip](https://csrc.nist.gov/CSRC/media/Projects/Cryptographic-Algorithm-Validation-Program/documents/shs/shabytetestvectors.zip),
copied byte for byte.

| law | bug | oracle and setup |
|---|---|---|
| **H1** CAVP | a wrong round constant, rotation or initial word, or SHA-384 returning the wrong 48 bytes | Every record of the four files: `sha512.hash` or `sha384.hash` of `Msg` (the first `Len / 8` bytes) equals `MD`. Also check the published NIST `abc` and 896-bit two-block examples for both hashes. |
| **H2** padding and streaming | a wrong pad at the 112-byte boundary, a 64-bit length field in the 128-bit slot, or a pending block lost between `add` calls | For every length 0 to 300 of the bytes `i mod 251`, and every split of the input into two `add` calls, `sha512.hash` and `sha384.hash` equal Python `hashlib`'s `sha512` and `sha384` digests, which the test reads from a checked-in table that `hashlib` generated, not from this build. A million `a` bytes streamed 1000 bytes per `add` give the million-`a` digests published with FIPS 180-2's SHA-384 and SHA-512 examples. |
| **H3** `finish` keeps the state | `finish` mutating the state it was given | After `finish` on the first 100 bytes, adding the remaining 200 and finishing equals the one-shot digest of all 300, for both hashes. |
| **H4** steps do not grow with the input | a per-block charge, or `add` compressing block by block in user code | On the reference evaluator, let `k` be the least bound under which `bound steps k { let mut s = take st; sha512.add(mut s, b, 0, list.len(b)); s }` succeeds for a 256-byte `b`, with `st` from `start()` and `b` bound outside the scope. The same scope under `k` also succeeds for a 1 MiB `b`, and for a 1 MiB `b` after a 5-byte `add` (so the pending path runs). The invariant is MEM-30's: a core call is one step whatever `count` is, and `add` has no loop over blocks. |

The laws are cases in [sha512.hirsel](../../core/std/tests/sha512.hirsel), run by the shared walker with
`--backend all`.

### SHA-512 build slice

| slice | size | depends on | contents | laws |
|---|---|---|---|---|
| **S5** SHA-512 | S | — | [Core SHA-512](../../core/core/sha512.hirsel) (`blocks`, `block`), `RtOp::Sha512Blocks` lowered as `Sha256Blocks` is, its C entry over a `hirsel-core-rt` `sha512.rs` shaped as `sha256.rs` is (portable rounds, plus the CPU's SHA-512 instructions where present), the evaluator arm; [std.sha512](../../core/std/sha512.hirsel) and [std.sha384](../../core/std/sha384.hirsel) with their API pages. | H1–H4 |

## Signature verification

The modules `ed25519`, `p256`, `p384` and `rsa` check signatures. The curve modules
provide functions of type `(key: Bytes, msg: Bytes, sig: Bytes) -> bool`: `ed25519.verify`,
`p256.verify`, `p256.verify_der`, `p384.verify` and `p384.verify_der`. The `rsa` module
takes the key as its two integers, `(n: Bytes, e: Bytes, msg: Bytes, sig: Bytes) -> bool`:
`rsa.verify_pkcs1_sha256` and `rsa.verify_pss_sha256`. Each returns `true` exactly when
`sig` is a valid signature of all of `msg` under the key by the rule below, and `false`
otherwise. Every input that is not well formed gives `false`, including a key or signature
of the wrong length, and no input makes the call fail. The answer does not say which check
failed. That is how ring reports it, and a caller checking untrusted bytes
needs no failure path for them. Together these verify every signature an OpenID Connect
provider issues under the algorithms JWS registers for it: `RS256`, `PS256`, `ES256`,
`ES384` and `EdDSA` with Ed25519 ([RFC 7518 §3.1](https://www.rfc-editor.org/rfc/rfc7518#section-3.1),
[RFC 8037](https://www.rfc-editor.org/rfc/rfc8037)).
Signing is `secret.sign` ([TLS-5](tls.md#tls-5-secrets)), and ephemeral key agreement is
`secret.key_share`, consumed by a `schedule` constructor ([TLS-8](tls.md#tls-8-packet-ciphers-and-key-agreement)):
both keep the private key in a `Secret` or a `KeyShare`, never in program bytes, and the
agreed value never leaves the schedule.

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
- **P-384.** `p384.verify` and `p384.verify_der` are the P-256 rule over NIST P-384 with
  SHA-384, word for word, with these sizes: `e` is the SHA-384 digest mod `n`, `key` is 97
  bytes `0x04 || X || Y`, and `p384.verify`'s `sig` is 96 bytes `r || s`, each 48 bytes
  big-endian, the JWS `ES384` form. The same forms give `false` (compressed, hybrid, bare
  96-byte and infinity keys), only strict DER is accepted, and a high `s` is accepted. The
  kernel is ring 0.17.14's `ECDSA_P384_SHA384_FIXED` and `ECDSA_P384_SHA384_ASN1` through
  `UnparsedPublicKey::verify`, as P-256's is. ring parses only the uncompressed point
  (`parse_uncompressed_point` refuses any first byte but `0x04` and any other length), and
  hashes `msg` with its own SHA-384, so no digest truncation rule applies: SHA-384's output
  is the curve's size.
- **RSA.** `n` and `e` are unsigned big-endian integers: a JWK's `n` and `e` after base64url
  decoding ([RFC 7518 §6.3.1](https://www.rfc-editor.org/rfc/rfc7518#section-6.3.1)), or
  the two fields of an X.509 `RSAPublicKey`. Leading zero bytes in either are ignored, as
  their values are what is specified. Each function returns `false` unless all of these hold:
  - `n` is odd, and its form without leading zero bytes is 256 to 1024 bytes long, so it
    has 2041 to 8192 significant bits. That is ring's `2048_8192` parameter set exactly:
    ring compares the modulus's length rounded up to whole bytes with 2048 bits, and its
    exact bit length with 8192. A smaller modulus is not a key a verifier should trust, and
    8192 bounds the work.
  - `e` is odd and `3 ≤ e < 2^33`, ring's range.
  - `sig` has exactly `k` bytes, `k` being the byte length of `n`, and read as an integer it
    is at least 1 and below `n` ([RFC 8017 §8.2.2](https://www.rfc-editor.org/rfc/rfc8017#section-8.2.2)
    steps 1 and 2a; a zero signature never verifies, and ring refuses it before the
    exponentiation). A shorter signature is not padded with zeros, and `n` is not
    subtracted from a larger one.

  `rsa.verify_pkcs1_sha256` is RSASSA-PKCS1-v1_5 with SHA-256 (JWS `RS256`). The encoded
  message `sig^e mod n` must equal, byte for byte, the EMSA-PKCS1-v1_5 encoding of `msg`'s
  SHA-256 digest ([RFC 8017 §9.2](https://www.rfc-editor.org/rfc/rfc8017#section-9.2)),
  whose `DigestInfo` carries the SHA-256 OID with a NULL parameter. The check encodes and
  compares; it never parses the padding, so no BER variant, no garbage after the digest and
  no `DigestInfo` without its NULL is accepted, though RFC 8017's note permits the last.
  `rsa.verify_pss_sha256` is RSASSA-PSS with SHA-256, MGF1 with SHA-256 and a salt of
  exactly 32 bytes (JWS `PS256`, which fixes the salt to the hash length, [RFC 7518 §3.5](https://www.rfc-editor.org/rfc/rfc7518#section-3.5)),
  verified by EMSA-PSS-VERIFY ([RFC 8017 §9.1.2](https://www.rfc-editor.org/rfc/rfc8017#section-9.1.2)):
  trailer `0xbc`, the bits above `emBits` zero, the padding `PS` zero then `0x01`, and `H`
  equal to the recomputed hash. The salt length is not detected from the signature, so a
  signature made with another salt length gives `false`.

  The kernel is ring 0.17.14's `RsaPublicKeyComponents { n, e }.verify` with
  `RSA_PKCS1_2048_8192_SHA256` or `RSA_PSS_2048_8192_SHA256`. ring refuses a leading zero
  byte in `n` or `e`, so the wrapper strips them first; an `e` of only zero bytes is then
  empty, which ring refuses. ring checks the rest of the rules above itself: the modulus
  size, odd `n`, `e`'s range and parity, `sig`'s length and range, and, for PKCS#1 v1.5,
  encode-and-compare (`pkcs1_encode`, then a byte comparison). ring's PSS takes the salt
  length from the hash: its `PSSMetrics` sets `sLen = hLen`, 32 bytes for SHA-256, which is
  the fixed 32-byte salt this rule and `PS256` require, so the rule needs no salt argument
  and no check of the wrapper's own. MGF1 uses the same hash. Verification uses only public
  values.
- **Charges.** Each call takes one step ([MEM-30](budgets.md#mem-30-step-bounds)) and admits
  no bytes, whatever the lengths of its arguments. The charge is 1 step per call plus
  0 steps per message byte in the reference count, as `sha256.blocks` charges. The work is bounded
  by the length of `msg`, which a limit bounds, and by the key size, which the rules above
  bound: 8192 bits for RSA. MEM-30's per-unit exceptions, `secret.scram_sha256` and
  Argon2id, cover work that a peer or a stored cost chooses.
- **Determinism.** The answer is a function of the input byte strings alone. Verification
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
| **SV10** reference charge | a kernel lowered as library code that loops, or a charge per byte or per block | On the reference evaluator, `bound bytes 0 { bound steps 1 { ed25519.verify(k, m, s) } }` gives `Ok(Ok(true))` for TEST 1. The same scope over `p256.verify` gives `Ok(Ok(true))` for NIST record 4, whose message is 128 bytes, and over `p256.verify_der` gives `Ok(Ok(true))` for tcId 5. The arguments are locals bound outside the scopes. The expected value follows by hand from MEM-30: a core call is one step and admits nothing. |
| **SV11** P-384 accepts and rejects | a wrong curve or hash, a P-256 routine reused, or a Wycheproof case read the other way | Over all of `ecdsa_secp384r1_sha384_p1363_test.json` with `p384.verify` and all of `ecdsa_secp384r1_sha384_test.json` with `p384.verify_der`: each `valid` case gives `true` and each `invalid` case gives `false`. Neither file has an `acceptable` case. With NIST records counted within `[P-384,SHA-384]` of the same `SigVer.rsp`, each record gives `true` exactly when its `Result` is `P`. |
| **SV12** P-384 forms, DER and high S | a compressed key decoded, a BER-lenient parser, or low-S normalisation | In `ecdsa_secp384r1_sha384_test.json`, `p384.verify_der` gives `true` for tcIds 5 and 7 (its high-S twin), and `false` for tcIds 6 (`s` without its leading zero), 8 and 9 (BER sequence lengths). In the P1363 file, tcIds 1 and 147 (high `s`) give `true`, and tcIds 164 (`r > n`) and 165 (`s > n`) give `false`. With tcId 1's message and signature, the key compressed by `Qy`'s parity, and the bare 96-byte `Qx \|\| Qy`, give `false`; tcId 1's signature cut to 95 bytes gives `false`. None of them fails the call. |
| **SV13** RS256 accepts | a wrong DigestInfo, a hash over the wrong bytes, or a big-endian/little-endian slip in `n` or `e` | `rsa_signature_2048_sha256_test.json` tcIds 1 to 7, with `n` and `e` from the group's `publicKey.modulus` and `publicKey.publicExponent`, give `true`. The JWS of [RFC 7515 Appendix A.2](https://www.rfc-editor.org/rfc/rfc7515#appendix-A.2), with `msg` its ASCII JWS Signing Input and `n` and `e` from its JWK, gives `true`, and with the last byte of the signing input changed gives `false`. tcId 1 with one leading zero byte added to `n` gives `true`. |
| **SV14** PKCS#1 v1.5 is encode-and-compare | a padding parser that accepts BER, trailing garbage or a missing NULL (Bleichenbacher 2006 forgeries against `e = 3`) | Over all of `rsa_signature_2048_sha256_test.json`, each `invalid` case gives `false`, among them tcIds 236 and 237 (the message not hashed), 244 and 245 (`sig ≥ n`) and 246 (two bytes too long). tcId 8, which Wycheproof marks `acceptable` (`MissingNull`), gives `false`: the expected value follows from §9.2's encoding, which the rule compares byte for byte. |
| **SV15** PS256 with a fixed salt | salt-length auto-detection, a wrong MGF hash, or a wrong `emBits` mask | Over all of `rsa_pss_2048_sha256_mgf1_32_test.json` with `rsa.verify_pss_sha256`, each `valid` case gives `true` (tcIds 73 and 74 among them: salts of all zero and all one bits) and each `invalid` case gives `false`. Each `valid` case of `rsa_pss_2048_sha256_mgf1_0_test.json` gives `false`, since its salt is empty and the rule fixes 32 bytes. |
| **SV16** RSA key bounds | a small or even key accepted, or a bad key that fails the call | Each gives `false` and none fails the call: the nine SHA-256 cases of `rsa_pkcs1_1024_sig_gen_test.json` (marked `acceptable` with `SmallModulus`), SV13's tcId 1 with `e` replaced by `01 00 00` (even) and by `01` (below 3), with `e` given as nine bytes `01 00 00 00 00 00 00 00 01` (≥ 2^33), with its signature cut to 255 bytes, and with the modulus replaced by either an even 256-byte integer or the 8201-bit odd integer encoded as `01`, 1023 zero bytes, then `01`. |
| **SV17** reference charge | a per-byte charge, or RSA's modular exponentiation lowered as a loop | On the reference evaluator, `bound bytes 0 { bound steps 1 { rsa.verify_pkcs1_sha256(n, e, m, s) } }` gives `Ok(Ok(true))` for SV13's tcId 1, the same scope over `rsa.verify_pss_sha256` gives `Ok(Ok(true))` for `rsa_pss_2048_sha256_mgf1_32_test.json` tcId 1, and over `p384.verify` for its P1363 tcId 1. The arguments are locals bound outside the scopes. By hand from MEM-30, a core call is one step and admits nothing. |

The laws are cases in the core contract harness, in `core/core/tests/ed25519.hirsel`,
`core/core/tests/p256.hirsel`, `core/core/tests/p384.hirsel` and `core/core/tests/rsa.hirsel`.
SV11, SV14 and SV15 walk whole Wycheproof files, which are checked in under
`core/core/tests/vectors/` byte for byte from the pinned commit. They take no capability, so the shared walker's `--backend all`
runs each on the evaluator, Cranelift dev and LLVM release ([README](../../core/std/tests/README.md)).

### Build slice

| slice | size | depends on | contents | laws |
|---|---|---|---|---|
| **SV** signature verification | S | — | `core/core/ed25519.hirsel` and `core/core/p256.hirsel` with the three bodiless headers, and their rows in `core_ops.rs` as `Lane::Value` kernels. Separate functions in `hirsel-core-net` wrap ring 0.17.14's `ED25519`, `ECDSA_P256_SHA256_FIXED` and `ECDSA_P256_SHA256_ASN1`. The `RtOp`s `Ed25519Verify`, `P256Verify` and `P256VerifyDer` take key, message and signature pointer and length pairs and return a `u64` 0 or 1. The C entries `hirsel_core_ed25519_verify`, `hirsel_core_p256_verify` and `hirsel_core_p256_verify_der` in `hirsel-core-rt` each call the corresponding wrapper, as does the evaluator's `RtOp` arm (its `eval` feature already links `hirsel-core-net`), so compiled code and the evaluator answer from the same code. Cranelift and LLVM lower these as native calls, as `Sha256Blocks` is lowered. There is no `HostOp`, provider path or recording change. The module row's `unbuilt` mark is removed when the slice lands. | SV1–SV10 |
| **SR** P-384 and RSA | S | SV | `core/core/p384.hirsel` and `core/core/rsa.hirsel` with the four bodiless headers, their `core_ops.rs` rows as `Lane::Value` kernels, the `RtOp`s `P384Verify`, `P384VerifyDer`, `RsaVerifyPkcs1Sha256` and `RsaVerifyPssSha256` (pointer and length pairs in, `u64` 0 or 1 out), wrappers in `hirsel-core-net` beside the ring ones over ring 0.17.14's `ECDSA_P384_SHA384_FIXED`, `ECDSA_P384_SHA384_ASN1`, `RSA_PKCS1_2048_8192_SHA256` and `RSA_PSS_2048_8192_SHA256` (the RSA wrapper stripping leading zero bytes from `n` and `e` before `RsaPublicKeyComponents::verify`), the C entries in `hirsel-core-rt`, and the evaluator's arms calling the same wrappers. Lowered as native calls, as SV's kernels are. | SV11–SV17 |

## Argon2id

```hirsel core
pub type Argon2Cost { memory_kib: u32, passes: u32, lanes: u32 }
pub type Argon2Error = Params | Capacity(Capacity)
pub fn id(password: Bytes, salt: Bytes, key: Bytes, data: Bytes, cost: Argon2Cost, len: u64, mut out: Bytes) -> Result[(), Argon2Error]
```

`argon2.id` computes Argon2id version `0x13`, the function `H` of
[RFC 9106](https://www.rfc-editor.org/rfc/rfc9106) §3, with `password` as `P`, `salt` as
`S`, `key` as the secret value `K`, `data` as the associated data `X`, `memory_kib` as `m`,
`passes` as `t`, `lanes` as `p` and `len` as the tag length `T`. It appends the `len` tag
bytes to the spare capacity of `out`. `key` and `data` are empty in ordinary password
hashing; they are there because RFC 9106's own test vector uses them. Lanes are computed
one after another on the calling thread, which gives the same tag as parallel lanes. Only
version `0x13` exists here: RFC 9106 specifies no other.

The results, decided in this order:

- `Params` before any work, admitting nothing and taking no step beyond the call's own,
  when any of these fails: `1 ≤ lanes ≤ 2^24 − 1`, `memory_kib ≥ 8 × lanes`, `passes ≥ 1`,
  `4 ≤ len ≤ 2^32 − 1`, `8 ≤ len(salt) ≤ 2^32 − 1`, `len(password) ≤ 2^32 − 1`,
  `len(key) ≤ 2^32 − 1` and `len(data) ≤ 32`. All but the last two bounds are RFC 9106 §3.1's;
  the eight-byte salt floor and the 32-byte associated-data ceiling are the RustCrypto
  crate's, refused here explicitly rather than inherited as a hidden failure.
- `Capacity(c)` before any work, with `len` as `requested`, when the tag does not fit the
  spare of `out`. Nothing is appended.
- The work charge, below. When the current bound has too few steps it raises `OutOfTime` at
  the call, having admitted and computed nothing.
- `Capacity(c)` when the limit refuses the working memory, below, with nothing computed.
- `Ok(())` with the tag appended.

**Charge.** The call takes one step, as every core call does, and then one further step
for each 1 KiB block compression the parameters choose: `passes × m′`, where
`m′ = 4 × lanes × ⌊memory_kib / (4 × lanes)⌋` is the block count RFC 9106 §3.2 rounds
`memory_kib` down to. The steps are taken at the call, before the memory is admitted and
before any work, as `secret.scram_sha256` takes its iteration steps
([MEM-30](budgets.md#mem-30-step-bounds)). This is MEM-30's per-unit exception and not the
one step of `sha256.blocks`, because here the work is not bounded by any length a limit
bounds: `passes` alone can make a call with small arguments run for an hour, and a
stored hash that names its own cost is outside input. The
initial and final BLAKE2b hashing is bounded by the argument lengths and is covered by the
call's own step. The call then admits `1024 × m′` bytes of working memory from the
current limit ([MEM-24](budgets.md#mem-24-logical-charges)), zeroes it after the tag is
computed and refunds it before it returns. The tag lands in capacity the caller holds, so
nothing else is admitted.

**Determinism and timing.** The tag is a function of the arguments alone. The kernel is a
pure computation: it passes no replay point and replay computes it again. The engine may
use the crate's vector code; the tag is the same on every CPU (SEM-8). Argon2id's first
half pass addresses memory independently of the password, and the rest addresses it by
the data, as RFC 9106 §3.4 designs; no further constant-time property is required or
claimed. The kernel is RustCrypto `argon2` 0.6.0 with `Algorithm::Argon2id`,
`Version::V0x13` and `Argon2::new_with_secret` for a nonempty `key`, computing with
`hash_password_into_with_memory` over the admitted blocks.

**Std.** `std.argon2` is library code over the kernel. Its `PhcError` has two cases,
`Malformed` and `Argon2(Argon2Error)`.

```hirsel
fn hash(password: Bytes, salt: Bytes, cost: Argon2Cost) -> Result[String, std.argon2.PhcError] {
  std.argon2.hash(password, salt, cost)
}
fn verify(password: Bytes, phc: Bytes) -> Result[bool, std.argon2.PhcError] {
  std.argon2.verify(password, phc)
}
fn recommended() -> Argon2Cost { std.argon2.recommended() }
```

`hash` computes a 32-byte tag and writes
`$argon2id$v=19$m=<m>,t=<t>,p=<p>$<salt>$<tag>`, the PHC string format that libargon2 and
RustCrypto write, with decimal parameters without leading zeros and the salt and tag in
standard base64 without padding. `verify` parses exactly that form, recomputes with the
decoded salt, the parameters and the decoded tag's length, and compares the tags with
`hmac.equal`, whose time depends only on the lengths. Any other text is `Malformed`: another
variant (`argon2i`, `argon2d`), a version other than `19` or none, parameters out of order
or repeated, a `keyid` or `data` field, base64 with padding or outside the alphabet, and a
tag shorter than 4 bytes. A well-formed string whose parameters the kernel refuses gives
`Argon2(Params)`. The salt is the caller's, drawn with `host.random`; `std.argon2` takes no
capability. `recommended` returns RFC 9106 §4's second recommendation, `m = 65536`, `t = 3`,
`p = 4`, which costs `196,609` steps and 64 MiB.

A password sealed as a `Secret` derives a key with `secret.derive_argon2id`
([TLS-5](tls.md#tls-5-secrets)). It uses the same Argon2id parameters, block steps
and working-memory charge. TLS-5 defines the child policy, storage charge and
refusal order.

### Argon2id laws

In the scoped laws, `out` is moved into the scope and returned with the result, since
MEM-30 refuses a body that lends `mut` a place declared outside it; the other arguments are
locals bound outside.

| law | bug | oracle and setup |
|---|---|---|
| **AR1** RFC vector | a wrong `H0` (missing `K` or `X`, wrong length prefixes), wrong addressing, or Argon2i or Argon2d computed | `argon2.id` with `P` 32 bytes `01`, `S` 16 bytes `02`, `K` 8 bytes `03`, `X` 12 bytes `04`, `m = 32`, `t = 3`, `p = 4` and `len = 32` appends RFC 9106 §5.3's tag. |
| **AR2** reference vectors and PHC | a lane-count or memory-size slip that only shows at larger `m`, or a PHC encoding that other implementations do not read | The seven `Argon2_id` `hashtest` cases in [phc-winner-argon2](https://github.com/P-H-C/phc-winner-argon2) `src/test.c` (password `password` or `differentpassword`, salt `somesalt`, `m` from 2^8 to 2^18, `t` from 1 to 4, `p` 1 or 2): `argon2.hash` returns each case's PHC string exactly, and `argon2.verify` of it gives `true`. The same string with its tag's last character changed so the decoded tag differs gives `false`. |
| **AR3** parameter refusals | a refusal after work, or a crate error that fails the call | Each gives `Params`, appends nothing and, under `bound bytes 0 { bound steps 1 { ... } }` with its arguments bound outside, returns `Ok(Ok(Error(Params)))`: `m = 15, p = 2`; `t = 0`; `p = 0`; `p = 2^24`; a 7-byte salt; `len = 3`; 33 bytes of `data`. |
| **AR4** per-block charge | the work counted as one step, `m` not rounded down, or the steps taken after the work | With AR2's `m = 256, t = 2, p = 2` case (`m′ = 256`, so 512 blocks), `bound steps 513 { ... }` succeeds and `bound steps 512 { ... }` gives `Error(OutOfTime(steps: 512))` with nothing appended to `out`. With `m = 259` and otherwise the same, `m′` is still 256 and the same two bounds give the same two results. By hand from RFC 9106 §3.2 and MEM-30. |
| **AR5** memory admission | working memory taken from nowhere, kept after return, or admitted before the steps | Under a limit with `262,143` bytes free, AR4's call gives `Capacity(c)` with `c.requested = 262,144`; with `262,144` free it succeeds, and afterwards the limit has `262,144` free again. |
| **AR6** PHC strictness | a lenient parser that accepts a downgraded variant or version | Starting from AR2's first string, `argon2.verify` gives `Malformed` for `$argon2i$` in its place, `v=16`, `v=` removed, `t=2,m=65536` swapped, `m=065536`, a trailing `=` on the salt, and `,keyid=AAAA` after the parameters. |

The laws are cases in `core/core/tests/argon2.hirsel` and `core/std/tests/argon2.hirsel`
with `--backend all`. AR2's larger cases take a quarter of a GiB and a fraction of a second
each on native engines; on the evaluator only the `m = 256` cases run.

### Argon2id build slices

| slice | size | depends on | contents | laws |
|---|---|---|---|---|
| **CK** the `argon2` crate | S | — | `argon2 =0.6.0` (default features off, `alloc` on) through `third-party/` and reindeer, with its dependency closure (`blake2`, which computes RFC 9106's `H` and `H′`, among it), each with its licence file. It is the only RustCrypto crate the kernels use. No kernel is added in this slice. | builds and lints |
| **A2** Argon2id | M | CK | `Argon2Cost` and `Argon2Error` in the prelude, `core/core/argon2.hirsel` with `id`, its `core_ops.rs` row, `RtOp::Argon2id` taking the seven arguments as pointer and length pairs and words, the per-block steps in the reference evaluator and in every engine (MEM-30's list), the working-memory admission and refund, the native C entry and the evaluator arm over one wrapper; `core/std/argon2.hirsel` and its [API page](../../core/std/api/argon2.md). | AR1–AR6 |

### Open

*Not normative.* Questions the crypto kernels (SHA-512, P-384, RSA, Argon2id here; the AEAD
and derivation sinks in [TLS-5](tls.md#tls-5-secrets)) leave for a later decision:

Two earlier questions are closed. The backend: every new kernel runs on ring 0.17.14 but
Argon2id, on RustCrypto `argon2` because ring has none, and the SHA-512 block kernel, which
is Hirsel's own as SHA-256's is ([SHA-512](#sha-512)). The one exception is TLS-8's
simulation backend: RustCrypto `p256` 0.14.0 and `x25519-dalek` 3.0.0 compute key shares and
P-256 signatures from virtual draws, because ring takes randomness only through its
`SecureRandom` and cannot be fed an ordered stream. Host keys stay on ring
(media-crypto-amend-2227; reversible if ring gains a scalar-from-bytes constructor). The `rsa` crate's age and RUSTSEC-2023-0071: no `rsa` crate is used.

1. **Argon2 associated data** is capped at 32 bytes, the crate's limit, where RFC 9106
   allows `2^32 − 1`. No known format needs more.
2. **No cost floor.** `argon2.id` and `std.argon2.hash` accept any RFC-valid cost, where
   SCRAM refuses fewer than 4096 iterations. A floor for `hash` alone (OWASP's
   `m = 19456, t = 2`) would refuse weak new hashes without refusing old stored ones.
3. **More JWS algorithms.** `RS384`, `RS512`, `PS384` and `PS512` are left out; each is a
   row in this section over ring's `RSA_PKCS1_2048_8192_SHA384`, `_SHA512`,
   `RSA_PSS_2048_8192_SHA384` or `_SHA512` and its Wycheproof file when a provider needs it.
   `ES512` needs P-521, which ring 0.17.14 lacks, so it would reopen the backend question.
4. **HKDF on program bytes** and HKDF over SHA-512 are not specified; the first is library
   code over `std.hmac` with no kernel, and ring's `HKDF_SHA512` would back the second.
   HKDF over SHA-384 on secrets is not a sink either: TLS-8's schedules run it inside,
   under fixed labels, and no generic Extract or Expand on a `Secret` exists.
5. **A pepper under `Secret`.** Argon2's secret value `K` is program bytes in `argon2.id`;
   a `secret.argon2id_hash` that takes `K` from a `Secret` is not specified.
6. **Random-nonce AEAD at scale.** XChaCha20-Poly1305 or AES-GCM-SIV would let callers
   draw nonces without the `2^32` message bound, and let `wrap` drop its per-blob HKDF.
   ring 0.17.14 offers neither, so either would reopen the backend question.
7. **Key state left after a call.** ring 0.17.14 implements no `Drop` that zeroes its AEAD
   key schedules (`LessSafeKey`) or its HMAC and HKDF keys (`hmac::Key`, `Prk`), so the
   AEAD and derivation sinks, like the MAC sink, `wrap` and `sign`, promise run-owned
   memory and no charge, not zeroing of that state. The secret's own bytes and Argon2id's
   working memory, which Hirsel holds, are still zeroed as their rules say. Zeroing ring's
   state would need a ring that does it or an allocator that clears what the sinks free.
   TLS-8 erases only what Hirsel owns: schedule secrets, sealer and opener IVs, and the VAES
   `AesKey` and `HTable` built in place in the slot. The state of its ring values
   (ChaCha20-Poly1305 and non-VAES AES-GCM keys, HMAC-SHA1 keys, ephemeral and ECDSA keys)
   falls under this item too, as do earlier copies, registers and stack.

## Provide

`provide` makes capabilities of virtual construction
([CAP-2](capabilities.md#cap-2-capabilities-are-opaque)) and answers their operations in the
program's own code ([CAP-5](capabilities.md#cap-5-provider-operations)). Callers see nothing
new: a `Dir`, `Net`, `Exec`, `Stdout`, `Stderr`, `Clock`, `Random` or `Process` from `provide` has its
ordinary type and supports the same operations as a host grant.
A `provide.net` port also answers the Unix operations ([NET-14](net.md#net-14-unix-datagrams)).
`provide.exec` makes an `Exec` answered on a `Served[ExecOp]` port
([EXEC-8](exec.md#exec-8-exec-at-the-replay-point-and-served-exec)).

Version note (2026-11-25, served-reply-table-2694): `DirOp` gains `ZeroExtension`,
`TryLockShared` and `ZeroAt` and `DirReply` gains `ZeroExtensionSupported`, as
[core.hirsel](../../core/core/core.hirsel) already declared them, and **Handles** now lists
the reply each operation takes. Before this note the runtime refused `Locked` for
`TryLockShared`, so a served port's server failed on the only reply it could give.

```hirsel core
pub opaque resource type Served[Op]
pub opaque resource type Call[Op]
pub opaque type Reply[Op]
pub opaque resource type ClockHand
pub opaque resource type StopTrigger
pub type Withdrawn = Withdrawn()
pub type Part = Whole | Read | Write
pub type DirOp = Open(dir: u64, path: u64, mode: OpenMode) | OpenDir(dir: u64, path: u64) | Mkdtemp(dir: u64, prefix: u64, room: u64) | Stat(dir: u64, path: u64) | Mkdir(dir: u64, path: u64) | Remove(dir: u64, path: u64) | RemoveTree(dir: u64, path: u64) | Rename(dir: u64, from: u64, to: u64) | SyncDir(dir: u64) | Entries(dir: u64, path: u64) | Read(file: u64, room: u64) | ReadAt(file: u64, at: u64, room: u64) | Write(file: u64, len: u64) | WriteAt(file: u64, at: u64, len: u64) | StatFile(file: u64) | Sync(file: u64) | TryLock(file: u64) | Unlock(file: u64) | Close(file: u64) | NextEntries(entries: u64, room: u64) | Released(id: u64) | Mkstemp(dir: u64, prefix: u64, room: u64) | ZeroExtension(dir: u64) | TryLockShared(file: u64) | ZeroAt(file: u64, at: u64, len: u64)
pub type DirReply = Done | Failed(IoError) | WriteFailed(WriteError) | Count(u64) | Locked(bool) | Stat(Stat) | File(u64) | Dir(u64) | Entries(u64) | ZeroExtensionSupported(bool)
pub type NetOp = Connect(addr: Addr, public: bool) | Resolve(name: u64, room: u64) | Listen(addr: Addr, backlog: u16) | Accept(listener: u64) | Recv(socket: u64, room: u64, max: u64) | Send(socket: u64, prefix: u64, tail: u64) | Handshake(socket: u64, name: u64, alpn: u64, system: bool) | SendSecret(socket: u64, wire: u64, secrets: u64, marks: u64) | Released(id: u64, part: Part) | ConnectUnix(root: u64, path: u64) | ListenUnix(root: u64, path: u64, backlog: u16) | AcceptUnix(listener: u64) | OpenDatagram | BindDatagram(root: u64, path: u64) | ConnectDatagram(datagram: u64, root: u64, path: u64) | SendDatagram(datagram: u64, len: u64) | SendTo(datagram: u64, root: u64, path: u64, len: u64) | RecvDatagram(datagram: u64, room: u64) | Ready(socket: u64) | LocalAddr(id: u64) | PeerAddr(socket: u64) | UdpOpen(v6: bool, slots: u32, slot_bytes: u32) | UdpBind(addr: Addr, slots: u32, slot_bytes: u32, members: u16, steer: Steer) | UdpRecv(socket: u64, room: u64, entries: u64) | UdpSend(socket: u64, data: u64, pkts: u64) | UdpLocal(socket: u64) | Hello(socket: u64, room: u64) | Serve(socket: u64, chain: u64, alpn: u64)
pub type NetReply = Done | Failed(NetError) | Count(u64) | Socket(u64, Addr) | Listener(u64, Addr) | Secured(Handshake) | UnixSocket(u64) | UnixListening(u64) | Datagram(u64) | Packet(len: u64, truncated: bool) | Address(Addr) | UdpSockets(first: u64, count: u16, addr: Addr) | UdpPackets(count: u64) | Accepted(protocol: Option[u64])
pub type StreamOp = Write(len: u64)
pub type StreamReply = Done | WriteFailed(WriteError)

pub fn dir(bytes n: u64) -> (Dir, Served[DirOp])
pub fn net(bytes n: u64) -> (Net, Served[NetOp])
pub fn stdout(bytes n: u64, terminal: Option[TermSize]) -> (Stdout, Served[StreamOp])
pub fn stderr(bytes n: u64, terminal: Option[TermSize]) -> (Stderr, Served[StreamOp])
pub fn next[Op](mut s: Served[Op]) -> Option[Call[Op]] waits
pub fn dir_op(c: Call[DirOp]) -> DirOp
pub fn net_op(c: Call[NetOp]) -> NetOp
pub fn stream_op(c: Call[StreamOp]) -> StreamOp
pub fn payload[Op](c: Call[Op], field: u64, mut out: Bytes)
pub fn payload_prefix[Op](c: Call[Op], field: u64, max: u64, mut out: Bytes) -> u64
pub fn fill[Op](mut s: Served[Op], mut c: Call[Op], b: Bytes) -> u64
pub fn secret(c: Call[NetOp], i: u64) -> Secret
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
  `Bounds` when `out`'s spare capacity is short). `provide.payload_prefix` appends only the
  first `max` bytes and returns the number of bytes omitted. It checks spare capacity
  against the bytes to append. A settled call supplies no bytes and returns zero.
  The `room` field is the spare capacity the caller offers for a result.
  `provide.fill` stages result bytes in the room, at most the
  call's `room`, and returns how many it took. No caller storage reaches the server, and
  no server storage reaches the caller until the reply.
- **Handles.** Ids name the server's own objects: 0 is the capability `provide` returned,
  and every other id is one the server issued in a reply (`File`, `Dir`, `Entries`,
  `Socket`, `Listener`). The caller receives a fresh handle of the ordinary type bound to
  the port, the kind and the id, so it reaches only that port's object of that kind, and a
  handle of another provider or another port given to an operation is refused before it
  reaches the server (`Denied`). A port's ids are one space, since `Released(id)` names no
  kind. Each issued id holds 32 bytes of the port's room until the server receives its
  `Released`; a `Dir` id, plain like the capability, holds them until the run ends. A
  `DirOp` is answered by `Failed` or by its own variants: `File` for `Open` and `Mkstemp`;
  `Dir` for `OpenDir` and `Mkdtemp`; `Stat` for `Stat` and `StatFile`; `Entries` for
  `Entries`; `Count` for `Read`, `ReadAt` and `NextEntries`; `Done` or `WriteFailed` for
  `Write` and `WriteAt`; `Locked` for `TryLock` and `TryLockShared`;
  `ZeroExtensionSupported` for `ZeroExtension`; and `Done` for every other operation, `ZeroAt`
  included. `Released` takes no reply. A reply
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
  (`Net`), and drops of its handles still succeed. A sealed `Net` port retires its dropped
  `UdpSocket` ids at the seal and later ones at their drop, since no server will take their
  `Released` ([NET-16](net.md#net-16-udp-sockets-and-batches)). `provide.next` returns `None` only at
  quiescence ([FAIL-15](failure.md#fail-15-run-stops)).
- **Terminal.** `provide.stdout` and `provide.stderr` take the stream's terminal
  explicitly: `None` is not a terminal, and `Some(size)` is a terminal of that size. For the
  stream's whole life `stdout.is_terminal` and `stderr.is_terminal` on the result return
  whether it is `Some`, and `stdout.size` and `stderr.size` return the size, or `None` when
  either dimension is 0 ([Size](../../core/std/api/stdin.md#size)). Both are answered by the
  capability itself, never queued on the port, and a sealed port still answers them. There
  is no virtual `Stdin`; a model-world stream answers `false` and `None`.
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
  wall time alone and completes every `host.sleep_until_wall` it reaches, in reading order
  and then issue order. A pending wall wait is no deadline of `clock.idle`'s. `clock.idle(mut h)` waits until every other task waits, then returns the
  earliest pending deadline on the clock, or `None` when there is none. `provide.random(seed)`
  answers each draw with the next bytes of a SplitMix64 stream from `seed`, eight
  little-endian bytes per step, so the same seed gives the same bytes on every engine.
  `provide.random_tape(take b)` supplies successive whole draws from `b`, or raises
  `Bounds` without moving the cursor if the next draw does not fit. The cell retains
  its 32-byte charge and `len(b)` until run teardown, as specified in [TLS-8](tls.md#tls-8-packet-ciphers-and-key-agreement).
  `provide.process()` gives a `Process` whose `host.stopped` completes once
  `process.trigger` has run, including a trigger before the call; dropping a `StopTrigger`
  without triggering never stops. Dropping a `ClockHand` freezes its clock: pending sleeps
  and deadlines on it never fire.
- **Replay.** A virtual provider's results are computed by the run, so they are not
  inputs of [CAP-8](capabilities.md#cap-8-the-replay-point): a replay recomputes them. The
  host operations a server makes pass the replay point as every provider operation does.
  A virtual capability is `cross`, so it can reach another unit
  ([CONC-15](tasks.md#conc-15-units)). There its operations depend on another unit's order,
  so they pass the replay point in the unit that issues them, and so does every operation
  on the same cell in its home unit after the home's frame that first carries a
  capability of the cell out of the unit
  ([unit recordings](../recording.md#unit-recordings)).
  A seeded simulation is a test-run configuration over today's task order, not an order
  promise.

