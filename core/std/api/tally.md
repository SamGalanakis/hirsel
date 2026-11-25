# Tally

[API overview](../API.md).

`std.tally.Tally[T: send]` counts and folds across tasks with no shared state; it is
the library form that takes the place of atomics for counting (Java's LongAdder, OTP
`counters`, and Linux per-CPU counters are its prior art). `new(n, start, combine)` opens one channel of max(n, 1) items, admitted
at creation, and reserves n slots; `start: fn() -> T` makes a fresh partial and
`combine: fn(take T, take T) -> T` folds partials, both cold. `handle(mut t)` issues a
`send` resource `Handle[T]` with the next index (0, 1, ... in the owner's program
order) and the partial `start()`; after n handles it returns `Error(Exhausted)`, and a
refused handle block returns `Error(Capacity(c))`, leaving the tally unchanged either
way. `index(h)` reads that index, which with a local sequence makes a deterministic id
with no event.

`update(mut h, f: scoped fn(mut T) -> R) -> R` applies f to the handle's own partial
in place, the `map.update` shape: f is given at the call site, so the engines inline
it into the caller's loop, and the partial is a plain T that is never empty, so there
is no tag test. A counter is `update(mut h, fn(mut n: u64) { n += 1 })`, a float sum
`fn(mut s: f64) { s += x }`, a concatenation `fn(mut s: String) { string.push(mut s, piece) }`.
`update` is the only way to change a partial. It is neither an event nor a
cancellation point and touches nothing another task can see. A handle's cleanup sends
`(index, partial)` once, so a handle dropped by a failing or cancelled task still delivers
its partial during cleanup, as far as an `update` that failed had changed it.
`finish(take t) -> T waits` drops the owner's sender, receives until every handle is
cleaned and folds `start()` then the partials in handle order. The partition of
updates among handles and their order within each are fixed by the programs, so the
result is the same under every schedule, also for a combine that is neither
associative nor commutative (a float sum, a concatenation). `finish` waits for every
issued handle: a handle moved into a long-lived child delays it until that child ends.

`flush(mut h) waits` sends the partial so far and starts a fresh `start()`; it is an
event, and one handle's parts fold into its slot in send order. `read(mut t)` (T:
plain) drains what has arrived with `try_receive` and returns `start()` folded with
the slots so far; its value depends on the schedule. Each handle's cleanup always fits
the queue; a flush or a cleanup waits only when flushes have filled it while the owner
is not reading or finishing, so an owner that joins a flushing task before reading can
wait forever.
The scripted-schedule laws are in `tests/tally.hirsel` (`// schedule: seeds N`, run
by `core/tools/std_schedule.c`); the evaluator has no tasks, so it runs only the
single-task laws.
