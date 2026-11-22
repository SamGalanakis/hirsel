# Clock

[API overview](../API.md). [Operations and contracts](../clock.hirsel).

`sleep_ns` reads the supplied Clock once, adds the relative duration with
checked arithmetic, then waits through `host.sleep_until`. Overflow fails before
waiting. Entry and waiting are cancellation points. Replay delivers the recorded
reading and completion. No allocation or ambient clock authority is introduced.

`deadline_after_ms` adds milliseconds to an absolute monotonic nanosecond
reading. The conversion and sum saturate at the maximum u64. It reads
no clock and allocates nothing. For example, call
`clock.deadline_after_ms(host.mono_ns(c), 5)` for five milliseconds after the
current reading. `clock.deadline_after_ms(2_000_000, 5)` returns `7_000_000`.

`alarm` spawns a child with a 4096-byte limit and its own sender handle. The
child sleeps until the absolute monotonic deadline on the supplied `Clock`,
then sends the owned message once. It waits if the queue is full and drops the
message if the receiver has closed. Dropping the task before the deadline
cancels the alarm without delivering the message.
