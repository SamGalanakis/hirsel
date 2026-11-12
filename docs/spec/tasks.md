# Tasks

Tasks share nothing. A value passes from one task to another only by moving it through a
channel, a spawn or a join; the only memory two tasks can both read is the payload of an
`Arc`, which is immutable. Every interaction between tasks is an event in one recorded
order, so a run replays exactly.

### CONC-1: Tasks

A **task** is a sequential computation with its own stack, limit and regions. Tasks are
created only through a `Tasks` capability ([CAP-1](capabilities.md#cap-1-authority-is-explicit)).
A task's handle is a resource that is not `send`, so it stays in the task that spawned it,
and a task ends only after cleaning its handles (CONC-8): no task outlives its spawner.
Whether tasks run in parallel is the engine's choice and is not observable.

### CONC-2: The send bound

`send` is the bound of values that may move to another task. Plain types are `send`. `Arc[T]`
is `send`. An own or resource type is `send` when it declares the bound
([OWN-4](ownership.md#own-4-bounds)) and each field type is `send`; a capability type is
`send` when its declaration says so ([CAP-2](capabilities.md#cap-2-capabilities-are-opaque)).
A closure is `send` when every capture is. Never `send`: `Rc`, stable values, regions,
scoped values, builders, task handles, and branded values whose brand owner is
not moved with them in one value. The check reads declarations and capture lists only.

```hirsel
type Job: send { id: u64, name: String }
```
```hirsel
type Job: send { id: u64, cache: Rc[Node] }            // refused: CONC-2, Rc is not send
```

### CONC-3: Spawn

`task.spawn(tasks, bytes n, take x, f) -> Result[Task[R], (Capacity, X)]` starts a task
that runs `f(take x)`, where `x: X`, `f: fn(take X) -> R`
([LANG-13](language.md#lang-13-callables)), and `X`, `R` and `f` are `send`. Everything
the child owns arrives as `x`. Spawn admits the task charge
([MEM-26](budgets.md#mem-26-logical-charges)) plus `n` against the spawner's current limit,
and the child's root limit is `n` ([MEM-21](budgets.md#mem-21-limits)). Admission is the
commit point; on `Capacity` the error owns `x`
([T7.1](ownership.md#own-26-the-transition-table)).

```hirsel
fn count(tasks: Tasks, take words: List[String]) -> Result[Task[u64], (Capacity, List[String])] {
  task.spawn(tasks, bytes 4096, take words, fn(take w: List[String]) -> u64 { list.len(w) })
}
```
```hirsel
fn count(tasks: Tasks, take words: List[Rc[Node]]) -> Result[Task[u64], (Capacity, List[Rc[Node]])] {
  task.spawn(tasks, bytes 4096, take words, fn(take w: List[Rc[Node]]) -> u64 { list.len(w) })
}                                                          // refused: CONC-2, Rc is not send
```

### CONC-4: Channels

`channel.new[T: send](cap: u64) -> Result[(Sender[T], Receiver[T]), Capacity]` admits the
channel's whole charge, including `cap` items, at creation; sending never allocates.
`Sender[T]` is a `send` resource; `channel.sender(tx)` adds a sender and cannot fail.
`Receiver[T]` is a `send` resource with no way to add a second receiver. When the last sender is cleaned, the
channel is closed for receiving after its queued items; when the receiver is cleaned, the
channel is closed for sending and its queued items are dropped (CONC-9).

### CONC-5: send and receive

`channel.send(tx, take x) -> Result[(), Closed[T]]` waits while the queue is full, then
publishes `x`; after publication the queue owns `x`. If the channel is closed the error
owns `x`. `send` is a cancellation point: cancellation observed before publication is a
failure, and the sender's cleanup drops `x` ([T7.2, T7.3](ownership.md#own-26-the-transition-table)).
`channel.try_send(tx, take x) -> Result[(), SendError[T]]` never waits and returns
`Full(x)` or `Closed(x)`. `channel.receive(rx) -> Result[T, Closed[()]]` waits for an item and
is a cancellation point; it returns `Closed` once the channel is closed and empty.
Items are received in publication order. The blocks of a sent value stay charged to the
limit that admitted them ([MEM-22](budgets.md#mem-22-each-block-remembers-its-limit)).

```hirsel
fn pump(tx: Sender[Job], take jobs: List[Job]) -> Result[(), Closed[Job]] {
  for take j in jobs { channel.send(tx, take j)? }
  Ok(())
}
```

### CONC-6: join

`task.join(take h: Task[R]) -> Result[R, JoinError]` waits for the child to end. The child
cleans its own state before it ends. `Ok(r)` moves the result to the joiner; `Failed(causes)`
delivers the child's cause chain ([FAIL-10](failure.md#fail-10-cause-chain)) as a value the
joiner owns, made of the child's cells, so the joiner's own primary cell is untouched and
`join` reserves nothing; `Cancelled` reports a child that ended by observing cancellation. `join` is a
cancellation point: if the joiner observes cancellation while waiting, `h` is dropped by
the joiner's cleanup (CONC-8).

```hirsel
fn total(tasks: Tasks, take a: List[String]) -> Result[u64, JoinError] {
  let h = match task.spawn(tasks, bytes 4096, take a, fn(take w: List[String]) -> u64 { list.len(w) }) {
    Ok(take h) => h, Error((_, take a)) => return Ok(list.len(a)),
  }
  task.join(take h)
}
```

### CONC-7: Cancellation points

`task.cancel(mut h)` requests cancellation of a child and returns at once. A task observes
a pending request only at a **cancellation point**: a waiting `send`, `receive` or `join`,
a provider call that its signature marks as waiting ([CAP-5](capabilities.md#cap-5-providers)),
and `task.check_cancel()`. Observing it is a failure with cause `Cancelled`
([FAIL-2](failure.md#fail-2-causes)), which runs the task's cleanup. Pure code is never
interrupted, finalizers are shielded, and no engine inserts a yield or a check. A loop that
should be cancellable calls `task.check_cancel()`.

```hirsel
fn spin(n: u64) -> u64 { let mut i: u64 = 0; while i < n { task.check_cancel(); i += 1 }; i }
```

### CONC-8: Abandoned handles

Cleaning an unjoined `Task[R]` cancels the child, waits for it to end with cancellation
shielded, then drops its result or its cause chain. The wait, the custody transfer of the
result and its refund are recorded events (CONC-10). Cancellation therefore reaches every
descendant through its spawner's cleanup.

### CONC-9: Queue drop

Items still queued when a channel is dropped are cleaned from the highest queue index
down. A finalizer failure among them appends its cause, and the remaining items are still
cleaned ([T7.7](ownership.md#own-26-the-transition-table)).

### CONC-10: Event order

A run has one total order of **events**: spawn, task end, publication by `send`, delivery by
`receive`, `join`, channel close, every charge or refund that crosses tasks
([MEM-22](budgets.md#mem-22-each-block-remembers-its-limit)), and every `Arc` creation,
share, release and `try_unwrap`. Each operation observes the state that the order gives.
The order is chosen by the scheduler provider at the replay point
([CAP-8](capabilities.md#cap-8-the-replay-point)); with the same inputs and the same
recorded choices, every task behaves identically on every engine.

### CONC-11: Arc across tasks

An `Arc` node stays charged to the limit that created it; its last release, in whichever
task it happens, refunds it there. Counts are exact under the event order: `try_unwrap`
succeeds in exactly the runs whose order puts it after every other holder's release.

### CONC-12: No shared mutable state

v1 has no mutable value reachable from two tasks: no mutable statics, no cells, no
mutexes and no atomics. Top-level `const` values are plain and immutable.

```hirsel
const LIMIT: u64 = 64
```
```hirsel
var hits: u64 = 0                                        // refused: LANG-5, no mutable statics
```
