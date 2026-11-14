# M2b replay cases for a failing call

Program: `core/programs/m2b/replay/failing.hirsel` (package `failing`), journal
`m2b.journal.call_isolated`, which runs the callee inside `isolate` (FAIL-16) under the
journal's bound. A failure is recorded as the one-byte outcome tag 2. Values are derived
by hand; none was pasted from a run.

`used` is 32 for both cases: after `isolate` catches a failure, the task's 32-byte reserved
cell replaces its primary cell and stays admitted (FAIL-16), and a second catch reuses it.

## fails = 1112
`divide` on the empty argument fails with Division. Recorded: the outcome is a failure (1),
one entry. Replayed: it fails again (1), and the outcomes agree, so no divergence. The
entry's outcome is the single tag byte 2. Value `entries × 1000 + recorded failure × 100 +
replayed failure × 10 + tag` = 1000 + 100 + 10 + 2. Catches: a failure that escapes the
journal, a replayed failure reported as a divergence, a second entry added on replay.

## cause_changed = 400 (red: Causes has no reader)
Replay passes `divide`'s identity with `overflow` as the callable, a body change that keeps
the identity (PROG-9), and it fails with Overflow where Division was recorded. The intended
value is `Outcome(0)` = 400. `Causes` is opaque, so both outcomes are tag 2 and replay
reports nothing: the case gives 1.
