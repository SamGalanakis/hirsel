# Schedule

[API overview](../API.md). [Types and operation contracts](../schedule.hirsel).

`std.schedule` computes retry delays from a policy and caller-supplied inputs.
The caller decides which failures to retry, owns the attempt payload, and
handles waiting and cancellation. The policy is plain data and the delay
functions allocate nothing.

## Policies and limits

`fixed(base_ms)` uses the same base for every attempt. `exponential(base_ms,
cap_ms)` doubles the base per attempt. Attempts are zero-based. Attempt zero
uses the base, and every delay is bounded by `cap_ms`, even when the shifted
base would exceed `u64`. A zero base stays zero. `doubles` selects the growth
mode independently of changes to the base or cap. Fixed policies start with a
cap of `u64`'s maximum value.

Both constructors start with jitter disabled and no attempt limit or deadline.
The `with_*` builders return new policies. With `max_attempts: Some(n)`, the
delay functions return `None` at attempt `n` and beyond. With a deadline of
`Some(t)`, they return `None` at `now_ms >= t`. The deadline and `now_ms` use
the same time origin. Before the deadline, the final delay is at most
`t - now_ms`.

## Jitter

For a capped delay `d`, `None` leaves `d` as-is, `Full` draws uniformly from
`[0, d]`, and `Equal` draws uniformly from `[floor(d/2), d]`. Both ranges
include their endpoints. `next_delay` draws from the caller's mutable `Pcg`,
then clips the result to the deadline. A stop limit, zero capped delay, or
`None` jitter leaves the generator state untouched.

`next_delay_fixed` computes the delay with jitter disabled for either growth
mode. It needs no generator. [Contract test](../tests/schedule.hirsel).
