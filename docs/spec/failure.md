# Failure

### FAIL-1: Errors, failures and run stops

An **error** is an `Error(e)` value; it is handled by ordinary code. A **failure** is the
abandonment of a task's computation with a cause (FAIL-2); it runs cleanup (FAIL-8) and is
observed only by the task's joiner (FAIL-14), by an enclosing `isolate` (FAIL-16) or, for
`OutOfTime`, by the step bound's scope ([MEM-30](budgets.md#mem-30-step-bounds)). A **run stop** ends the whole run without
cleanup (FAIL-15). No construct converts one into another, except that `join` delivers a
child's failure to its parent as an error value, `isolate` delivers a failure in its
body as a value (FAIL-16), and a step bound's scope delivers its own `OutOfTime` as a value.

### FAIL-2: Causes

A failure has a primary cause from this closed list, and a site, the operation that
raised it:

| cause | raised by |
|---|---|
| `Overflow` | checked integer arithmetic, conversion and shift counts ([SEM-1](semantics.md#sem-1-integers)) |
| `Bounds` | indexing and slicing out of range; `channel.new(0)`; `conn.new` with `cap` 0, and a connection send whose item's content charge exceeds the connection's room ([CONC-17](tasks.md#conc-17-connections)) |
| `Division` | integer division or remainder by zero |
| `Unwrap` | [`option.unwrap`](../../core/core/option.hirsel) of `None`, [`result.unwrap`](../../core/core/result.hirsel) of `Error` ([core](core.md#contracts-the-signatures-do-not-state)) |
| `Fail(text)` | `fail(msg)`, which has type `Never`; a foreign call refused by its handle checks, with one of the fixed texts `foreign handle released`, `foreign handle in use`, `foreign handle of another kind` and `foreign pointer already held` ([FFI-6](foreign.md#ffi-6-handles)), or `foreign call needs a host Dir` (the `sqlite` package's slice 2); for contained code, `foreign instance discarded`, `contained loans exceed linear memory`, `contained loan area out of bounds` and `contained trap: X` ([FFI-12](foreign.md#ffi-12-the-contained-abi) to [FFI-14](foreign.md#ffi-14-determinism-traps-and-replay)) |
| `Cancelled` | a cancellation point that observes cancellation ([CONC-7](tasks.md#conc-7-cancellation-points)) |
| `Capacity(c)` | an admission the current limit refuses ([MEM-21](budgets.md#mem-21-admission)), `c` the refused charge ([MEM-22](budgets.md#mem-22-the-capacity-value)) |
| `OutOfTime` | a step past the current step bound ([MEM-30](budgets.md#mem-30-step-bounds)) |

The admissions that return their refusal as a value instead (a bound scope's or an
`isolate`'s entry, a resource literal, a `try_` operation) raise nothing
([MEM-21](budgets.md#mem-21-admission)).

A failure raised inside a core function has as its site the call that reached it from the
nearest non-core caller. When that call is through a function value, the site is the call
through the value, and where the function was named is not recorded: with `fn apply(f:
fn(take Option[u64]) -> u64, take x: Option[u64]) -> u64 { f(take x) }`, `apply(option.unwrap,
None)` fails with `Unwrap` at `apply`'s `f(take x)`.

```hirsel
fn mean(xs: List[i64]) -> i64 { list.sum(xs) / int.narrow(list.len(xs)) }   // Division when xs is empty
```

### FAIL-3: Commit points

Every operation that takes custody of an input has a **commit point**: the step at which
custody passes. It is entry for a call ([T2.3](ownership.md#own-26-the-transition-table)),
publication for a send ([CONC-4](tasks.md#conc-4-channels)) and admission for an allocating
constructor or core operation ([MEM-21](budgets.md#mem-21-admission)). Before it, the
operation consumes nothing: inputs that its error type lists come back in the error, and
every other input stays where it was. After it, the operation may still fail (a replaced
value's finalizer, a callee's own failure), and what it did is not undone: a callee's
effects on its `mut` arguments stand, and so does an installed value.

```hirsel
fn put(mut q: List[HostFile], take f: HostFile) {
  list.push(mut q, take f)              // on refusal: q unchanged, f dropped by the failure's cleanup
}
```

### FAIL-4: Constructor temporaries

A record or variant construction evaluates its field expressions left to right into
statement temporaries; a resource literal then admits its charge. If a field expression fails, the completed temporaries are dropped by the
statement's cleanup in reverse evaluation order, their finalizers included
([T1.1](ownership.md#own-26-the-transition-table)). If the admission fails, nothing is
cleaned and no finalizer runs: a resource literal hands its fields back in its error, as
`box.new` hands back its value (T1.3)
([T1.2](ownership.md#own-26-the-transition-table)).

```hirsel
type Pair { a: HostFile, b: HostFile }
fn pair(d: Dir) -> Result[Pair, IoError] waits {
  Ok(Pair(a: host.open(d, "a", Read)?, b: host.open(d, "b", Read)?))   // if b fails, a is dropped once
}
```

### FAIL-5: Multi-step operations

An operation that allocates in several steps, such as `clone`, `clone_in` and map
insertion that grows, undoes its own partial results before it raises its failure or
returns its error, as
[T1.5](ownership.md#own-26-the-transition-table) states. A callback's effects inside such an
operation are never undone.

### FAIL-6: Region prefixes

Bytes that a failed `span.push`, a failed or unused sized destination or writer, or an
abandoned span-builder allocation took in a region stay charged to the region until it
closes ([MEM-8](memory.md#mem-8-close)). Nothing refunds them earlier, and nothing reuses
them; only `finish_trim` of a completed builder refunds its unused end
([MEM-4](memory.md#mem-4-sized-destinations)).

### FAIL-7: One owner per survivor

After any error or failure, each surviving value has exactly one owner: the error value,
its original slot, or a statement temporary that the current cleanup will drop. The
[transition table](ownership.md#own-26-the-transition-table) names that owner for every
operation; an operation not in the table follows FAIL-3.

### FAIL-8: Exit-edge cleanup

On every exit edge of a scope (block end, `break`, `continue`, `return`, `?`, failure and
cancellation), the views of the exited scopes end first, and views of scopes not exited
stay; then owning slots are cleaned, innermost scope first and each scope's slots in reverse
declaration order (the slots a pattern binds are declared in pattern order). A live slot is
cleaned, a maybe slot is cleaned when its flag is set, and a moved slot is not touched.
Cleaning a value runs its finalizer if it has one, then cleans its record and tuple fields
in reverse declaration order, the elements of a complete `List` or `Inline` array from
index 0 up, a `Map`'s entries as [core](core.md) says and a queue's items as
[CONC-9](tasks.md#conc-9-queue-drop) says, then frees and refunds its block. This order is
fixed wherever it is observable: where cleaning runs a finalizer, ends a resource or makes an
event, such as an `Arc` release or a refund that crosses tasks; elsewhere the as-if rule of
[SEM-8](semantics.md#sem-8-determinism) applies. A construction
that fails cleans its completed temporaries in reverse evaluation order
([T1.1](ownership.md#own-26-the-transition-table)). A region opened by an exited block closes after its block's owners
are cleaned.

```hirsel
fn two(d: Dir) -> Result[(), IoError] waits {
  let a = host.open(d, "a", Read)?
  let mut b = host.open(d, "b", Append)?
  host.try_lock(mut b)?                // on error: b dropped, then a
  Ok(())
}
```

### FAIL-9: Pending results

An exit edge that carries a value (`return v`, `break 'l v`, a block's final expression)
first stages the value as the edge's **pending result**, then cleans the exited scopes. If
a drop on the edge fails, the failing value's fields are cleaned (FAIL-12), then the
pending result is dropped, then the remaining drops of the exited scopes run, each cause
appended in the order it happens, and the edge becomes failure propagation past its original target,
whose continuation never runs ([T6.3](ownership.md#own-26-the-transition-table)).

### FAIL-10: Cause chain

A failure keeps its primary cause. A further failure during cleanup (a finalizer failing,
FAIL-12) appends its cause to the chain in the order it happens, and cleanup continues with
the next value. The chain is delivered whole to the joiner (FAIL-14). Cleaning never
replaces the primary cause, and no cause is dropped.

A cause is stored in a **cause cell**. Propagating a failure moves the existing chain and
reserves nothing. A cell is reserved only with an obligation that can add a cause: each
task has one primary cell from its start, and each resource value that is not mechanical
([MEM-24](budgets.md#mem-24-logical-charges), FAIL-13) has one, admitted with
the resource ([MEM-24](budgets.md#mem-24-logical-charges)); a resource created by a
finalizer takes its cell and storage from the finalizer's grant
([MEM-27](budgets.md#mem-27-finalizer-grants)). A cause raised by a finalizer goes into its
resource's cell, and any other cause into the task's primary cell. If a finalizer cannot
admit an obligation while a failure is active, its grant being exhausted, that is its own
failure with cause `Capacity(c)`, recorded in its resource's cell, and cleanup continues; an
`isolate`'s refused cell is a value instead ([FAIL-16](#fail-16-isolation)). So
code that makes no resources performs no cause admission beyond task startup, the chain has no depth limit and
is never truncated, and recording, linking and delivering causes never admit. An unused
cell is refunded with its obligation; a used cell stays with the chain, through `join`, until
the `Causes` value that holds it is cleaned, and is then refunded to the limit that admitted
it. A `Fail` cause takes ownership of its message.

### FAIL-11: Resources

A `resource type` declares a finalizer after its fields:

```hirsel
resource type Temp { dir: Dir, name: String }
  drop(t) { let _ = host.remove(t.dir, t.name) }
```

A resource literal admits the resource's storage, cause cell and grant
([MEM-24](budgets.md#mem-24-logical-charges)) and has type `Result[T, (Capacity, F)]`, with
`F` its one field or the tuple of its fields in declaration order, or `Result[T, Capacity]`
when it has no fields ([T1.2](ownership.md#own-26-the-transition-table)). A
resource is cleaned exactly once: by its finalizer on implicit drop, or by an explicit
consuming function. A consuming function in the defining module ends a resource without
its finalizer by a whole destructure ([T3.7](ownership.md#own-26-the-transition-table)),
which is how fallible finalisation (`close`, `commit`, `flush`) is written; after its fields
are transferred the destructure frees and refunds the resource's whole block (its field
storage, unused cause cell and unused grant), and the blocks its fields own stay charged
with them. There is no `forget`. A finalizer has a unit result and no `?` of its own; the
functions it calls may use `?` inside them. It is a shield: it observes no cancellation
([CONC-7](tasks.md#conc-7-cancellation-points)), though it may call `waits` operations, and
it may spawn from its grant. It uses only the capabilities stored in the resource and its grant
([MEM-27](budgets.md#mem-27-finalizer-grants)); it cannot move, copy or return the
resource.

```hirsel
resource type File { handle: HostFile }
  drop(f) { let _ = host.unlock(mut f.handle) }
pub fn open(take h: HostFile) -> Result[File, (Capacity, HostFile)] { File(handle: take h) }
pub fn close(take f: File) -> Result[(), IoError] waits {
  let File(handle: h) = take f          // f ends without its finalizer
  host.close(take h)
}
```
```hirsel
resource type T { f: HostFile }
  drop(t) { host.unlock(mut t.f)? }     // refused: FAIL-11, ? in a finalizer
```

### FAIL-12: Finalizer failure

A failure inside a finalizer abandons the finalizer body, still cleans the resource's
fields, and is a failed drop: on a normal exit edge it starts propagation (FAIL-9); during
failure cleanup it appends to the cause chain (FAIL-10). In both cases cleanup continues
with the next value.

### FAIL-13: Mechanical release

Ending views, freeing and refunding own blocks, closing regions, releasing counted nodes
and failure propagation itself never allocate, never admit, never run user code other than
finalizers and never fail. Dropping a `HostFile`, `HostEntries`, `Socket`, half or `Listener`
made by a virtual provider ([core](core.md#provide)) is one of these: the release notice is
reserved inside the handle when the server issues it, dropping links that notice into its
port without waiting, and the drop succeeds when the server has ended or its room is full.
Its effects are visible at once (a dropped half reads end of stream at its peer, a dropped
lock holder unlocks), and the server sees `Released` from `provide.next` later.

### FAIL-14: Failure boundary

A failure that no `isolate` catches (FAIL-16) ends the task in which it happens, after the task's cleanup. Its joiner
receives `Failed(causes)`, or `Cancelled(causes)` when the primary cause is `Cancelled`, as
an error value of `join` ([CONC-6](tasks.md#conc-6-join)). A failure in `main` ends the run
with the failure as its outcome. No construct other than `isolate` catches a failure,
except that a step bound's scope catches its own `OutOfTime` ([MEM-30](budgets.md#mem-30-step-bounds)).

### FAIL-15: Run stops

Exhausting the stack, a counter that would pass its 64-bit range (a counted value's holders,
[MEM-16](memory.md#mem-16-counting); a channel's senders, [CONC-4](tasks.md#conc-4-channels); a `Latest` root's handles,
[CONC-13](tasks.md#conc-13-latest);
one the engine keeps itself), the host refusing memory or a service the engine needs, a host
provider that breaks the provider ABI, and termination by the host are run stops. A native
callee that aborts, overflows its stack or corrupts memory is termination by the host
([FFI-1](foreign.md#ffi-1-hirsel-trusts-audited-native-code-and-does-not-contain-it)). A run stop ends every task without cleanup. It is
reported as the run's outcome and is not a failure: no cause chain is formed, no cleanup or
finalizer runs, and no joiner observes it. In a process it exits with status 102 and
writes one line to stderr, `run stop: ` and the stop's name
([PROG-3](programs.md#prog-3-entry)), except termination by the host, whose status and output
are the host's, PROG-3's exception, a native abort's included. An embedded runtime
([EMB-3](embed.md#emb-3-embedded-launch)) never exits and writes no stderr line: a run stop
ends its runtime generation, and the stop's name is reported to the embedder:

| stop | name |
|---|---|
| the stack is exhausted | `Stack` |
| every task waits on another task (a join, a channel send or receive, an abandon, or an operation on a virtual provider whose call its server has not answered), no timer is pending and no provider operation or issued foreign `waits` call ([FFI-7](foreign.md#ffi-7-host-stack-and-waits)) is outstanding | `Deadlock` |
| a counted value's holders would pass `2^64 − 1` | `CountOverflow` |
| a channel's senders would pass `2^64 − 1` | `ChannelSenders` |
| a `Latest` root's handles would pass `2^64 − 1` | `LatestHandles` |
| an engine counter would pass `2^64 − 1`: task ids, host handles and provider operation tokens, temporary names, runs in the process, destination identities ([MEM-4](memory.md#mem-4-sized-destinations)) | `TaskIds`, `HostHandles`, `HostNames`, `ActiveRuns`, `DestinationIds` |
| the host refuses memory the engine needs | `HostMemory` |
| the host refuses the signal stack, a signal action, the clock or the page size | `SignalStack`, `Signal`, `Clock`, `PageSize` |
| a host provider lacks one of its five callbacks when the run installs it, before `main`, or answers a `wait` or step kind outside the provider ABI | `Provider` |
| a unit's spawn, result or connection send attempts to transfer a socket half, a TLS socket, or a socket or listener owned by a virtual `Net` provider ([CONC-15](tasks.md#conc-15-units), [CONC-17](tasks.md#conc-17-connections)) | `UnitSocket` |

A flow operation ends at its closure ([CONC-14](tasks.md#conc-14-flows)). The `Deadlock` rule does not count its owner as waiting.
An operation on a virtual provider ([core](core.md#provide)) waits on the task that serves
its port, not on the host, so it is never an outstanding provider operation: a task that
calls its own server, or two that wait on each other through ports, stop with `Deadlock`.
A timer of a virtual `Clock` is pending only while some task waits in `clock.idle` on its
hand. Before the run stops with `Deadlock`, each task waiting in `clock.idle` is woken with
the earliest pending deadline of its clock, and if none of them advances time, each server
waiting in `provide.next` with no queued call is woken with `None`.
These wakes are the run's, across all its units ([CONC-15](tasks.md#conc-15-units)): they
happen once every task in every unit waits on something inside the run, and a wait on a
connection between its units counts as such. A run in which some task waits on a
connection does not stop with `Deadlock`.

Termination by the host writes nothing and its status is the host's.

### FAIL-16: Isolation

`isolate(body)`, with `body: scoped fn() -> R`, runs `body` once and returns a failure in
it as a value, `Result[Result[R, Causes], Capacity]`, instead of ending the task. It is a
recovery boundary and undoes nothing:

- It first reserves a new cause cell, charged as a task's
  ([MEM-24](budgets.md#mem-24-logical-charges)) and admitted from the current limit, which
  inside a finalizer is its grant ([MEM-27](budgets.md#mem-27-finalizer-grants)). The cell
  stands in for the **current obligation**'s cell ([FAIL-10](#fail-10-cause-chain)): the
  cell of the resource whose finalizer is running, the innermost one, or else the task's
  primary cell. When that admission fails, under a grant too, `isolate` returns the value
  `Error(c)`, `c` the refused `Capacity`, and never fails; it does not call `body`, and the
  chain is as it was. Otherwise it sets aside the chain active at entry, a failure being
  cleaned when a finalizer runs on a failure path, and calls `body` with the activation of
  a call that receives it ([OWN-21](ownership.md#own-21-scoped-callbacks)).
- If `body` returns `r`, the reserved cell is refunded, the chain set aside is restored, and
  `isolate` returns `Ok(Ok(r))`.
- If `body` fails, the failure is cleaned as on every exit edge (FAIL-8) up to `isolate`:
  the owners, loans, captures and pending results of `body` are cleaned, and every cause
  that cleanup adds joins the chain (FAIL-10). `isolate` then returns `Ok(Error(causes))`
  holding every cause raised since its entry (read by `for`, [core](core.md)), and the chain set aside is restored as it
  was. When the caught failure used the current obligation's cell, the reserved cell takes
  its place, so the code after `isolate` has an unused cell to fail into; when it did not,
  every caught cause having gone into resources' cells, the reserved cell is refunded. A
  failure is caught whatever its primary cause other than `Cancelled`: `Overflow`,
  `Bounds`, `Division`, `Unwrap`, `Fail`, `Capacity`
  ([MEM-21](budgets.md#mem-21-admission)), and `OutOfTime` at an `isolate` where the
  step bound that ran out is no longer current.
- A failure whose primary cause is `Cancelled` is not caught, and neither is one whose
  primary cause is `OutOfTime` at an `isolate` where the step bound that ran out is still
  current, inside that bound's scope or in a task spawned under it
  ([MEM-30](budgets.md#mem-30-step-bounds)): after the
  same cleanup the reserved cell is refunded, the chain set aside is restored with the failure's causes
  after it, and the failure goes on past `isolate`. A run stop (FAIL-15)
  is not caught, and nothing resumes at the operation that failed.

`body`'s type is not `waits`, so nothing in it waits ([LANG-8](language.md#lang-8-signatures)),
and it takes no parameter, so it has no `mut` or `take` parameter; a callable that does
not fit that type is refused by [LANG-13](language.md#lang-13-callables). `R` is a type
parameter and therefore never scoped ([OWN-19](ownership.md#own-19-scoped-values)).

`isolate` is refused here, at its argument, when the argument's capture descriptor
(OWN-21) has a `mut` entry. The descriptor checked is the transitive one: it unites the
descriptors of the scoped callbacks the body invokes or passes on, with `mut` dominating
view, and a scoped callback that the enclosing function received as a parameter counts as
a `mut` entry, because its descriptor is its caller's and is not known in this body, unless
its type is `scoped[view] fn`, whose entries are views ([LANG-13](language.md#lang-13-callables)). A
capture the body lends `mut`, a `Box` payload reached through it included, is a `mut`
entry (LANG-14). An `own fn` or `fn` value lends nothing outside its environment, which
its calls only view ([LANG-14](language.md#lang-14-captures)), so the body may call one.

So if `body` fails, every place outside it that it reached through a view holds the value
that was installed in it at entry. Effects that do not install into such a place persist,
and are not undone. They include appends to a log and changes to a cell
([MEM-29](memory.md#mem-29-append-only-logs-and-task-local-cells)), sends on a channel and the queue changes
they make ([CONC-4](tasks.md#conc-4-channels)), provider operations already issued
([CAP-5](capabilities.md#cap-5-provider-operations)), counted holders' counts
([MEM-16](memory.md#mem-16-counting)) and bytes allocated in an outer region, which stay
charged until it closes (FAIL-6). A program that needs all or nothing keeps its working
state in owned values made inside `body`, returns them on success and installs them after
`isolate` returns. A single-`Result` form and a form that moves owned state into the body
and back are reserved ([reserved](reserved.md), F).

```hirsel
fn run(db: Map[u64, u64], step: own fn(Map[u64, u64], mut Map[u64, u64])) -> Result[Result[Map[u64, u64], Causes], Capacity] {
  isolate(fn() -> Map[u64, u64] {
    let mut writes: Map[u64, u64] = map.new()
    step(db, mut writes)                          // a failure here cleans writes; db is unchanged
    writes
  })
}
```
```hirsel
fn copy_or_empty(s: String) -> String {
  match isolate(fn() -> String { string.from(s) }) {   // a refused copy is caught here
    Ok(Ok(t)) => t,
    _ => string.new(),
  }
}
```
```hirsel
fn report(tx: Sender[u64], xs: List[u64]) -> u64 {
  match isolate(fn() -> u64 {
    let _ = channel.try_send(tx, 1)               // stays sent when the sum fails
    list.sum(xs)
  }) {
    Ok(Ok(s)) => s,
    Ok(Error(_)) => 0,
    Error(_) => 0,
  }
}
```
```hirsel
fn bump(mut n: u64) -> u64 {
  let _ = isolate(fn() -> u64 { n = n + 1; n })   // refused: FAIL-16, n is captured mut
  n
}
```
```hirsel
fn count(xs: List[u64]) -> u64 {
  let mut seen: u64 = 0
  let tick = scoped fn() { seen = seen + 1 }
  let _ = isolate(fn() -> u64 { tick(); list.len(xs) })   // refused: FAIL-16, tick lends seen mut
  seen
}
```
```hirsel
fn guard(f: scoped fn() -> u64) -> u64 {
  match isolate(fn() -> u64 { f() }) {            // refused: FAIL-16, f's descriptor is not known here
    Ok(Ok(v)) => v,
    _ => 0,
  }
}
```

## Proposal: stack exhaustion is a failure (FAIL-17, awaiting ratification)

**Status: proposal, not ratified, not built.** Nothing here applies until it is ratified.
Until then FAIL-2, FAIL-15 and FAIL-16 above are the rule, and exhausting the stack is the
run stop `Stack`.

**Why.** A run stop ends every task in the run. Recursion depth, the size of a nested value
and the frames a call chain needs are chosen by input as often as loop counts and sizes
are. A step bound already turns runaway steps into a failure (MEM-30), and a limit turns
runaway allocation into one (MEM-21). Stack exhaustion is the one input-chosen resource
whose exhaustion still ends the whole run. That includes a host that runs untrusted code
under `bound bytes`, `bound steps` and `isolate` and expects a failure to stay inside them.
On the main branch at this proposal, each of these ends the run with status 102 and
`run stop: Stack` on Cranelift and LLVM:
- recursion with a 4 KiB frame inside `isolate`, `bound bytes` or `bound steps`;
- a chain of 50 non-recursive calls, each with a 256 KiB `Inline` local, inside
  `isolate`, in `main` and in a spawned task;
- `clone` of a 1,000,000-node `Box` chain inside `isolate`.

On the evaluator, the first case stops the run too, or, when the host thread's stack is
smaller than the evaluator assumes, aborts the host process.

### Proposed FAIL-17: Stack exhaustion

Each task runs on its own stack. The part of that stack that user code may use is the task's
**stack allowance**. It is the same for every task of one build, and is fixed by the build.
It is not a limit (MEM-19): nothing admits, refunds or hands it over, and nothing names it.
Below the allowance, each stack keeps an **engine reserve** for the engine code that runs
between activations: cleanup, refunds, cause recording, and core functions whose stack use
the input does not choose.

- **The refusal.** Every entry of user code needs room for its activation: its whole frame,
  however large, and the frames of any callees the build checks with it (Determinism,
  below). The entries are a call of a function or callable value, a finalizer when cleanup
  runs it, and a callable that a core function calls back (MEM-30's list). If the room is
  not left in the allowance, the entry is refused. Nothing of the activation is made and its
  body does not run. A failure with primary cause `Stack` is raised at the call, after its
  arguments are evaluated, where MEM-30 takes the call's step. For an entry made without a
  call, it is raised at the operation that made it.
  - A refused call is refused before its commit point (FAIL-3). The callee takes nothing, so
    its `take` arguments are dropped with the caller's statement temporaries (FAIL-8).
  - A refused finalizer is a finalizer failure (FAIL-12). Its resource's fields are still
    cleaned mechanically, the cause goes into the resource's cell (FAIL-10), and cleanup
    continues.
- **The cause.** `Stack` joins FAIL-2's closed list as a payload-free case of core's
  `CauseKind`, raised by "an entry of user code that the stack allowance refuses
  (FAIL-17)". Raising it admits nothing and uses a cell that is already reserved
  (FAIL-10). Its PROG-3 line is `Stack at SITE`.
- **Who catches it.** It is an ordinary failure. FAIL-8's cleanup runs on every exit edge
  it takes. `isolate` catches it like any other cause, so FAIL-16's list gains `Stack`. A
  bound scope does not catch it, as a `bound bytes` scope does not catch `Capacity`
  (MEM-25). If nothing catches it, it ends its task (FAIL-14) and the joiner receives
  `Failed(causes)`. In `main` it ends the run with status 101. No other task is touched.
- **Cleanup near the edge.** Cleanup after a `Stack` failure starts at the deepest frame.
  Mechanical release (FAIL-13) runs in the engine reserve and always fits. A finalizer
  whose activation does not fit is refused as above, as a finalizer under an exhausted step
  bound is (MEM-30). So cleanup always ends, and each finalizer that could not run adds one
  `Stack` cause to the chain.
- **Generated glue uses constant stack.** Code an engine generates for a type uses an
  amount of stack set by the type alone, never by the value it visits. That covers drop,
  `clone`, `clone_in`, equality, ordering and hashing. So glue never raises `Stack`, and
  it works on a recursive type of any depth: a `Box` chain, a tree, a nested `List`. Work
  that grows with the value's depth goes in the value itself, or in an engine worklist.
  - Drop and `clone` need no worklist: a drop walks and frees the value, and a `clone`
    links each copied node through the copy it is building. Drop glue already works this
    way: it drops a 1,000,000-node chain at constant stack.
  - Equality, ordering and hashing walk with a worklist of at most one word for each level
    of nesting below the current node. The nodes on that path are already charged, so the
    worklist is engine bookkeeping like drop flags (MEM-19). The host refusing it is the
    run stop `HostMemory`.
  - Mechanical release still never fails (FAIL-13), and glue that admits still fails only
    with `Capacity` (MEM-21).
- **Core functions.** A core function whose stack depth its input chooses checks the
  allowance the same way, and raises `Stack` at the call that reached it. Examples are a
  recursive parser, a pattern compiler, and a decoder of nested values. Every other core
  function uses a bounded amount of stack, which the engine reserve covers.
- **Determinism.** Where an entry is refused depends on the build's frame sizes. Every run
  of one build with the same inputs is refused at the same entry, so a `Stack` failure
  passes the replay point like any other failure ([SEM-8](semantics.md#sem-8-determinism)).
  Across builds the point differs, as for limits (MEM-28) and step bounds (MEM-30). An
  engine or optimisation level may use larger or smaller frames, or inline calls, so no
  program or test may rely on a depth across engines. A build may check at one entry for a
  whole subtree of calls whose stack need is bounded and known, and leave the entries
  inside that subtree unchecked. The subtree's root is then refused earlier than strictly
  needed, but at the same point on every run of that build.
- **What stays a run stop.** FAIL-15's `Stack` row becomes "the engine reserve is
  exhausted (`Stack`)". FAIL-17 keeps every activation of user code, all generated glue and
  every input-chosen core recursion inside the allowance, so this row covers only engine
  code past the reserve. The other rows of FAIL-15 are unchanged:
  - `HostMemory`, when the host refuses stack growth;
  - native foreign frames, which run on a separate host stack, where overflow is
    termination by the host (FFI-1);
  - the embedded rules of [EMB-3](embed.md#emb-3-embedded-launch).

  Contained foreign code already fails, with `contained trap: stack overflow`, when its
  guest stack runs out ([FFI-14](foreign.md#ffi-14-determinism-traps-and-replay)). FAIL-17
  makes Hirsel's own code behave the same way.
- **No per-scope stack bound.** `bound bytes` does not charge the stack (MEM-19), and no
  scope narrows the allowance. A stack figure is one engine's frame size, so a bound
  written in one would mean something different on each build. A portable depth bound,
  counted in activations as MEM-34 counts them for constants, is reserved.

```hirsel unbuilt
fn depth(n: u64) -> u64 { if n == 0 { 0 } else { depth(n - 1) + 1 } }

// Run depth(n) in an isolate. If this build refuses it for stack, retry once at half
// the depth, and give up with 0 if that is refused too.
fn depth_or_half(n: u64) -> u64 {
  match isolate(fn() -> u64 { depth(n) }) {
    Ok(Ok(d)) => d,
    Ok(Error(first)) => {
      for a in first {
        match a.kind {
          Stack => match isolate(fn() -> u64 { depth(n / 2) }) {
            Ok(Ok(d)) => return d,
            Ok(Error(second)) => {
              for b in second { match b.kind { Stack => return 0, _ => {} } }
            },
            Error(_) => {},
          },
          _ => {},
        }
      }
      0
    },
    Error(_) => 0,
  }
}
```

### Laws a build must add

Each law is red on the main branch at this proposal, because the run stops with status 102.
No law compares a depth across engines.

| law | real bug it catches | check (oracle) | harness |
|---|---|---|---|
| **S1** `isolate` catches deep recursion | exhaustion still ends the run, or `isolate` leaves `Stack` out of its catch list | Recursion with a 4 KiB frame and no base case, inside `isolate`, inside `bound bytes` and inside `bound steps`, returns `Ok(Error(causes))`, primary cause `Stack`, site the recursive call. The bound scopes pass it on to the `isolate`. The oracle is FAIL-17 and FAIL-16. | corpus case, `--backend all` |
| **S2** large frames without recursion, in `main` and in a fiber | a check that covers only recursion, or only the main stack, so large frames still reach the guard page | 50 non-recursive functions, each with a dynamically indexed 256 KiB `Inline` local, each calling the next, under `bound bytes { bound steps { isolate(..) } }`. In `main`, and in a task spawned with `task.spawn` and joined, the `isolate` returns `Ok(Error(causes))`, primary cause `Stack`. The run goes on and exits 0. A sibling task that computes a known value returns it. | `test_run_native.rs`, beside `prog4_runs`, Cranelift and LLVM |
| **S3** cleanup at the edge ends and loses nothing | a finalizer at the deepest frame faults into the guard page, cleanup is skipped, or a `take` argument of the refused call is leaked or dropped twice | Each level creates a resource whose finalizer appends one entry to a log, and passes a second resource `take` to the call that is refused. After `isolate` returns, created = logged + refused, where refused is the number of causes after the primary. Every cause is `Stack`. Creation is counted in a cell the body cannot roll back. | corpus case, `--backend all` |
| **S4** glue on deep values | recursive `clone`, `==` or hash glue that runs out of stack on a deep value | A 1,000,000-node `Box` chain: `clone` inside `isolate` returns `Ok(Ok(_))`, `==` of the chain and its clone is `true`, and dropping both succeeds, under a limit that holds both chains. The oracle is FAIL-17's glue rule: no glue depends on depth. | corpus case, `--backend all` |
| **S5** replay | a refusal point that depends on the run, not the build | `hirsel test --replay` of a test that catches S1's `Stack` beside a task that prints a line, on a 2-thread scheduler, ends `Sealed` (FW27's shape). | `hirsel test --replay` |
| **S6** `main`'s outcome | the stop name is still written, or the status is 102 | The existing `stopped` row of `prog4_runs` (`deep()`) expects status 101 and a first stderr line beginning `Stack at`. | change the existing row; no new test |

### Build slices

| slice | contents | laws |
|---|---|---|
| **K1** cause and evaluator | `Stack` in `CauseKind`, in the PROG-3 cause line and in `isolate`'s catch list. The evaluator's activation check raises the failure `Stack` where it now stops the run, before the activation is made, and keeps its host-stack reserve for cleanup. | S1, S3, S6 on the evaluator |
| **K2** native check | Add a stack-limit word per task to the native Context, set when the fiber is made to the guard end plus the engine reserve. Add an IR `stack_check` instruction, fallible like `step`, at the call. It compares the stack pointer, less the callee's need, with the word, and branches cold to the failure successor. A callee's need is its frame plus the need of the unchecked calls it makes. The emitter publishes it as a link-time constant per function, and a function value carries its own. Calls are checked where they enter a recursive component, where they are indirect, and where a subtree's need exceeds a fixed threshold. A call inside a checked subtree is not checked. The runtime checks `fiber::room` before it enters a finalizer, task entry or core callback. Then frames never reach the guard page, and the guard stays as the backstop for engine code. Cranelift first, then LLVM. | S1 to S3 and S6 on both engines |
| **K3** iterative glue | Rewrite `clone`, `clone_in`, equality, ordering and hashing glue for recursive types to use constant stack, as drop glue already does. | S4 |
| **K4** core recursion | Audit the core functions whose stack depth the input chooses. Each gets `fiber::room` and raises `Stack`. | a case per function in its existing harness |
| **K5** replay and fold-in | S5. Then move this text into FAIL-2, FAIL-15 and FAIL-16 and [reserved](reserved.md) R5 and R8, update the LANG-19 recursion case and the `Stack` exceptions in `crates/hirsel-core-program-tests/src/cases/engines.rs`, and remove the proposal mark. | S5 |

Reversible: until K1 lands, this section is spec text only. After it, the cause and the
catch list are core and spec text. Where the check sits and what the glue does are private
to the engines.
