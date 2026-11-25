# Refusal sweeps

[API overview](../API.md). [Operations and contracts](../refuse.hirsel).

```hirsel
pub type Budget { call: u64, limit: u64 }
pub type Refusal[E] { end: E, calls: u64, next: Option[u64] }
pub type Swept { runs: u64, failed: u64 }
pub fn sweep[C, E: key](subject: C, run: fn(C, u64, Option[Budget]) -> Refusal[E], take show_fn: own fn(E, mut String)) -> Result[Swept, String]
pub fn past(budget: u64, c: Capacity) -> u64
pub fn bounded(b: Option[Budget], call: u64) -> Option[u64]
```

`sweep` checks that a refused admission leaves its subject usable. `run(subject, index,
budget)` builds a fresh subject and drives it to an end class, numbering its calls on the
subject. When `budget` is `Some(Budget(call: k, limit: b))`, `run` makes call `k` under
`bound bytes b` ([byte bounds](../../../docs/spec/budgets.md#mem-25-the-bound-scope)). If that
call answers `Capacity` as a value, `run` repeats it unbounded and reports
`next = Some(past(b, c))`. `bounded(budget,
call)` gives the limit for the call that `budget` names. `index` is distinct for each run,
so `run` can draw fresh seeds.

The sweep first runs with no budget. That reference run gives the wanted end class and the
call count. Then, for each call `k` below that count, it runs with budgets 0, then each
run's `next` (at least one byte more), until the bounded call is not refused. Each bounded
run is wrapped in an `isolate`:

- A run that ends in a `Capacity` failure is allowed and counted in `failed`: an admission
  that is not one of the four value-returning refusals raises `Capacity` and unwinds the
  subject with it. The next budget steps past the failure's shortfall.
- Any other failure is red.
- An end class other than the reference run's is red.

`sweep` answers `Error` with up to eight red lines of the form `call K budget B: ...`,
each naming the end class through `show_fn`. A reference run that reports `next` fails
the test.
