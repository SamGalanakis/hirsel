# Failure

Expected outcomes are values of `Result` ([LANG-18](language.md#lang-18-errors)). Bugs and
cancellation are **failures**: they abandon the current computation, clean up every
exited scope, and end the task. Everything a failure leaves behind has exactly one owner.

### FAIL-1: Errors, failures and run stops

An **error** is an `Error(e)` value; it is handled by ordinary code. A **failure** is the
abandonment of a task's computation with a cause (FAIL-2); it runs cleanup (FAIL-8) and is
observed only by the task's joiner (FAIL-14). A **run stop** ends the whole run without
cleanup (FAIL-15). No construct converts one into another, except that `join` delivers a
child's failure to its parent as an error value.

### FAIL-2: Causes

A failure has a primary cause from this closed list, and a site, the operation that
raised it:

| cause | raised by |
|---|---|
| `Overflow` | checked integer arithmetic and conversion ([SEM-1](semantics.md#sem-1-integers)) |
| `Bounds` | indexing, slicing and `span.get` out of range |
| `Division` | integer division or remainder by zero |
| `Unwrap` | `option.unwrap` of `None`, `result.unwrap` of `Error` |
| `Fail(text)` | `fail(msg)`, which has type `Never` |
| `Cancelled` | a cancellation point that observes cancellation ([CONC-7](tasks.md#conc-7-cancellation-points)) |
| `Capacity` | an allocation beyond a finalizer's grant ([MEM-29](budgets.md#mem-29-finalizer-grants)) |

`Capacity` outside a finalizer is an error, never a failure.

```hirsel
fn mean(xs: List[i64]) -> i64 { list.sum(xs) / int.narrow(list.len(xs)) }   // Division when xs is empty
```

### FAIL-3: Commit points

Every operation has a **commit point**: the first step after which it cannot fail. Before
it, the operation consumes nothing: inputs that its error type lists come back in the
error, and every other input stays where it was. For allocating operations the commit point
is admission ([MEM-23](budgets.md#mem-23-admission)). A callee's effects on its `mut`
arguments before its own failure are not undone.

```hirsel
fn put(mut q: List[File], take f: File) -> Result[(), (Capacity, File)] {
  list.try_push(mut q, take f)          // on Capacity the error owns f
}
```

### FAIL-4: Constructor temporaries

A record or variant construction evaluates its field expressions left to right into
statement temporaries; a placed literal `T(..) in r` and `box.new` then admit their block.
If a field expression or the admission fails,
the temporaries are dropped by the statement's cleanup in reverse evaluation order, their
finalizers included; on admission failure the error is bare `Capacity`
([T1.1, T1.2](ownership.md#own-26-the-transition-table)).

```hirsel
fn pair(d: Dir) -> Result[Pair, IoError] {
  Ok(Pair(a: open_temp(d)?, b: open_temp(d)?))   // if b fails, a's temporary is dropped once
}
```

### FAIL-5: Multi-step operations

An operation that allocates in several steps, such as `clone`, `clone_in` and map
insertion that grows, drops or releases its own partial results in reverse, each refunded,
before it returns its error. A callback's effects inside such an operation are never undone.

### FAIL-6: Region prefixes

Bytes that a failed builder append, a failed or unused sized destination, or an abandoned
builder allocation took in a region stay charged to the region until it closes
([MEM-8](memory.md#mem-8-close)). Nothing refunds them earlier.

### FAIL-7: One owner per survivor

After any error or failure, each surviving value has exactly one owner: the error value,
its original slot, or a statement temporary that the current cleanup will drop. The
[transition table](ownership.md#own-26-the-transition-table) names that owner for every
operation; an operation not in the table follows FAIL-3.

### FAIL-8: Exit-edge cleanup

On every exit edge of a scope (block end, `break`, `continue`, `return`, `?`, failure and
cancellation), the views of the exited scopes end first; then owning slots are cleaned,
innermost scope first and each scope's slots in reverse declaration order. A live slot is
cleaned, a maybe slot is cleaned when its flag is set, and a moved slot is not touched.
Cleaning a value runs its finalizer if it has one, then cleans its fields in reverse
declaration order and the elements of a sequence from the highest index down, then frees
and refunds its block. A region opened by an exited block closes after its block's owners
are cleaned.

```hirsel
fn two(d: Dir) -> Result[(), IoError] {
  let a = open_temp(d)?
  let b = open_temp(d)?
  write(b)?                            // on error: b dropped, then a
  Ok(())
}
```

### FAIL-9: Pending results

An exit edge that carries a value (`return v`, `break 'l v`, a block's final expression)
first stages the value as the edge's **pending result**, then cleans the exited scopes. If
a drop on the edge fails, the pending result is dropped at once, the remaining drops of the
exited scopes run, and the edge becomes failure propagation past its original target,
whose continuation never runs ([T6.3](ownership.md#own-26-the-transition-table)).

### FAIL-10: Cause chain

A failure keeps its primary cause. A further failure during cleanup (a finalizer failing,
FAIL-12) appends its cause to the chain in the order it happens, and cleanup continues with
the next value. The chain is delivered whole to the joiner (FAIL-14). Cleaning never
replaces the primary cause, and no cause is dropped. Storage for every cause a task can
record is admitted in advance, with the task and with each resource
([MEM-26](budgets.md#mem-26-logical-charges)), so recording a cause never admits.

### FAIL-11: Resources

A `resource type` declares a finalizer after its fields:

```hirsel
resource type Temp { dir: Dir, path: Path }
  drop(t) { let _ = file.remove(t.dir, t.path) }
```

A resource is cleaned exactly once: by its finalizer on implicit drop, or by an explicit
consuming function. A consuming function in the defining module ends a resource without
its finalizer by a whole destructure ([T3.7](ownership.md#own-26-the-transition-table)),
which is how fallible finalisation (`close`, `commit`, `flush`) is written. There is no
`forget`. A finalizer has a unit result and no `?`; it is shielded, so it is never a
cancellation point; it uses only the capabilities stored in the resource, its brand owner
([MEM-20](memory.md#mem-20-branded-resource-cleanup)) and its grant
([MEM-29](budgets.md#mem-29-finalizer-grants)); it cannot move, copy or return the resource.

```hirsel
pub fn close(take f: File) -> Result[(), IoError] {
  let File(handle: h) = take f          // f ends without its finalizer
  os_close(take h)
}
```
```hirsel
resource type T { f: File }
  drop(t) { file.flush(mut t.f)? }      // refused: FAIL-11, ? in a finalizer
```

### FAIL-12: Finalizer failure

A failure inside a finalizer abandons the finalizer body, still cleans the resource's
fields, and is a failed drop: on a normal exit edge it starts propagation (FAIL-9); during
failure cleanup it appends to the cause chain (FAIL-10). In both cases cleanup continues
with the next value.

### FAIL-13: Mechanical release

Ending views, freeing and refunding own blocks, closing regions, releasing counted nodes
and failure propagation itself never allocate, never admit, never run user code other than
finalizers and never fail.

### FAIL-14: Failure boundary

A failure ends the task in which it happens, after the task's cleanup. Its joiner receives
`Failed(causes)` as an error value of `join` ([CONC-6](tasks.md#conc-6-join)). A failure in
`main` ends the run with the failure as its outcome. No construct inside a task catches a
failure.

### FAIL-15: Run stops

Exhausting the stack, the engine failing to obtain backing memory for bytes that a limit has
already admitted, and termination by the host are run stops. A run stop ends every task
without cleanup. It is reported as the run's outcome and is not a failure: no cause chain
is formed and no joiner observes it. Charges make the second case impossible when the
host provides the root limit's bytes.
