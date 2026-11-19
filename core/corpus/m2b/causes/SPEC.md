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

Used, by FAIL-10, FAIL-16 and MEM-24. Each catch installs its 32-byte reservation as the
task's primary. Dropping the caught Causes refunds the previous 32-byte primary and
any owned message. An isolate whose body returns refunds its own reservation. Each
case returns with zero net bytes admitted relative to entry.

In a child task (`spawn` with `bytes 4096`, then `join`), by FAIL-16, FAIL-10, MEM-20 and MEM-23:
- child_drop: the child runs `drop`, so 1. The catch installs the child's reserved cell (32, admitted from the
  child's limit) as the child's primary, and the child returns. Its limit ends and is handed over to the root
  (MEM-20), so the cell's record leads to the root. `join` cleans the child's context, which refunds that cell
  to the root; the unused reservation, the task record and the message String are refunded too. So 0 stays.
- child_kept: the child returns its Causes through `join`; the root reads it (Fail, so 1) and drops it, which
  refunds its cell and message to the root, where the child's records now lead. The child's own replacement
  cell is refunded when `join` cleans the child. So 0 stays.
