# M2b replay cases on the intended StaticFn API

Program: `core/programs/m2b/replay/static.hirsel` (package `static`): one `StaticFn` names
the function, and the journal takes both its identity and its callable from it (PROG-9).
Both rows are red until a `StaticFn` parameter lowers (core-check-callables-913); then
`m2b.journal.call` takes the `StaticFn` and the workaround's separate identity argument goes.

## static_call = 1004
Record and replay `double("ab")` through `code.describe(double)`: one entry, outcome "abab".
Value `entries × 1000 + length` = 1000 + 4.

## static_key = 10
PROG-9: a `StaticFn` is a key, equal exactly when its identity bytes are. `describe(double)`
equals itself (1); `describe(double)` and `describe(other)` differ (0). Value `1 × 10 + 0`.
