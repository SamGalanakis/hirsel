# Core library

The core library is source: one signature file per core module under
[`core/core/`](../../core/core/), 22 files. Each file is a module ([LANG-7](language.md#lang-7-names));
its functions end at their headers, and a type declared there with no body is built in, so
its summary is the one the table below gives it.

| module | file | rules |
|---|---|---|
| `core` (the prelude: every core type, `fail`, `replace`, `clone`, `clone_in`, `build_in`, `finish`) | [core.hirsel](../../core/core/core.hirsel) | LANG-7, MEM-4, MEM-17, OWN-13, FAIL-2 |
| `option`, `result` | [option](../../core/core/option.hirsel), [result](../../core/core/result.hirsel) | FAIL-2 |
| `int`, `float`, `char` | [int](../../core/core/int.hirsel), [float](../../core/core/float.hirsel), [char](../../core/core/char.hirsel) | SEM-1 to SEM-3 |
| `box`, `rc`, `arc`, `unique` | [box](../../core/core/box.hirsel), [rc](../../core/core/rc.hirsel), [arc](../../core/core/arc.hirsel), [unique](../../core/core/unique.hirsel) | MEM-13 to MEM-18 |
| `list`, `string`, `text`, `map`, `span`, `dest` | [list](../../core/core/list.hirsel), [string](../../core/core/string.hirsel), [text](../../core/core/text.hirsel), [map](../../core/core/map.hirsel), [span](../../core/core/span.hirsel), [dest](../../core/core/dest.hirsel) | SEM-3 to SEM-6, MEM-3, MEM-4, MEM-12 |
| `task`, `channel` | [task](../../core/core/task.hirsel), [channel](../../core/core/channel.hirsel) | CONC-1 to CONC-11 |
| `args`, `host`, `stdout`, `stderr` | [args](../../core/core/args.hirsel), [host](../../core/core/host.hirsel), [stdout](../../core/core/stdout.hirsel), [stderr](../../core/core/stderr.hirsel) | PROG-3, CAP-5 |

A library module may extend a core module by name (LANG-7).

## Built-in types

This table is the complete summary ([OWN-2](ownership.md#own-2-type-summaries)) of every
type that is built in: the builtin type names of [LANG-10](language.md#lang-10-built-in-types)
and the bodiless declarations of the core files. A bound column never lists `plain`:
`plain` holds exactly for a type of plain kind ([OWN-4](ownership.md#own-4-bounds)), so
`List[T]` and `Box[T]` are own and never `plain`, whatever `T` is. "When T" means the type
has each of the listed bounds exactly when every type argument has that bound; a holds
entry "T's" is the union of its type arguments' holds sets. Tuples, `Option` and `Result`
take their kind and copyability from OWN-3 and their other bounds componentwise; `Inline`
takes its kind from `T`. Sizes are logical ([MEM-24](budgets.md#mem-24-logical-charges)). A
scoped value is never stored in a field of an own, stable or resource type, a collection or
an environment (OWN-19), so it has no heap size; a scoped type's own fields
([OWN-22](ownership.md#own-22-scoped-types)) and a callable's logical width
([LANG-13](language.md#lang-13-callables)) are unaffected.

| type | kind | bounds | size | holds |
|---|---|---|---|---|
| integers, floats | plain | all; floats not `key` or `ordered` | width | nothing |
| `bool`, `char`, `()` | plain | all | 1, 4, 0 | nothing |
| tuples, `Option[T]`, `Result[T, E]` | OWN-3 | `shareable`, `send`, `key`, `ordered` when T | tuple: sum; variant rule (MEM-24) | T's |
| `Inline[T, n]` | T's kind (plain or own) | `shareable`, `send`, `key`, `ordered` when T | n × size(T) | T's |
| `Never` | plain | all | 0 | nothing |
| `String` | own | `shareable`, `send`, `key`, `ordered` | 24 | nothing |
| `List[T]` | own | `shareable`, `send`, `key`, `ordered` when T | 24 | T's |
| `Map[K, V]` | own | `shareable`, `send` when K and V | 24 | K's and V's |
| `Box[T]` | own | `shareable`, `send`, `key`, `ordered` when T | 8 | T's |
| `Rc[T]` | counted | `shareable`; `key`, `ordered` when T | 8 | T's |
| `Arc[T]` | counted | `shareable`, `send`; `key`, `ordered` when T | 8 | T's |
| `View[T]` | scoped, copyable | `key`, `ordered` when T | | scoped value, and T's |
| `Region`, `Dest[T]` | scoped; `Dest` moves | none | | scoped value |
| `Text` | stable | `key`, `ordered` | 16 | stable reference |
| `FixedSpan[T]` | stable | `key`, `ordered` when T | 16 | stable reference, and T's |
| `Link[T]` | a compact field only (MEM-10) | none | its holder's `W` | nothing |
| `Child[T]` | plain | `shareable`, `send`, `key` | 16 | nothing |
| `SpanBuilder[T, in r]`, `Builder[T, in r]` | own, affine | none | 16 | stable reference, opaque owner |
| `Unique` | plain | `shareable`, `send`, `key` | 8 | nothing |
| `Task[R]` | resource, mechanical | none | 8 | resource, and R's |
| `Sender[T]`, `Receiver[T]` | resource, mechanical | `send` | 24 | resource |
| `HostFile` | resource, mechanical | `send` | 8 | resource |
| `Dir`, `Net`, `Clock`, `Random`, `Stdout`, `Stderr`, `Stdin`, `Env`, `Process`, `Tasks` | plain | `shareable`, `send` | 8 | capability |
| `Args` | plain | `shareable`, `send` | 8 | nothing |
| `Site` | plain | `shareable`, `send`, `key` | 8 | nothing |
| `Causes` | own | `send` | 8 | opaque owner |

`Args` designates the entry arguments, which are host memory outside every limit, immutable
and live for the run; `args.get` returns a view of one. `Causes` owns its chain's cause
cells, which were charged when each cause was raised
([FAIL-10](failure.md#fail-10-cause-chain)); dropping it refunds them. The core declarations
that have bodies (`Option`, `Result`, `Capacity`, the error types) have the summaries OWN-2
computes, with `Option` and `Result` taking bounds as tuples do. `Task[R]` is never `send`
([CONC-2](tasks.md#conc-2-the-send-bound)); its parameter's `R: send` constrains the
result, not the handle.

## Contracts the signatures do not state

Each item gives the domain, result, length and capacity, custody and partial effects of the
operations it names; nothing else about them may be inferred from an implementation.

- **Errors.** An operation that fails with `Capacity` returns in its error every argument it
  would have taken and leaves its other arguments unchanged. Only the failures named here or
  by a rule occur: `Bounds`, `Overflow`, `Division` and `Unwrap` are failures
  ([FAIL-2](failure.md#fail-2-causes)), every other error is a `Result`.
- **Unwrap.** `option.unwrap(take x)` returns the `Some` payload and fails with `Unwrap` on
  `None`. `result.unwrap(take x)` returns the `Ok` payload; on `Error(e)` it fails with
  `Unwrap`, and `e` is cleaned by the failure's cleanup as an ordinary owner.
- **Allocation.** `list.new`, `string.new`, `map.new`, `list.with_capacity(0)` and an empty
  `span.builder` allocate nothing; `list.with_capacity(n)` admits exactly `n` elements and
  `span.with_capacity(r, n)` exactly `8 + n × size(T)`, rounded up to 8, its count header
  even when `n` is 0; `text.copy_in` admits its count header even for empty text.
  Finishing a builder admits nothing and refunds nothing. `list.reserve(mut xs, n)` leaves
  capacity at least `len + n`; every other allocating operation grows by
  [MEM-24](budgets.md#mem-24-logical-charges). `string.from` copies a literal or a view.
- **Literals.** A string literal has type `String`, is static, allocates nothing and is never
  charged; it has capacity 0, so the first push onto it grows it into new storage, and no
  operation writes static storage. A byte-string literal `b".."` is not static: it has type
  `Result[Bytes, Capacity]`, admits its length as `List[u8]` storage and copies its bytes.
- **Strings.** `string.into_bytes(take s)` transfers `s`'s owned storage unchanged, with its
  length, capacity and charge; a static `s` is copied into newly admitted storage of capacity
  equal to its length, and on `Capacity` the error returns `s`. `string.from_utf8` returns its
  argument when it is not UTF-8. `string.push_u64` appends `n` in ASCII decimal, with no sign
  and no leading zeros (`0` for zero). `string.next_scalar(s, at)` returns `None` when `at`
  is `byte_len(s)`, and fails with `Bounds` when `at` is greater or falls inside a scalar.
- **Slices.** `list.slice(xs, lo, hi)` and `string.slice(s, lo, hi)` view the elements or
  bytes from `lo` up to `hi`; they fail with `Bounds` when `lo > hi` or `hi` exceeds the
  length, and `string.slice` also when `lo` or `hi` falls inside a scalar. A slice's
  capacity is its length; `string.as_bytes` reports its `String`'s capacity, 0 for static
  storage. No view grants spare capacity.
- **Lists.** `list.set` returns the old element and `list.get` returns `None` out of range,
  where `xs[i]` and `list.set` fail with `Bounds`. `list.fill(mut xs, n, x)` appends `n`
  copies of `x`, growing once to at least `len + n`. `list.sum` adds the elements from index
  0, returns 0 for an empty list, and fails with `Overflow` at the first sum that does not
  fit. A `List` is cleaned element by element from index 0.
- **Maps.** `map.insert` returns its key and value with `Capacity`. `map.set(mut m, k, take
  v)` returns `Ok(old)` when `k` is present, after installing `v` in place, and `Error(v)`
  when it is absent; it copies no key and admits nothing. `map.remove` drops the stored key
  and returns the value. A `Map` is cleaned entry by entry in insertion order, each key
  before its value.
- **Integers.** The `integer` and `float` bounds ([OWN-4](ownership.md#own-4-bounds)) say
  which numeric types a core parameter ranges over. `int.widen` is accepted only where
  every `T` fits in `U`, and `int.truncate` only where `U` is no wider than `T`
  ([SEM-1](semantics.md#sem-1-integers)); `int.narrow` fails with `Overflow`; `int.truncate` keeps
  the low bits of the destination's width. `wrapping_neg` and `wrapping_shl` work modulo 2^width, `wrapping_shl` taking
  its count modulo the width. `checked_div` returns `None` for a zero divisor and for `MIN /
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
  numeric conversions.
- **Host.** A path is resolved relative to its `Dir` and may not leave it, through every
  traversal, symbolic links included. `OpenMode`:
  `Read` opens an existing file for reading (`NotFound` when absent); `Write` opens for
  writing, creating the file when absent and emptying it otherwise; `Append` opens for
  writing at the end, creating it when absent; `CreateNew` creates a file for writing and
  returns `Exists` when one is present. `host.read` appends to `buf` at most its spare
  capacity, advances the file's cursor by the count and returns it, `Ok(0)` at end of file
  or when `buf` has no spare capacity; a read that returns an error has appended nothing
  and moved no cursor, so a partial read is delivered as `Ok`. A successful `host.write`
  committed all of `b`; a `WriteError`'s `written` bytes stay written. `host.try_lock` tries
  an exclusive advisory lock without waiting: `Ok(true)` when the handle holds it (also when
  it already did), `Ok(false)` when another holder has it, and on an error it acquires
  nothing. `host.unlock` of an unlocked handle returns `Ok(())`; a failed unlock leaves the
  lock held. `host.close` consumes its file whatever it returns and always ends its lock;
  an error reports the host's close outcome. `host.mkdir` creates one directory, `Exists`
  when present; `host.remove` removes one file or empty directory. `host.mkdtemp(d, mut
  name)` checks before issue that `name` has at least 32 bytes of spare capacity, failing
  with `Bounds` otherwise; it creates a new empty directory in `d`, appends its name (at
  most 32 ASCII bytes) to `name` and returns a `Dir` rooted at it, and on an error creates
  nothing and appends nothing. `host.remove_tree(d, name)` removes such a directory without
  following symbolic links; on an error it may have removed part of the tree, and nothing
  removed is restored. `host.wall_ms` is milliseconds since 1970 and
  `host.mono_ns` never decreases in a run. No host operation admits; every byte it returns
  lands in capacity the caller already holds. `HostFile` is a mechanical resource
  ([MEM-24](budgets.md#mem-24-logical-charges)): its finalizer closes it and ignores the
  outcome. `Net`, `Random`, `Stdin`, `Env` and `Process` have no operations in v1.
- **Other results.** `box.unbox` returns the payload and refunds the node. `ParseError`'s
  `Invalid` holds a byte position. `Causes` is iterated by `for`, primary cause first. A
  `Box`, `Rc` or `Arc` is read where a view of its payload is expected.
  `task.check_cancel()` fails with `Cancelled` when a request is pending, and
  `channel.new(0)` fails with `Bounds`.
- **Built-in forms.** `dest.put(mut d, L)` accepts only a construction literal `L`
  ([MEM-4](memory.md#mem-4-sized-destinations)). `clone` and `clone_in` are typing forms
  whose result type selects what is copied ([MEM-17](memory.md#mem-17-clone)).
