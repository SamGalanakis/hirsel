# causes: isolate catching a Fail(String) cause

From spec-v1-880's M2b findings (v.hirsel, retry.hirsel). `boom` and `attempt(n < 2)` fail with a heap
`String` message ("busy", built by `string.from`); `attempt(5)` unwraps a `None`; any other `attempt(n)` is
`10 * n`.

Values, by hand:
- drop: the isolate catches Fail, so `Ok(Error(_))` gives 1. read: the first cause's kind is `Fail(_)`, giving 1.
  twice: drop + drop = 2.
- fail_then_ok (start 0, limit 5): n=0 and n=1 are Fail, so they are retried; n=2 returns 20, giving `Done(20)` = 20.
- unwrap_stops (start 5): Unwrap is not retryable, giving `Stopped(5)` = 2005.
- gives_up (start 0, limit 1): n=0 is Fail and 0 - 0 + 1 >= 1, giving `GaveUp(0)` = 1000.

Used, by FAIL-16 and MEM-24 (the isolate cell is 32):
- The first caught failure fails into the root's primary cell, which is outside the limit. The catch installs the
  reserved cell (32, admitted) as the primary, and dropping the Causes frees the message String (refunded) and
  the root's cell (nothing to refund), so 32 stays.
- A later caught failure uses that primary, so its own reserved cell takes its place. The dropped Causes then
  refunds the earlier cell, so 32 stays; an isolate that returns refunds its own reservation.
- So every case returns with 32 used.
