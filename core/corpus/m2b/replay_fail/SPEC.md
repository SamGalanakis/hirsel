# M2b replay cases for a failing call

Program: `core/programs/m2b/replay/failing.hirsel` (package `failing`), journal
`m2b.journal.call_isolated`, which runs the callee inside `isolate` (FAIL-16) under the
journal's bound. A failure is recorded as tag 2 and its primary cause's kind, read by
`for c in causes` (SEM-6: primary cause first), numbered in FAIL-2's order: Overflow 1,
Bounds 2, Division 3, Unwrap 4, Fail 5 (then its text), Cancelled 6, Capacity 7. Values are
derived by hand; none was pasted from a run.

`used` is 32 for both cases: after `isolate` catches a failure, the task's 32-byte reserved
cell replaces its primary cell and stays admitted (FAIL-16), and a second catch reuses it.

## fails = 11123
`divide` on the empty argument fails with Division. Recorded: the outcome is a failure (1),
one entry. Replayed: it fails again (1) with the same cause, so no divergence. The entry's
outcome is the two bytes 2, 3 (failure, Division). Value `entries × 10000 + recorded failure
× 1000 + replayed failure × 100 + tag × 10 + kind` = 10000 + 1000 + 100 + 20 + 3. Catches: a
failure that escapes the journal, a replayed failure reported as a divergence, a second
entry added on replay, a cause recorded as another kind.

## cause_changed = 400
Replay passes `divide`'s identity with `overflow` as the callable, a body change that keeps
the identity (PROG-9), and it fails with Overflow (2, 1) where Division (2, 3) was recorded:
`Outcome(0)`. Catches: a journal that records only that a call failed.
