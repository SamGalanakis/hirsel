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
first sleep moves the model clock past the budget (one sleep, a large elapsed): 1.
Catches: a wait that is a spin (native `sleep_until` was one before core-ops-914's
d30f980d; the case gave 0).

## replace = 1
Writes "1" to `a` and "2" to `b`, renames `a` to `b`, reads `b`. core.md says rename
replaces a regular file at the target, which is what publishing over a stale name relies on:
value 1 when `b` holds "1". Catches: a rename that refuses an existing target (the evaluator
returned Exists and gave 0 before core-form-883's FORM #105).

## rename_refusals = 122011
core.md's other rename sentences in one directory holding the file `a` ("1") and the
directory `m`, one decimal digit each (0 Ok, 1 NotFound, 2 Denied, 3 Exists, 4 Closed,
5 Other): `x` (absent) to `m` is NotFound (1, the sentence is unconditional, though `m`
alone would be Denied); `m` to `c` is Denied, `from` not a regular file (2); `a` to `m`
is Denied, `to` a directory (2); `a` to `a` succeeds (0); `a` still reads "1" (1, the
errors and the same-name rename renamed nothing); `c` is absent (1). Catches: a rename that
moves a directory, replaces one, reports Other for one (native renameat's EISDIR), or
moves a file onto its own name. Symbolic links, which core.md also does not follow, are
not in the evaluator's world; the native provider's table covers them
(crates/hirsel-core-rt/src/provider_tests.rs).
