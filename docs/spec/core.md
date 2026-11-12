# Core library

The core library is source: one signature file per core module under
[`core/core/`](../../core/core/). Each file is a module ([LANG-7](language.md#lang-7-names));
its functions end at their headers, and a type declared there with no body is built in, so
its kind, bounds, size and layout are the ones this specification gives it.

| module | file | rules |
|---|---|---|
| `core` (the prelude: every core type, `fail`, `replace`, `clone`, `clone_in`, `build_in`, `prepare_in`, `finish`) | [core.hirsel](../../core/core/core.hirsel) | LANG-7, MEM-4, MEM-17, OWN-13, FAIL-2 |
| `int`, `float`, `char` | [int](../../core/core/int.hirsel), [float](../../core/core/float.hirsel), [char](../../core/core/char.hirsel) | SEM-1 to SEM-3 |
| `box`, `rc`, `arc`, `unique` | [box](../../core/core/box.hirsel), [rc](../../core/core/rc.hirsel), [arc](../../core/core/arc.hirsel), [unique](../../core/core/unique.hirsel) | MEM-13 to MEM-18 |
| `list`, `string`, `text`, `map`, `span`, `dest` | [list](../../core/core/list.hirsel), [string](../../core/core/string.hirsel), [text](../../core/core/text.hirsel), [map](../../core/core/map.hirsel), [span](../../core/core/span.hirsel), [dest](../../core/core/dest.hirsel) | SEM-3 to SEM-6, MEM-3, MEM-4, MEM-12 |
| `task`, `channel` | [task](../../core/core/task.hirsel), [channel](../../core/core/channel.hirsel) | CONC-1 to CONC-11 |
| `args`, `host`, `stdout`, `stderr` | [args](../../core/core/args.hirsel), [host](../../core/core/host.hirsel), [stdout](../../core/core/stdout.hirsel), [stderr](../../core/core/stderr.hirsel) | PROG-3, CAP-5 |

A library module may extend a core module by name (LANG-7). The scalar types, `bool`,
`char` and `()` are builtin type names and are not declared.

## Contracts the signatures do not state

- **Errors.** An operation that fails with `Capacity` returns in its error every argument it
  would have taken and leaves its other arguments unchanged. Only the failures named here or
  by a rule occur: `Bounds` and `Overflow` are failures
  ([FAIL-2](failure.md#fail-2-causes)), every other error is a `Result`.
- **Allocation.** `list.new`, `string.new` and `map.new` allocate nothing;
  `list.with_capacity(n)` admits exactly `n` elements; `list.reserve(mut xs, n)` leaves
  capacity at least `len + n`; every other allocating operation grows by
  [MEM-24](budgets.md#mem-24-logical-charges). A string literal has type `String`, is
  static, allocates nothing and is never charged; `string.from` copies a literal or a view.
- **Failures.** `list.slice`, `string.slice` and `list.set` fail with `Bounds` out of range,
  as `xs[i]` does where `list.get` returns `None`; `list.sum` and `int.narrow` fail with
  `Overflow`; `channel.new(0)` fails with `Bounds`; `task.check_cancel()` fails with
  `Cancelled` when a request is pending.
- **Results.** `list.set` returns the old element. `map.set(mut m, k, take v)` replaces the
  value of an existing key and returns the old one, or returns `Error(v)` when `k` is
  absent; it admits nothing. `string.from_utf8` returns its argument when it is not UTF-8.
  `host.read` fills the spare capacity of `buf` and returns the bytes read. `WriteError`
  holds the bytes written before the error, and `ParseError`'s `Invalid` its byte position.
  `Causes` is iterated by `for`, primary cause first, and is made of the chain's cells
  ([FAIL-10](failure.md#fail-10-cause-chain)). A `Box`, `Rc` or `Arc` is read where a view
  of its payload is expected.
- **Numbers.** In `int`, `T` and `U` range over the integer types; in `float`, `F` over
  `f32` and `f64` and `I` over the integer types. `int.widen` is accepted only where every
  `T` fits in `U`; `int.truncate` keeps the low bits; `float.from_int` and `float.narrow`
  round to nearest, ties to even; `float.to_int` rounds toward zero and returns `NaN` or
  `OutOfRange`; the bit conversions are exact. These, with `char.from_u32` and
  `char.to_u32`, are all the numeric conversions.
- **Host.** A path is resolved relative to its `Dir` and may not leave it. `OpenMode`:
  `Read` opens an existing file for reading (`NotFound` when absent); `Write` opens for
  writing, creating the file when absent and emptying it otherwise; `Append` opens for
  writing at the end, creating it when absent; `CreateNew` creates a file for writing and
  returns `Exists` when one is present. `host.mkdtemp(d)` creates a new empty directory in
  `d` and returns it with its name in `d`, which `host.remove_tree(d, name)` removes.
  `host.wall_ms` is milliseconds since 1970 and `host.mono_ns` never decreases in a run.
  None of the host operations admits; reads fill capacity the caller already holds.
  `HostFile` is a mechanical resource ([MEM-24](budgets.md#mem-24-logical-charges)): its
  finalizer closes it and ignores the outcome. `Net`, `Random`, `Stdin`, `Env` and
  `Process` have no operations in v1. The capability types are plain and `send`.
- **Built-in forms.** `dest.put(mut d, L)` accepts only a construction literal `L`
  ([MEM-4](memory.md#mem-4-sized-destinations)).
