# Exec

[API overview](../API.md). [Operations and contracts](../exec.hirsel).
[Core launch and lifecycle](../../../docs/spec/exec.md).

`run_until(mut p, clock, deadline_ns, grace_ns)` waits for a natural exit until
the absolute monotonic deadline. If the child exits first, it returns the
reaped status at once with stage `Exited`. On timeout it sends SIGTERM and
waits until `deadline_ns + grace_ns`, saturating at the maximum u64. It returns
stage `Terminated` if that wait reaps the child. If the grace period also
expires, it sends SIGKILL and waits for the reaped status with stage `Killed`.

The final wait has no second deadline. On a native host, the grace deadline
determines when to escalate. Reaping can finish later. A signal or wait error
leaves the caller holding the Proc.

`output` spawns the supplied launch and closes any piped stdin. It drains
stdout and stderr in separate tasks, with an independent capture limit for
each. It returns the reaped termination, each captured prefix, and whether
each pipe produced bytes past its limit. A discarded stream gives an empty
capture. A limit of zero still drains a piped stream, so a child can finish
writing. A nonzero exit status is a successful collection.

The same `Until` applies to the wait and each read. Process, stdout, stderr, and
worker errors identify where collection failed. When collection fails, scope
cleanup closes the pipe halves and releases the `Proc`
([child process cleanup](../../../docs/spec/exec.md#exec-6-drop-and-cleanup)). Each piped
stream reserves a task with its rounded capture limit plus 4096 bytes.
The task allocates scratch after filling the capture. The returned prefixes
keep their storage.
