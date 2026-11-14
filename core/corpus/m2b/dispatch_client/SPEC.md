# M2b dispatch: the client role alone

Program: `core/programs/m2b/dispatch/client.hirsel` (package `client`), with `m2b.spool` and
`rpc.handlers`. Runner and engines as in `../dispatch_server/SPEC.md`: an empty spool
directory per engine, no server process. Values are derived by hand; none was pasted from a
run. `used` is 0.

## solo_reply = 1104
The case publishes reply 0 = `Done("abab")` itself, then `call`s request 0 for
`rpc.handlers.double("ab")`: the request is published, the reply is found on the first
attempt, so no sleep happens. It then takes q0 back with a budget of 0 (one attempt) and
compares it with the frame it meant to send. Value 1000 (q0 present and equal to the frame:
the rename published it whole) + 104 (`Done` with 4 bytes) + 10000 × sleeps (0). Catches: a
publish that leaves the request under its temporary name, a client that sleeps before its
first look.

## poll_waits = 1
A poll for reply 7, which nobody writes, with a 1 ms poll interval and a 20 ms wall-clock
budget. The poll must time out; every `sleep_until` waits one interval (core.md: it returns
once the clock reaches the deadline), so the wall time that passed covers the sleeps:
`elapsed + 1 >= sleeps`, the + 1 for the millisecond resolution of `wall_ms`. Value 1. A
sleep that does not wait makes thousands of sleeps in 20 ms and gives 0. On the evaluator the
first sleep moves the model clock past the budget (one sleep, a large elapsed): 1. Red on
Cranelift and LLVM, giving 0: native `sleep_until` moves the provider's monotonic reading to
the deadline and returns at once (os.rs SLEEP_UNTIL, core-ops-914). Catches: a wait that is a
spin.

## replace = 1
Writes "1" to `a` and "2" to `b`, renames `a` to `b`, reads `b`. core.md says rename
replaces a regular file at the target, which is what publishing over a stale name relies on:
value 1 when `b` holds "1". Red on the evaluator, giving 0: its rename returns Exists for an
existing target (eval.rs `HostOp::Rename`).
