# M2b replay cases

Program: `core/programs/m2b/replay/` (library module `m2b.journal` in `journal.hirsel`, cases
in `main.hirsel`, package `app`). Runner: `core/tools/m2b.py` (Buck `//:core-m2b`): the
evaluator runs `case_<name>` with each capability parameter bound to handle 0 of its model
world (clock at 0, +1 per reading, `sleep_until` moves it to the deadline); Cranelift and
LLVM run `main <name>` with `Clock` granted. Every value is derived by hand below from the
journal's rules; none was pasted from a run. `used` is 0: every case frees what it made.

A divergence is reported as `kind × 100 + entry index`: Missing 1, Code 2, Arg 3, Outcome 4,
Extra 5. An outcome is stored as tag 0 plus the result bytes, or tag 1 plus the refused
`Capacity` (requested, available as little-endian u64).

The session used by most cases, from argument "ab": entry 0 `double("ab")` = "abab";
entry 1 the second callee on "abab" (`double` gives "abababab", 8 bytes); entry 2 a clock
read; entry 3 `sleep(1000 ns)`. It returns the second outcome's length and the reading.

## roundtrip = 4008
Record the session, rewind, replay it; `finish` finds every entry consumed. Both runs give
length 8 and the same reading (replay returned the recorded one). Value
`entries × 1000 + length` = 4 × 1000 + 8. Catches: replay that appends entries, a cursor that
does not advance. (A replayed read that asks the host gives 1 against the recorded 0 on the
evaluator, but may repeat the same millisecond natively; `clock` is the case for that.)

## outcome = 401
Entry 1's outcome is replaced by the one byte 0 (tag 0, empty result). Replay re-runs
`double("abab")`, whose outcome is 0 + "abababab": `Outcome(1)`. Catches: a replay that
compares only identities and arguments, or returns the stored outcome without re-running.

## changed_code = 201
Replay passes `triple` and its identity where `double` was recorded at entry 1: `Code(1)`.
Catches: identity bytes that do not depend on the function's name.

## changed_arg = 300
Replay starts from "xy": entry 0's argument differs, `Arg(0)`.

## extra = 501
Replay makes one call and calls `finish`: entries 1..3 are left, `Extra(1)`. Catches: a
`finish` that accepts a shorter run.

## missing = 104
Replay runs the whole session and then one more call: there is no entry 4, `Missing(4)`.

## clock = 7
One recorded clock read; its stored u64 is set to 7 (byte 0 = 7, bytes 1..7 = 0), then
replayed. The value is 7, which no host reading here gives (the evaluator's second reading
is 1, a native wall clock is far above 7): replay read the journal, not the host.

## sleep = 11
`sleep(200 ms)` recorded, then replayed, with `host.mono_ns` read around each. Recording
waits at least 200 ms (10); replay waits less than 200 ms (1). Value 10 + 1. On the
evaluator, recording moves the model clock to the deadline and replay only adds one reading.
Catches: a replay that waits again, a record that does not wait.

## refused = 10000065536
`big` asks for a 100000-byte list inside the journal's `bound bytes 65536`; nothing else is
admitted inside the bound, so it is refused with `Capacity { requested: 100000, available:
65536 }` (MEM-25). The refusal is recorded (tag 1), and replay meets the same refusal with
the same figures. Value `requested × 100000 + available`. Catches: a refused allocation
treated as a divergence or dropped from the outcome.

## identity = 1 (red until core-form-883)
`code.identity(code.describe(double))` compared with PROG-9's own example bytes for this
function: `\x01app@1/main/double(core@1/core/List[u8])->core@1/core/Result[core@1/core/List[u8],core@1/core/Capacity]`.
Today the front end writes `view ` and spaces, so the case gives 0.

## bench (measurement only, not a case)
`bench(100000)` records 100000 calls of `double` on k's 8 little-endian bytes and replays
them. Value `entries + Σ (k mod 256)` = 100000 + 390 × 32640 + Σ 0..159 = 12842320; the
Rust twin `core/twins/m2b/replay.rs` prints the same.
