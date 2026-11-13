# Tasks

### CONC-1: Tasks

A **task** is a sequential computation with its own stack, limit and regions. Tasks are
created only through a `Tasks` capability ([CAP-1](capabilities.md#cap-1-authority-is-explicit)).
A task's handle is a resource that is not `send`, so it stays in the task that spawned it,
and a task ends only after cleaning its handles (CONC-8): no task outlives its spawner.
A value passes from one task to another only by moving through a spawn, a channel or a
join. The only program memory two tasks can both read is the immutable payload of an `Arc`
(CONC-11). Whether tasks run in parallel is the engine's choice and is not observable,
except in a run that CONC-10 leaves unspecified.

### CONC-2: The send bound

`send` is the bound of values that may move to another task. Plain types are `send`. `Arc[T]`
is `send`. An own or resource type is `send` when it declares the bound
([OWN-4](ownership.md#own-4-bounds)) and each field type is `send`; a capability type is
`send` when its declaration says so ([CAP-2](capabilities.md#cap-2-capabilities-are-opaque)).
A plain `fn` is `send`; an owning closure is `own[send] fn` when every capture is `send`
([LANG-13](language.md#lang-13-callables)). Never `send`: `Rc`, stable values, regions,
scoped values, builders and task handles. The check reads declarations and capture lists only.

```hirsel
type Job: send { id: u64, name: String }
```
```hirsel
type Node: shareable { label: String }
type Job: send { id: u64, cache: Rc[Node] }            // refused: CONC-2, Rc is not send
```

### CONC-3: Spawn

`task.spawn(tasks, bytes n, take x, f) -> Result[Task[R], (Capacity, X)]` starts a task
that runs `f(take x)`, where `x: X`, `X` and `R` are `send`, and `f: fn(take X) -> R waits`
is a plain callable, which captures nothing ([LANG-13](language.md#lang-13-callables)).
Everything the child owns arrives as `x`. Spawn admits the task charge
([MEM-24](budgets.md#mem-24-logical-charges)), which includes `n`, against the spawner's
current limit, and the child's root limit is `n` ([MEM-19](budgets.md#mem-19-limits)). Admission is the
commit point; on `Capacity` the error owns `x`
([T7.1](ownership.md#own-26-the-transition-table)).

```hirsel
fn count(tasks: Tasks, take words: List[String]) -> Result[Task[u64], (Capacity, List[String])] {
  task.spawn(tasks, bytes 4096, take words, fn(take w: List[String]) -> u64 { list.len(w) })
}
```
```hirsel
type Node: shareable { label: String }
fn count(tasks: Tasks, take words: List[Rc[Node]]) -> Result[Task[u64], (Capacity, List[Rc[Node]])] {
  task.spawn(tasks, bytes 4096, take words, fn(take w: List[Rc[Node]]) -> u64 { list.len(w) })
}                                                          // refused: CONC-2, Rc is not send
```

### CONC-4: Channels

`channel.new[T: send](cap: u64) -> Result[(Sender[T], Receiver[T]), Capacity]` admits the
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
point; it returns `Closed` once the channel is closed and empty. The queue is first in,
first out by publication order; positions are logical, so how an engine stores the queue
is not observable. The blocks of a sent value stay charged to the limit that admitted them
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
`type JoinError = Failed(Causes) | Cancelled(Causes)`. The child
cleans its own state before it ends. `Ok(r)` moves the result to the joiner; `Failed(causes)`
delivers the child's cause chain ([FAIL-10](failure.md#fail-10-cause-chain)) as a value the
joiner owns, made of the child's cells, so the joiner's own primary cell is untouched and
`join` reserves nothing; `Cancelled(causes)` does the same for a child whose primary cause
is `Cancelled`, with any causes its cleanup added. `join` is a
cancellation point: if the joiner observes cancellation while waiting, `h` is dropped by
the joiner's cleanup (CONC-8).

```hirsel
fn total(tasks: Tasks, take a: List[String]) -> Result[u64, JoinError] waits {
  let h = match task.spawn(tasks, bytes 4096, take a, fn(take w: List[String]) -> u64 { list.len(w) }) {
    Ok(take h) => h, Error((_, take back)) => return Ok(list.len(back)),
  }
  task.join(take h)
}
```

### CONC-7: Cancellation points

`task.cancel(mut h)` requests cancellation of a child and returns at once. A task observes
a pending request only at a **cancellation point**: the entry of every core `waits`
operation (`send`, `receive`, `join` and the waiting provider operations of
[CAP-5](capabilities.md#cap-5-provider-operations)), each moment such an operation would
wait, and `task.check_cancel()`. At an entry, a pending request is observed before anything
else is done. A provider operation that has been issued completes and delivers its result
([CAP-8](capabilities.md#cap-8-the-replay-point)); cancellation reaches it only at issue.
A user function declared `waits` is not itself a cancellation point. Observing it is a failure with cause `Cancelled`
([FAIL-2](failure.md#fail-2-causes)), which runs the task's cleanup. Pure code is never
interrupted, finalizers are shielded and observe nothing, and no engine inserts a yield or a check. A loop that
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
observation, every `task.check_cancel()`, publication by `send`, every `try_send` decision, delivery by `receive`, every
change of a channel's sender count, `join`, channel close, every charge or refund that
crosses tasks ([MEM-20](budgets.md#mem-20-limit-records-and-hand-over)), and every `Arc`
creation, share, release and `try_unwrap`. Each operation observes the state that the order gives.
The order is chosen by the scheduler provider at the replay point
([CAP-8](capabilities.md#cap-8-the-replay-point)); with the same inputs and the same
recorded choices, every task behaves identically on every engine.

Every event is a point where the scheduler may run another task, and every scheduler
provider is fair: no task performs unboundedly many events while another task that is
not waiting performs none, unless that other task computes forever without an event. A
task is waiting only inside a `waits` operation at a moment it would wait (CONC-7). A
task that polls (a `try_send` returning `Full`, a failed `try_unwrap`, a `check_cancel`
that finds no request) is not waiting, so the tasks it polls for reach their events.
Fairness limits which orders the scheduler may choose; a replay follows the recorded
order. A task that computes forever without an event is a program error: the run does not
end, and which events other tasks perform meanwhile is not specified. Each `try_send`
below is an event, so the parent that drains the gate runs while the child polls:

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
