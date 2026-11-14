# M2b transaction cases

Program: `core/programs/m2b/tx/` (library module `m2b.tx`, cases in `main.hirsel`).
Runner: `core/tools/m2b.py` (Buck `//:core-m2b`), as for the table cases. Every value is
derived by hand below; none was pasted from a run.

Every case starts from the bank: accounts 1 = 10, 2 = 20, 3 = i64 max, 4 = 40, owners
empty. It runs one transaction with `m2b.tx.run` and a fresh `Log[u64]`, then builds its
value from four parts, written as decimal digits from left to right:

- the outcome: 1 `Committed`, 2 `Aborted(Insufficient)`, 5 `Aborted(NoRow)`,
  6 `Aborted(Full)`, 3 `Failed`, 4 `Refused`;
- one digit for each key in the read log, in append order (`log.finish`);
- 1 if the table then holds exactly accounts 1 = b1, 2 = b2, 3 = i64 max, 4 = 40,
  else 0. b1/b2 are 5/25 after the commit and 10/20 (unchanged) everywhere else;
- 1 if a later transfer of 1 from account 2 to account 1 on the same table commits
  and returns account 2's new balance (24 after the commit, 19 elsewhere), else 0.
  This shows the task goes on after the outcome.

`used` is the bytes still admitted when the case returns. A caught failure used the
task's primary cell, so `isolate` leaves its reserved 32-byte cell in its place (FAIL-16,
MEM-24): 32 after each case with one caught failure, and 0 for every other case.

| case | body | value | used |
|---|---|---|---|
| commit | transfer 5 from 1 to 2: reads 1, 2; writes 1 = 5, 2 = 25 | 1 12 1 1 = 11211 | 0 |
| insufficient | transfer 50 from 1 to 2: reads 1, 2; 10 < 50 | 2 12 1 1 = 21211 | 0 |
| overflow | transfer 5 from 1 to 3: reads 1, 3; writes 1 = 5; `i64 max + 5` fails | 3 13 1 1 = 31311 | 32 |
| bounds | reads 1, then indexes an empty list at 0 | 3 1 1 1 = 3111 | 32 |
| unwrap | reads 1 and 9, then `option.unwrap` of the absent 9 | 3 19 1 1 = 31911 | 32 |
| explicit_fail | reads 1, writes 1 = 17, then `fail` | 3 1 1 1 = 3111 | 32 |
| refused_alloc | reads 1, then `list.with_capacity(8)` inside `bound bytes 0` | 6 1 1 1 = 6111 | 0 |

These catch:
- a write set installed after a failure (the overflow and explicit_fail writes);
- a failure not caught, or a task that does not continue;
- a read log that loses the reads made before a failure, or logs only reads that
  hit. The second was confirmed red on a mutant: unwrap gave 3111;
- an Error abort that commits;
- a refused allocation treated as a failure: it is an Error (FAIL-2), so it gives
  `Aborted(Full)` and keeps no cell.

## full = 411512072

Without a later transaction, inside `bound bytes 640`. `Account` is 40 bytes, so a
`Map[u64, Account]` entry charges 64 (MEM-24). The table is empty, so its first
growth is charged to the bound (MEM-26): rows 1..4 make capacity 4, 256 bytes. The
log node is 24 bytes, and its first entry grows its storage to capacity 4 of u64,
32 bytes. The body `open` reads 1 and writes 1 = 5 and 5 = 5 into its own write map,
whose capacity 4 is 256 bytes. The isolate cell is refunded when the body returns.
Admitted: 256 + 24 + 32 + 256 = 568, so 72 are available. Commit counts one fresh key
(5) and reserves for it before any write. Five keys need capacity 8, 512 bytes,
admitted before the old storage is refunded: refused, `Capacity { requested: 512,
available: 72 }`. The outcome is `Refused`. The read log is [1]. The table still holds
the bank with no row 5.

Value `((4 1 1) × 1000 + 512) × 1000 + 72` = 411512072. This catches a commit that
writes before it reserves: on that mutant, account 1 becomes 5 before the put of 5 is
refused, and the value is 410512072. That was confirmed red.

## generic_relay = 2

Not a transaction. This is the smallest shape found that the evaluator refuses with `not
well formed: direct call through another signature`. `apply[A, R]` is instantiated
at one type from a generic caller's fn parameter and from a plain callee's fn item.
`direct()` is `successor(1)` = 2, which the case returns. Red on the evaluator until
core-form-883 fixes it.

On Cranelift every row is red until core-cranelift-904 lowers `isolate`.
