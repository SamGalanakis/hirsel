# Tasks

### CONC-1: Tasks

A **task** is a sequential computation with its own stack, limit and regions. Tasks are
created only through a `Tasks` capability ([CAP-1](capabilities.md#cap-1-authority-is-explicit)).
A task's handle is a resource that is not `send`, so it stays in the task that spawned it,
and a task ends only after cleaning its handles (CONC-8): no task outlives its spawner.
A value passes from one task to another only by moving through a spawn, a channel, a
connection ([CONC-17](#conc-17-connections)) or a join. A flow stage ([CONC-14](#conc-14-flows)) receives and sends plain values through its flow's ports. The only program memory two tasks can both read is the immutable payload of an `Arc`
(CONC-11). Whether tasks and stages run in parallel is the engine's choice
([SEM-8](semantics.md#sem-8-determinism)), except in a run that CONC-10 leaves unspecified.

### CONC-2: The send bound

`send` is the bound of values that may move to another task. Plain types are `send`. `Arc[T]`
is `send`. An own or resource type is `send` when it declares the bound
([OWN-4](ownership.md#own-4-bounds)) and each field type is `send`; a capability type is
`send` when its declaration says so ([CAP-2](capabilities.md#cap-2-capabilities-are-opaque)).
A plain `fn` is `send`; an owning closure is `own[send] fn` when every capture is `send`
([LANG-13](language.md#lang-13-callables)). Never `send`: `Rc`, region values, regions,
scoped values, builders and task handles. The check reads declarations and capture lists only.

`cross` is the narrower bound of values that may move to another unit
([CONC-15](#conc-15-units)); declaring `cross` also declares `send`. These are `cross`: the
plain types, except those that hold a raw foreign handle; a tuple or array whose element
types are `cross`; `String`;
`List[T]` when `T` is plain and `cross`; `Client[T]` ([CONC-17](#conc-17-connections));
`Socket` and `Listener`; the capability types; `Args`, `Child`, `Unique`, `Site` and
`StaticFn`; and plain `fn`s. An own or resource record is `cross` when it declares the bound
and each field type is `cross`; a nominal variant type is `cross` only when it is plain.
Nothing else is: not owning closures, `Arc`, `Box`, `Map`, `Causes`, `Sender`, `Receiver`,
`Latest`, `Inbox`, `HostFile`, `ReadHalf` or `WriteHalf`.

```hirsel
type Job: send { id: u64, name: String }
```
```hirsel
type Ask: cross { n: u64, reply: Client[u64] }
```
```hirsel
type Hello: cross { back: Inbox[u64] }                 // refused: CONC-2, Inbox is not cross
```
```hirsel
type Node: shareable { label: String }
type Job: send { id: u64, cache: Rc[Node] }            // refused: CONC-2, Rc is not send
```

### CONC-3: Spawn

`task.spawn(tasks, bytes n, take x, f) -> Task[R]` starts a task
that runs `f(take x)`, where `x: X`, `X` and `R` are `send`, and `f: fn(take X) -> R waits`
is a plain callable, which captures nothing ([LANG-13](language.md#lang-13-callables)).
Everything the child owns arrives as `x`. Spawn admits the task charge
([MEM-24](budgets.md#mem-24-logical-charges)), which includes `n`, against the spawner's
current limit, and the child's root limit is `n` ([MEM-19](budgets.md#mem-19-limits)). A child spawned while a step bound is current draws its
steps from that bound ([MEM-30](budgets.md#mem-30-step-bounds)). Admission is the
commit point; a refused one raises `Capacity` ([MEM-21](budgets.md#mem-21-admission)) and
has not taken `x` ([T7.1](ownership.md#own-26-the-transition-table)).
`task.try_spawn(tasks, bytes n, take x, f) -> Result[Task[R], (Capacity, X)]` is the same
except that a refusal is the value `Error((c, x))`, for a caller that answers it in place,
as a server refuses a connection it cannot run.

```hirsel
fn count(tasks: Tasks, take words: List[String]) -> Task[u64] {
  task.spawn(tasks, bytes 4096, take words, fn(take w: List[String]) -> u64 { list.len(w) })
}
```
```hirsel
type Node: shareable { label: String }
fn count(tasks: Tasks, take words: List[Rc[Node]]) -> Task[u64] {
  task.spawn(tasks, bytes 4096, take words, fn(take w: List[Rc[Node]]) -> u64 { list.len(w) })
}                                                          // refused: CONC-2, Rc is not send
```

### CONC-4: Channels

`channel.new[T: send](cap: u64) -> (Sender[T], Receiver[T])` admits the
channel's whole charge, including `cap` items, at creation; sending never allocates. A
`cap` of 0 is a failure with cause `Bounds`. `Sender[T]` is a `send` resource;
`channel.sender(tx)` adds a sender, admits nothing and cannot fail; a sender count past
`2^64 − 1` is a run stop ([FAIL-15](failure.md#fail-15-run-stops)). `Receiver[T]` is a
`send` resource with no way to add a second receiver. Neither has a cause cell
([MEM-24](budgets.md#mem-24-logical-charges)). When the last sender is cleaned, the
channel is closed for receiving after its queued items; when the receiver is cleaned, the
channel is closed for sending and its queued items are dropped (CONC-9).

### CONC-5: send and receive

`channel.send(tx, take x) -> Result[(), Closed[T]] waits` waits while the queue is full, then
publishes `x`; after publication the queue owns `x`. If the channel is closed the error
owns `x`. `send` is a cancellation point: cancellation observed before publication is a
failure, and the sender's cleanup drops `x` ([T7.2, T7.3](ownership.md#own-26-the-transition-table)).
`channel.try_send(tx, take x) -> Result[(), SendError[T]]` never waits and returns
`Full(x)` or `Closed(x)`; its decision is an event (CONC-10).
`channel.receive(rx) -> Result[T, Closed[()]] waits` waits for an item and is a cancellation
point; it returns `Closed` once the channel is closed and empty.
`channel.try_receive(rx) -> Result[T, ReceiveError]`, where `type ReceiveError = Empty |
Closed`, never waits and is not a cancellation point: it delivers the front item as
`receive` would, and with the queue empty returns `Closed` once the last sender is cleaned
and `Empty` while a sender remains. Its decision is an event (CONC-10), so it finds an item
exactly when one was published before it in the event order and not yet taken, and a
replay decides as recorded. With one receiver, `receive` and `try_receive` take items from the one front in
the receiving task's program order, whichever it calls. The queue is first in,
first out by publication order; positions are logical, so how an engine stores the queue
is its choice ([SEM-8](semantics.md#sem-8-determinism)). The blocks of a sent value stay charged to the limit that admitted them
([MEM-20](budgets.md#mem-20-limit-records-and-hand-over)).

```hirsel
type Job: send { id: u64, name: String }
fn pump(tx: Sender[Job], take jobs: List[Job]) -> Result[(), Closed[Job]] waits {
  for take j in jobs { channel.send(tx, take j)? }
  Ok(())
}
```

### CONC-6: join

`task.join(take h: Task[R]) -> Result[R, JoinError] waits` waits for the child to end, where
`type JoinError = Failed(Causes) | Cancelled(Causes)`, a `Causes` being read by `for`
([core](core.md)). The child
cleans its own state before it ends. `Ok(r)` moves the result to the joiner; `Failed(causes)`
delivers the child's cause chain ([FAIL-10](failure.md#fail-10-cause-chain)) as a value the
joiner owns, made of the child's cells, so the joiner's own primary cell is untouched and
`join` reserves nothing; `Cancelled(causes)` does the same for a child whose primary cause
is `Cancelled`, with any causes its cleanup added. `join` is a
cancellation point: if the joiner observes cancellation while waiting, `h` is dropped by
the joiner's cleanup (CONC-8).

```hirsel
fn total(tasks: Tasks, take a: List[String]) -> Result[u64, JoinError] waits {
  let h = match task.try_spawn(tasks, bytes 4096, take a, fn(take w: List[String]) -> u64 { list.len(w) }) {
    Ok(take h) => h, Error((_, take back)) => return Ok(list.len(back)),
  }
  task.join(take h)
}
```

### CONC-7: Cancellation points

`task.cancel(mut h)` requests cancellation of a child and returns at once. A task observes
a pending request only at a **cancellation point**: the entry of every core `waits`
operation (`send`, `receive`, `join` and the waiting provider operations of
[CAP-5](capabilities.md#cap-5-provider-operations)), the entry of every foreign `waits`
call ([FFI-7](foreign.md#ffi-7-host-stack-and-waits)), each moment such an operation or call
would wait before its issue, and `task.check_cancel()`. A foreign release
([FFI-6](foreign.md#ffi-6-handles)) is the exception: its entry and its waits are not
cancellation points, so a reached release is always issued. An issued foreign call is not
reached by cancellation at all. At an entry, a pending request is observed before anything
else is done. A provider operation that has been issued completes and delivers a result
([CAP-8](capabilities.md#cap-8-the-replay-point)). Cancellation reaches it at issue, and,
while it waits in progress for readiness, through its provider's abort, whose result is the
operation's `Aborted` error; the request stays pending and is observed at the next
cancellation point. An operation that does not wait after issue is reached only at issue,
and in v1 only the socket operations and `stdin.read` wait after issue, joined, unbuilt, by the
Unix socket operations ([NET-13](net.md#net-13-unix-streams), [NET-14](net.md#net-14-unix-datagrams)),
I/O on a child's pipe halves and `exec.wait` ([EXEC-5](exec.md#exec-5-wait-and-signal)).
A deadline `At(c, t)` is held on `c`'s own timers, never by the operation's provider: on a
host `Clock` it passes at host monotonic time `t`, and on a virtual `Clock`
([core](core.md#provide)) only when `clock.advance_to` reaches `t`, never by host time. An
operation on a virtual provider waits after issue until its server replies; its server never
sees the deadline or the `Clock`. Cancellation, the deadline and the server's
`provide.reply` settle the call at one point, and the first wins: if cancellation or the
deadline wins, the caller sees `Aborted` or `TimedOut` as from the host, and the reply
returns `Error(Withdrawn)` with no effect on the caller, so a server commits its own state
only after `reply` returns `Ok(())`.
A user function declared `waits` is not itself a cancellation point. A flow operation ends at its closure within the stages' step bounds and observes no cancellation. A task looping over flow operations observes cancellation only where it reaches an explicit cancellation point. Observing a request is a failure with cause `Cancelled`
([FAIL-2](failure.md#fail-2-causes)), which runs the task's cleanup. Pure code is never
cancelled. A step bound may stop it, and the scheduler may run another task at its slice
ends ([MEM-30](budgets.md#mem-30-step-bounds)); neither is a cancellation point. A **shield**, a finalizer
([FAIL-11](failure.md#fail-11-resources)) or the wait of CONC-8, holds a cancellation: inside
it no cancellation point observes a request and no operation it waits in, issued or not, is
ended by one; a request made before or during it stays pending and takes effect at the
first cancellation point after it. A loop that
should be cancellable calls `task.check_cancel()`. A task holds itself at a point that is
not a cancellation point by polling a non-waiting operation such as `try_send`; under the
fairness of CONC-10 the tasks it polls for still run.

```hirsel
fn spin(n: u64) -> u64 { let mut i: u64 = 0; while i < n { task.check_cancel(); i += 1 }; i }
```

### CONC-8: Abandoned handles

Cleaning an unjoined `Task[R]` cancels the child, waits for it to end with cancellation
shielded, then drops its result or its cause chain. The wait, the custody transfer of the
result and its refund are recorded events (CONC-10). Cancellation therefore reaches every
descendant through its spawner's cleanup.

### CONC-9: Queue drop

Items still queued when a channel is dropped are cleaned by the task cleaning the receiver,
last published first; their causes belong to that task. A finalizer failure among them appends its cause, and the remaining items are still
cleaned ([T7.7](ownership.md#own-26-the-transition-table)).

### CONC-10: Event order

A run has one total order of **events**: spawn, task end, cancellation requests and their
observation, every `task.check_cancel()`, publication by `send`, every `try_send` decision, delivery by `receive`, every `try_receive` decision, the `Closed` outcome of a blocking `send` or `receive`, every
change of a channel's sender count, `join`, channel close, every `latest.publish` and
`latest.read` and every change of a `Latest` root's handle count (CONC-13), every slice end ([MEM-30](budgets.md#mem-30-step-bounds)), every refund or hand-over that
crosses tasks ([MEM-20](budgets.md#mem-20-limit-records-and-hand-over)), every `Arc`
creation, share, release, `try_unwrap` and `with_unique` test, and every refused admission
([MEM-21](budgets.md#mem-21-admission)). Each operation observes the state that the order gives.
That list follows one rule: an operation whose result can depend on the order of tasks is
an event, and one whose result cannot, such as a change to a value only its own task
reaches, is not. Stage steps are private. The only owner flow operations in the event order are refused wiring admissions ([CONC-14](#conc-14-flows)). So only events need sequencing when tasks run in parallel, and no
operation observes a state that no prefix of the order gives: no weaker ordering is
visible.
The order is chosen by the scheduler provider at the replay point
([CAP-8](capabilities.md#cap-8-the-replay-point)); with the same inputs and the same
recorded choices, every task behaves identically on every engine.
A wake of a waiting provider operation (its deadline reached, its provider readying it, or a
cancellation request) takes effect at one point of the order, before its task runs again,
and the operation's next step is decided there. A cancellation request ordered before a
readiness wins over it: an operation not yet issued fails `Cancelled`, and one in progress
is ended by its provider (CONC-7).

A **poll** is a non-waiting operation that returns without effect because of another
task's state: a `try_send` that returns `Full`, a `try_receive` that returns `Empty`, a `task.check_cancel()` that finds no
request, a `try_unwrap` that returns the `Arc`, a `with_unique` that returns `None`, and a refused admission;
any later operation of that kind is a poll too. Every event is a point where the scheduler
may run another task, and every scheduler provider is fair: no task performs unboundedly
many polls while another task that is not waiting performs no event, unless that other
task computes forever without an event. A task is waiting only inside a `waits` operation
at a moment it would wait (CONC-7); a polling task is not waiting, so the tasks it polls
for reach their events. Fairness limits which orders the scheduler may choose; a replay
follows the recorded order. A task that runs forever with neither a poll nor a wait waits
for nobody and may keep every other task from running; it is a program error: the run
does not end, and which events other tasks perform meanwhile is not specified.
This is about computing forever; a run whose tasks all wait stops with `Deadlock` (FAIL-15).
A step bound limits only the first: runnable code under it reaches a slice end, where other
tasks run, or exhausts the bound ([MEM-30](budgets.md#mem-30-step-bounds)). Waiting takes no
steps (MEM-30), so tasks under a bound that all wait still stop with `Deadlock`
([FAIL-15](failure.md#fail-15-run-stops)). Each `Full`
below is a poll, so the parent that drains the gate runs while the child polls:

```hirsel
fn wait_gate(tx: Sender[()]) {
  loop {
    match channel.try_send(tx, ()) { Ok(()) => break, Error(Full(_)) => {}, Error(Closed(_)) => fail("gate closed") }
  }
}
```

### CONC-11: Arc across tasks

An `Arc` node stays charged to the limit that created it; its last release, in whichever
task it happens, refunds it there. Counts are exact under the event order: `try_unwrap`
succeeds, and `with_unique` runs its callback, in exactly the runs whose order puts the
test after every other holder's release ([MEM-15](memory.md#mem-15-arc)).

### CONC-12: No shared mutable state

v1 has no mutable value reachable from two tasks, except a `Latest` root (CONC-13), which
holds an `Arc`: no mutable statics, no cells, no mutexes, no atomics, and no write through
the view an `Arc` gives ([OWN-12](ownership.md#own-12-assignment)). `arc.with_unique`
writes an `Arc` payload only while its count is 1, when no other task reaches it
([MEM-15](memory.md#mem-15-arc)). Top-level `const` values are plain and immutable.

```hirsel
const LIMIT: u64 = 64
```
```hirsel
var hits: u64 = 0                                        // refused: LANG-5, no mutable statics
```
```hirsel
type Hits: shareable + send { n: u64 }
fn bump(mut a: Arc[Hits]) { a.n += 1 }                   // refused: OWN-12, not writable
```

### CONC-13: Latest

`Latest[T]`, for `T: shareable + send`, is a handle to a root that holds one `Arc[T]`.
`latest.new(take a: Arc[T]) -> Latest[T]` admits the root, 32
bytes, from the current limit, to which it stays charged
([MEM-20](budgets.md#mem-20-limit-records-and-hand-over)), and returns its first handle;
a refusal raises `Capacity` and has not taken `a`. A handle is a `send` resource, mechanical
([MEM-24](budgets.md#mem-24-logical-charges)) and neither `shareable` nor region data, so no
`Arc` holds one. `latest.handle(h)` adds a handle, admits nothing and cannot fail; a handle
count past `2^64 − 1` is the run stop `LatestHandles`
([FAIL-15](failure.md#fail-15-run-stops)).
`latest.publish(h, take a)` makes `a` the root's `Arc` and releases the share the root
held, and `latest.read(h) -> Arc[T]` returns a new share of the root's `Arc`
([MEM-16](memory.md#mem-16-counting)). Neither waits, fails or admits, and neither is a
cancellation point, so a cancelled task never leaves one half done. Each is an event of its own kind
(CONC-10); the share a `read` makes and the release of the `Arc` a `publish` replaces are
the ordinary `Arc` share and release events ([MEM-15](memory.md#mem-15-arc)), right after
it; and `new`, `handle` and the cleaning of a handle each change the handle count, an event
of the same kind as a change of a channel's sender count. A `read` returns what the last `publish` before it in the event order put in
the root, or what `new` did. No operation hands out a view of the root, so a `publish`
never changes what a reader holds: a version lives while any share of it does, and its
last release, in whichever task, frees it and refunds it to the limit that created it
(CONC-11). Cleaning the last handle releases the root's share and refunds the root; a
task's cleanup cleans the handles it owns, as it cleans every resource.

```hirsel
type Config: shareable + send { routes: List[String] }
fn reload(h: Latest[Config], take c: Config) {
  latest.publish(h, arc.new(take c))
}
fn routes(h: Latest[Config]) -> u64 { let c = latest.read(h); list.len(c.routes) }
```

### CONC-14: Flows


A **flow** is a resource, held by the task that made it (its **owner**), that runs a fixed graph of
**stages** joined by **ports**. Items, and the state of each stage, are `plain + codec`
([OWN-4](ownership.md#own-4-bounds)), so `inert`: they hold nothing ([OWN-2](ownership.md#own-2-type-summaries)), whatever
their `via`. A type argument that holds something is refused. A port moves an item by copying its plain
representation.

**Wiring.** The owner wires the graph from its end backwards. `flow.sink(mut f)` makes a merge that the
owner reads, and returns it with an `Into[T]` naming that merge. `flow.stage(mut f, to, cap, bytes m,
steps k, x, body)` adds a stage whose one output is a new port of `cap` slots into `to`, and returns the
stage's input merge as an `Into[A]`. `flow.port(mut f, to, cap)` adds a port into `to` that the owner
writes. A port's index in its merge is its creation order there, and stages are numbered from 0 in
creation order. Wiring returns `Err(Foreign)` for a handle of another flow and `Err(Zero)` for `cap = 0`;
a refused charge raises `Capacity` ([MEM-21](budgets.md#mem-21-admission)). The owner's first `push`, `advance`, `end` or
`next` **seals** the flow, and wiring after it returns `Err(Started)`. A handle of another flow in any
other flow operation fails the owner with `Bounds`.

**Charges.** Each port charges `cap × (16 + size(T)) + 64` bytes, and each stage `m + size(X) + 320`,
to the current limit ([MEM-21](budgets.md#mem-21-admission)). Each charge is refunded to the record it
was charged to when the flow is cleaned ([MEM-20](budgets.md#mem-20-limit-records-and-hand-over)). A
total above the largest `u64` is refused as a request of that largest value.

**Bodies.** `body` is a `StaticFn` in the **transform fragment**. Its **reach** is the instance set that
its revision covers ([PROG-9](programs.md#prog-9-code-descriptors)). No instance in the reach is `waits`, no
expression in it has a type that holds a resource or a capability, and no call in it goes through a
callable value (`code.callable`, or a parameter, field or local of callable type) other than a closure
written inside the reach. `flow.stage` refuses any other body, and a body whose reach PROG-9 cannot
close. A stage is not a task ([CONC-1](#conc-1-tasks)): it has no handle, and
its `task.check_cancel()` finds no request.

**Ports.** A port holds, in order, items `(key, x)`, frontier marks and at most one terminal, with
non-decreasing keys; its **bound** is the key of its last entry, or 0. `push(p, key, x)` below the
bound returns `Order(x)`. `advance(p, k)` above the bound appends a mark at `k`. The operation `end(p)`, or a failure
of its writer, appends the terminal `End` at the bound or `Dropped(from, at)` at `at`. A port is
**ended** once it has a terminal. Pushing to it returns `Ended(x)`. An advance or repeated end leaves it ended. A
port of `cap` slots gives its `i`th item the slot that the reader's take of item `i − cap` from that
port frees. A reader that stops after taking `t` items from a port, the failing or refused one counted
on its own port, is **gone** for that port, and the port's `i`th push then returns `Closed(x)` exactly
when `i > t + cap`, with no free slot needed. A push reports `Ended`, then
`Order`, then `Closed`.

**Merges.** A port's **lower bound** is the key of its first entry, or its bound if it is empty. A merge
delivers the first entry of port `i`, at key `k`, when every other port `j` that has not given its
terminal has a lower bound above `k` for `j < i`, or at least `k` for `j > i`. A `Dropped` terminal is
delivered once, in place, and the merge gives `End` once every port has given its terminal. The owner's
`next` skips marks and returns `End` every time after the first `End`.

**Stage steps.** A **step** of a stage takes its merge's next deliverable entry and completes it. An
item runs `body(take x, key, a)` under a fresh `bound steps k` within a copy of the owner's remaining
steps made at `flow.stage`, which refuses entry when fewer than `k` remain, and with temporaries charged
to the stage's own limit of `m` bytes. It returns the next `x` and an `Option`: `Some(b)` is pushed at
the same key, and `None` appends a mark there. A mark is passed on as a mark. The stage's **position** is
the key of the last entry it completed. When its merge gives `End`, the stage ends its port. When an item
fails, its merge gives `Dropped`, or its own push gives `Closed`, the stage stops. A failing item at key
`k` ends its port with `Dropped(i, k)`, where `i` is the port's index, and a delivered `Dropped(from, at)`
ends it with `Dropped(i, at)`. Every position below `at` is complete. Only the first
counts as the stage failing, with `halt` `OutOfTime` (`k` or the copy ran out) or `Failed`. An
admission the stage's limit refuses fails the item with `Capacity`, so its `halt` is `Failed`.

**The fixed point.** Steps of different stages commute, except that a mark may replace an unread mark,
which no answer reads. So taking steps until none is possible reaches, up to marks,
one state, the **closure**. Every owner operation acts on the closure of the state its earlier
operations left: `push` gives `Ok`, or `Stalled(x)` when the closure has no free slot. `next` gives the
sink's next item, `Dropped` or `End`, or `Stalled` when the closure has none. `flow.finish(take f)` ends
the owner's live ports, takes the closure discarding every entry that reaches a sink, refunds every
charge, and returns `Ok(())` or `Err(Failure { stage, key, halt })` for the lowest-numbered stage that
failed, with the failing item's key. Cleaning a flow without `finish` may skip remaining steps. It refunds every charge and has no result.

Every answer depends only on the owner's operations and is identical at every degree of parallelism.
A refused wiring admission is the flow's only event ([CONC-10](#conc-10-event-order)).
Flow operations end within bounded steps and observe no cancellation ([CONC-7](#conc-7-cancellation-points)).
An owner that loops over flow operations checks for cancellation at an explicit cancellation point.

```hirsel
fn double(take seen: u64, key: u64, x: u64) -> (u64, Option[u64]) {
  (seen + 1, Some(x * 2))
}
fn run() -> Result[u64, Wire] waits {
  let mut f = flow.new();
  let (s, out): (Sink[u64], Into[u64]) = flow.sink(mut f)?;
  let into = flow.stage(mut f, out, 4, bytes 256, steps 1000, 0, code.describe(double))?;
  let p = flow.port(mut f, into, 4)?;
  flow.push(mut f, p, 1, 20); flow.push(mut f, p, 2, 1); flow.end(mut f, p);
  let mut sum: u64 = 0;
  loop {
    match flow.next(mut f, s) {
      Item(key: _, from: _, x: x) => { sum += x },
      _ => break,
    }
  }
  match flow.finish(take f) { _ => () }
  Ok(sum)                                                  // 42, at every worker count
}
```

### CONC-15: Units

A **unit** is a set of tasks that shares no mutable memory with any other unit. The run's
entry task is the root of the first unit, and `task.spawn` and `task.try_spawn` add the child
to the spawner's unit, so a program that never calls `task.spawn_unit` is one unit. A value
reaches another unit only as the `x` of a `spawn_unit`, as a helper's result at its join, or
as an item of a connection ([CONC-17](#conc-17-connections)), and each of these has a
`cross` type ([CONC-2](#conc-2-the-send-bound)). A plain value is copied. A `List` or
`String` crosses as a copy of its content: its old storage is refunded where it was charged,
and the copy is charged where it arrives. A client, socket, listener or `Secret` moves as a handle.
The **content charge** of a value is the sum, over the lists and strings it holds, of
length × element size rounded up to 8.

`task.spawn_unit(tasks, bytes n, take x, f) -> Result[Task[R], (Capacity, X)]`, where
`X` and `R` are `cross` and `f: fn(take X) -> R waits` is a plain callable as in
[CONC-3](#conc-3-spawn), starts a task that runs `f(take x)` as the root of a new unit, a
**helper** owned by the spawner's unit. It first admits the task charge, 256 + size(R) + 32
+ `n` ([MEM-24](budgets.md#mem-24-logical-charges)), against the spawner's current limit,
and then admits `x`'s content charge against `n`. Either refusal returns `Error((c, x))`,
where `c` gives that admission's requested and available bytes, and leaves `x` as it was.
Otherwise `x`'s content is charged to the helper's root limit of `n`. The helper starts
with its own root step bound equal to the spawner's remaining steps at the spawn, if a
bound is current ([MEM-30](budgets.md#mem-30-step-bounds)). An `X` or `R` that is not
`cross` is refused at the call.

The helper's `Task[R]` is a task handle (CONC-1). `task.join` waits for the helper to end and
returns `Ok(r)`, `Failed(causes)` or `Cancelled(causes)` as [CONC-6](#conc-6-join) says, with
the full chain of causes, including each cause's site and payload. A `Fail` message keeps
its storage charge until it is dropped. `task.cancel` asks the helper's root to cancel,
and the root observes the request at a cancellation point
([CONC-7](#conc-7-cancellation-points)). Cleaning an unjoined helper handle cancels the
helper, waits for its end with cancellation shielded, and drops its result or causes. Once
`join` returns or that cleaning ends, all of the task charge has been refunded to the limit
that admitted it, except the charges for the result's content, any delivered causes, and
each connection the helper made that is still open (CONC-17) and each `Secret` designation
it made that is still held ([TLS-5](tls.md#tls-5-secrets)). These charges are refunded
when their owners are cleaned.

A unit's **entry** is its function `f`, its argument `x`, its root limit `n` and its root
step bound or the absence of one; the first unit's entry is `main` with its grants, the run's
root limit and the launch metadata ([launch](launch.md)). A unit's **inputs** are what
reaches it from outside: the items and close of each inbox it holds and the outcome of each
`conn.send` it issues (CONC-17), the end of each helper it owns, a cancellation request for
its root, refunds sent to its limits by other units, and its providers' results. Two units
with the same entry that receive the same inputs in the same order behave the same. The order
in which inputs from different senders reach a unit is free. How many workers run the
units, which worker or thread runs a unit at any moment, and whether a spare thread takes
the place of a worker blocked in a host call are not observable.

Every charged byte has one payer, a limit, at each moment, and each reservation is released
at one point. A limit's spendable balance changes only at an operation of its own unit or
when its unit takes a refund that another unit sent it, and a send to an inbox of the
sender's own unit follows the same rule. A unit's storage is charged to that unit's limits
and refunded to the limit that paid for it ([MEM-20](budgets.md#mem-20-limit-records-and-hand-over));
where an engine places that storage, and whether a waiting task keeps its stack, are not
observable.

This clause fixes no order between the actions of different units, and does not say when an
action in one unit becomes visible in another.

**Unbuilt.** These are not built, and no program or test may rely on them:

- `Shared` payloads and boards, which would let units read one immutable payload by
  reference and publish its latest version to many units; no program can name them.
- A recording of a unit other than the run's first: a recorded run records its first unit
  and not its helpers, so no replay of a helper, and no equality of recordings made at
  different worker counts for a program with helpers, is promised.
- A test configuration that fixes every input of a unit, including how many of the inputs
  that have reached it it takes at each point; until it exists, a test of units asserts only
  what holds for every such count.

```hirsel
type Table: cross { rows: List[u64] }
fn grow(tasks: Tasks, take t: Table) -> Result[Task[Table], (Capacity, Table)] {
  task.spawn_unit(tasks, bytes 4096, take t, fn(take v: Table) -> Table {
    let mut u = take v
    list.push(mut u.rows, 1)
    take u
  })
}
```
```hirsel
type Config: shareable + send { routes: List[String] }
fn watch(tasks: Tasks, take a: Arc[Config]) -> Result[Task[u64], (Capacity, Arc[Config])] {
  task.spawn_unit(tasks, bytes 4096, take a, fn(take c: Arc[Config]) -> u64 { list.len(c.routes) })
}                                                          // refused: CONC-15, Arc is not cross
```

### CONC-17: Connections

A **connection** carries items of a `cross` type `T` from any unit to the one unit that holds
its inbox. `Client[T]` and `Inbox[T]` are mechanical resources
([MEM-24](budgets.md#mem-24-logical-charges)). A client is `cross`; an inbox is `send` but not
`cross`, so it stays in the unit that made it, and nothing makes a second inbox.

`conn.new(cap: u64, bytes b: u64) -> Result[(Client[T], Inbox[T]), Capacity]` admits
64 + `cap` × size(T) + `b` from the current limit and returns the connection's first client
and its inbox. A `cap` of 0 is a failure with cause `Bounds`. A `cap` above `2^22 − 1` or
`b` above `2^40 − 1` returns `Capacity` with the computed charge (saturated at `2^64 − 1`)
as `requested` and 0 as `available`. The connection holds at most `cap` items, and its
**room** of `b` bytes holds their content
([CONC-15](#conc-15-units)). `conn.client(c)` returns another client of the same connection
and admits nothing.

`conn.send(c, take x) -> Result[(), Closed[T]] waits` fails with cause `Bounds` when `x`'s
content charge exceeds `b`. It answers `Closed(x)` once the inbox has been cleaned. The
connection accepts `x` only while fewer than `cap` items are queued and the room's free bytes
hold `x`'s content charge; acceptance moves `x` into the connection and refunds its old
storage where it was charged. Until then the send waits. Its entry and its wait are
cancellation points ([CONC-7](#conc-7-cancellation-points)), and a send cancelled there is
not accepted. Waiting sends are accepted oldest first, by the order in which they began
waiting, and no send is accepted while an older one waits, even one that would fit. One
large waiting send therefore holds back every smaller one behind it; a program that needs
small items to pass a large one sends them on another connection. Accepting a newer send
that fits ahead of an older one is not built.
`conn.try_send(c, take x) -> Result[(), SendError[T]]` decides as `send` does, but never
waits and is not a cancellation point: it returns `Full(x)` where `send` would wait.

`conn.receive(i) -> Result[T, InboxError] waits`, where `InboxError = Closed |
Capacity(Capacity)`, takes the item at the front of the queue. It admits the item's content
charge into the current limit and refunds it to the room; when the limit cannot admit the
charge, it returns `Capacity` with the requested and available bytes and leaves the item at
the front. Items leave the inbox in the order the connection accepted them. With no item
queued it returns `Closed` once every client has been cleaned, and otherwise waits; the wait
is a cancellation point. `conn.try_receive(i) -> Result[T, InboxPoll]`, where `InboxPoll =
Empty | Closed | Capacity(Capacity)`, decides as `receive` does, but never waits and is not
a cancellation point: it returns `Empty` where `receive` would wait. This clause does not say
when an item accepted from another unit, or the cleaning of the last client in another unit,
is seen by the inbox's unit.

Cleaning the inbox seals the connection: every later send and every waiting send answers
`Closed(x)`. The cleaning waits for the seal with cancellation shielded, then drops every
item not received, last accepted first, as [CONC-9](#conc-9-queue-drop) says for a channel.
When that cleaning ends, what the drops left charged to the room, such as a failing
finalizer's cause, is charged from then on to the limit that admitted the connection. After
the inbox and every client have been cleaned, the rest of the connection's charge is
refunded to that limit. When the last of them is cleaned in the unit that holds that
limit, the refund is complete when that cleaning ends.

A request that wants a reply carries a client of the asker's own inbox:

```hirsel
type Ask: cross { n: u64, reply: Client[u64] }
fn serve(i: Inbox[Ask]) -> u64 waits {
  let mut total: u64 = 0
  loop {
    match conn.receive(i) {
      Ok(take a) => {
        total += a.n
        let _ = conn.send(a.reply, total)
      },
      Error(Capacity(_)) => {},
      Error(Closed) => break,
    }
  }
  total
}
fn ask(c: Client[Ask], n: u64) -> Option[u64] waits {
  let made: Result[(Client[u64], Inbox[u64]), Capacity] = conn.new(1, bytes 0)
  match made {
    Ok((take reply, take back)) => match conn.send(c, Ask(n: n, reply: take reply)) {
      Ok(()) => match conn.receive(back) { Ok(t) => Some(t), Error(_) => None },
      Error(_) => None,
    },
    Error(_) => None,
  }
}
```
```hirsel
fn open() {
  let r: Result[(Client[Arc[u64]], Inbox[Arc[u64]]), Capacity] = conn.new(1, bytes 0)
}                                                          // refused: LANG-12, Arc is not cross
```
