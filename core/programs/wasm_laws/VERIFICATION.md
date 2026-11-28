# Contained foreign verification

Each law's expected value comes from this section, the WebAssembly specification, Wasmtime's
documented behaviour, or the fixture's own source. None comes from a run's output. Each law runs
on both native backends, on x86-64 and AArch64 Linux. The fixture package `wasm_laws`, with
interface `Box` and kind `Cell`, has a module `box.wasm` built by its `build.sh` from `box.c`
with prefix `hwl_`. Its `build.sh` compiles `box.c` with clang 21 as FFI-10 says, and
assembles `box_traps.s`, written in LLVM's WebAssembly assembly, with the same clang. The
repository's pinned LLVM tree builds only the X86 and AArch64 targets, so `build.sh` takes an
LLVM 21 clang, `wasm-ld` and `llvm-objcopy` that have the WebAssembly target from the
environment. `wasm-ld` links both with `--initial-memory=131072` <!-- doc-ref: external -->,
`-z stack-size=16384` <!-- doc-ref: external -->, `--stack-first` <!-- doc-ref: external --> and
`--table-base=3` <!-- doc-ref: external -->, so the memory's 2 pages and the table's 4 elements
are stated link inputs, and the stack and static data lie in the first page.
The functions whose operators a law counts are written in `box_traps.s`, so no optimizer
folds a loop, removes a recursion or chooses the trapping operator: at `-O2`, a C loop that
returns `n` folds to `return n`, and signed overflow, a wild load and a mistyped indirect call
are undefined behaviour in C. The module imports exactly two null imports,
`wasi_snapshot_preview1.clock_time_get` and `wasi_snapshot_preview1.proc_exit` (declared in
`box.c` with clang's `import_module` and `import_name` attributes), defines one table of 4
`funcref` elements (slots 0 to 2 null, slot 3 a function of type `[] -> []`), and
exports:

- `make(f) -> Option[foreign.Ptr[Cell]]` and `free(f, take p) -> i32`, which use a static pool
  of 8 cells, and `live(f) -> u64`, the number of cells in use (`box.c`);
- `spin(f, n: u64) -> u64`, which loops `n` times and returns `n` (`box_traps.s`);
- `straight(f) -> i32`, which runs 2,000 `i32.const 1; i32.add` pairs, 4,000 straight-line
  operators with no loop and no call, from 0, and returns 2,000 (`box_traps.s`);
- `straight_trap(f) -> i32`, which runs the same operators and ends in `unreachable`
  (`box_traps.s`);
- `loop_div(f, n: u64) -> i32`, which loops `n` times with no call and then executes
  `i32.div_u` of 1 by 0 (`box_traps.s`);
- `fill_mem(f, n: u32) -> u32`, which executes one `memory.fill` of `n` bytes of `0xAB` at
  address 65,536 and returns the byte there (`box_traps.s`);
- `grow(f, pages: u32) -> i32`, which returns `memory.grow`'s result;
- `tgrow(f, n: u64) -> i32`, which `n` times asks `table.grow` for `2^32 − 1` null elements,
  each refused with −1 but charged its requested count, and returns the last answer
  (`box_traps.s`);
- `fill(f, mut b: Bytes) -> u64`, which writes `0xAB` to each byte, and `sum(f, b: Bytes) ->
  u64`;
- `nan32(f) -> f32`, which returns `f32.reinterpret_i32(0x7fa00001)`, a NaN that no float
  operator made (`box_traps.s`);
- `div_bits(f, a: f64, b: f64) -> u64`, which executes `f64.div` of `a` by `b` in the guest,
  stores the quotient to memory and returns its bits read back as an `i64` (`box_traps.s`);
- `trap(f, k: u32) -> i32`, which for `k` from 1 to 10 traps as row `k` of FFI-14's table, in
  its order (`box_traps.s`): `unreachable`; an `i32.load` at `0xFFFFFFF0`; `call_indirect` of
  slot 4; of slot 0; of slot 3 with type `[] -> [i32]`; `i32.div_s` of `-2^31` by `-1`;
  `i32.div_u` by 0; `i32.trunc_f32_s` of a NaN; a call of `recurse` with depth `2^32 − 1`; and
  `proc_exit(7)`;
- `clock(f) -> i32`, which calls `clock_time_get(0, 1, a)` with an address `a` in its static
  data and returns the answer;
- `count(f) -> u64`, the number of entries so far into the instance's exported functions other
  than `loan_area`, this one included;
- `fill_trap(f, mut b: Bytes) -> u64`, which writes `0xAB` to each byte and then executes
  `unreachable`;
- `set_area(f, mode: u32) -> i32`: with mode 1, `loan_area` from then on returns the memory's
  size in bytes minus 8, which is a multiple of 8 but leaves no room for 16 bytes;
- `recurse(f, d: u32) -> u32`, which recurses `d` frames deep and returns `d`, holding its
  depth in Wasm locals only (`box_traps.s`);
- `loan_area(n)`, which in mode 0 returns the address of a static 4,096-byte area, and traps
  when `n` is larger.

`box.wasm` declares an initial memory of 2 pages and no maximum, and one table of 4 elements.
At instantiation they are charged 131,072 + 32 = 131,104 bytes (FFI-13 Bytes).

Refusal cases live under `core/front/cases/ffi/` in the existing case harness, with real
manifests.

| law | bug | oracle and setup |
|---|---|---|
| **FW1** a wrong module digest | a pin checked after parsing or not at all | The row names a file of exactly `abc` pinned to the empty string's SHA-256. The build is refused with `FFI-11: module F of wasm_laws@1/native does not match its pin: expected sha256 e3b0…b855, found sha256 ba78…15ad` (FIPS 180-2). With the true pin, it is refused as `not a core WebAssembly module`. |
| **FW2** a component, or a disabled proposal | a validator run with Wasmtime's default features | A component-layer file with its true pin is refused as not a core module. A module using one `i32.atomic.rmw.add` on a shared memory is refused with `not valid with the admitted features`. So are one `f32x4.relaxed_madd`, one `memory64` memory, and one `externref` table, which is valid WebAssembly 2.0 but needs `GC_TYPES` (FFI-11). |
| **FW3** an OS import | a module that reaches a clock, a file or randomness | A module importing `wasi_snapshot_preview1.path_open` is refused with `imports wasi_snapshot_preview1.path_open, which is not a null import`. So is `env.getenv`. `box.wasm`, which imports `clock_time_get`, is admitted, and `clock(f)` returns 52 (`ENOSYS`, WASI preview 1). |
| **FW4** a header that disagrees with its module | an export of the wrong type, or ABI text that does not bind | Exporting `hwl_fill` as `[i32] -> [i64]` is refused with `… exports hwl_fill as [i32] -> [i64], but its header gives [i32 i32] -> [i64]`. A module with no `hirsel_abi` section is refused with `has no ABI text`. A header with `waits` is refused with FFI-12's text. |
| **FW5** loans are copies | a loan passed by pointer, copied back after a trap, or measured by capacity | `b = list.with_capacity(16)` filled to 4 zeros: `fill(f, mut b)` returns 4, `list.len(b)` is 4, and every byte is `0xAB`. `c` is the bytes 0..11: `sum(f, list.slice(c, 4, 8))` returns 22. An empty `b` gives `fill` 0. With `d` of four zeros, `fill_trap(f, mut d)` fails with `Fail("contained trap: unreachable")`, and `d` is still four zeros. |
| **FW6** a loan area outside memory | a write past linear memory into host memory | In a fresh task, `set_area(f, 1)`, then `sum(f, b)` with a 16-byte `b` fails with `Fail("contained loan area out of bounds")`. Then `count(f)` returns 2 (`set_area` and itself): `sum` was never entered, and the instance was not discarded. |
| **FW7** a step bound around contained code | fuel not tied to `bound steps`, or tied to wall time | `bound steps 1000 { spin(f, 10⁹) }` returns `Error(OutOfTime(steps: 1000))`. `bound steps 10⁶ { spin(f, 10) }` returns `Ok(10)`. In one build, `spin(f, n)` for n = 10⁴ takes the same steps on 3 runs: read with a step bound searched by bisection, and compared only within the build. |
| **FW8** fuel and traps are deterministic per build | a count read from leftover fuel that varies by run at a trap | `trap(f, 7)` (integer division by zero) after a fixed prologue: the bisected smallest `n` with `bound steps n { trap(f, 7) }` failing with the trap, not `OutOfTime`, is the same on 3 runs of one build. |
| **FW9** slice ends inside a contained call | a contained call that holds a scheduler thread past a slice end | On a 1-thread scheduler, task A runs `bound steps 10⁹ { spin(f, 10⁸) }`, and task B prints a line and ends. B's line appears before A's result. With no bound current, the same order is not required. |
| **FW10** bytes charged to the program's limit | linear memory outside every limit, or growth past a refused admission | In a task that has already called `count(f)`, `bound bytes 2²⁰ { (grow(f, 32), grow(f, 1)) }` returns `Ok((-1, 2))`: 32 × 65,536 > 2²⁰ is refused and the guest sees −1, and one page is admitted, the old size being 2 pages. In a fresh task, `isolate { bound bytes 65536 { count(f) } }` returns `Ok(Error(causes))` with primary cause `Capacity`, because 2 initial pages and a 4-element table are 131,104 bytes. A following `count(f)` outside it returns 1, so no instance was made. A task that grows by 16 pages and ends returns its charge: its spawner's limit then admits `bound bytes` of what it had before the spawn. |
| **FW11** NaN bits | a non-canonical NaN reaching Hirsel or linear memory | `float.to_bits32(nan32(f))` is `0x7fc00000` (SEM-2): the guest returns `0x7fa00001` unchanged by any float operator, so the boundary rule (FFI-5) is what makes it canonical. `div_bits(f, 0.0, 0.0)` divides inside the guest by `f64.div` and returns `0x7ff8000000000000`, on x86-64 and on AArch64. x86-64's own NaN for that division is `0xfff8000000000000`, so the law is red without NaN canonicalization. |
| **FW12** the trap map | a trap that aborts the process, is caught as the wrong text, or leaves the instance live | For each `k` from 1 to 10 (rows 1 to 10 of FFI-14's table; row 10 is `proc_exit(7)`, so its `X` is `exit 7`), `isolate { trap(f, k) }` returns `Ok(Error(causes))` with primary cause `Fail("contained trap: X")` and the listed `X`. Then `count(f)` returns 1, because the instance was discarded and the new generation's counter starts again. |
| **FW13** a discarded instance's handles | a use of a handle into freed linear memory, or a release run on a discarded instance | `p = make(f)`, then `trap(f, 1)`, then `isolate { free(f, p) }` returns `Ok(Error(causes))` with `Fail("foreign instance discarded")`, and a second `isolate { free(f, p) }` gives `Fail("foreign handle released")`. An `ObjBox` holding a handle made before a trap, when dropped, reports its finalizer's cause `Fail("foreign instance discarded")`, and `count(f)` shows no entry into `free`. |
| **FW14** instances are per task | state shared between tasks, or a handle crossing tasks | Two tasks each call `count(f)` 3 times, and each sees 1, 2, 3. A refusal case declares `pub opaque resource type CellBox: send { f: Foreign[Box], p: foreign.Ptr[Cell] }` and is refused at the type (FFI-12, CONC-2). |
| **FW15** a resource's mechanical release | a contained handle that a dropped resource leaks or releases twice | `ObjBox { f, p }` with `drop(o) { let _ = free(o.f, o.p) }`. Make two boxes: `live` is 2. Drop one: `live` is 1. Destructure the other, call `free` on its handle, then drop nothing more: `live` is 0, and the test passes with no cause. A box whose finalizer fails (`fail("x")`) still leaves `live` one lower, through the mechanical release. |
| **FW16** guest stack overflow | a guest overflow that aborts the process | `recurse(f, 10⁷)` fails with `Fail("contained trap: stack overflow")`, and the run goes on to print a following line. |
| **FW17** replay of a contained program | a contained call that changes replay, or a contained test's pass reused | `hirsel test --replay` of a test calling `spin`, `fill`, `grow` under `bound bytes`, and `trap` inside `isolate` replays `Sealed`. `hirsel test` twice with one cache runs the test both times and reports no reuse: it takes `Foreign[Box]`, so PROG-4 never reuses its pass. A test that reaches both `wasm_laws` and `ffi_laws` is refused as in FL7. |
| **FW18** addresses and recipes | a recipe that does not pin the module, or an address that ignores it | Changing one byte of `box.c`'s output and its pin changes the caller's revision (FFI-16). `hirsel build --recipe R --code-store DIR` with the package deleted builds and runs `count`. A recipe with the module's object missing is refused as `Missing`. |
| **FW19** cross-compiled artifacts | a host-inferred CPU feature, or an artifact that differs by host | The `aarch64-unknown-linux-gnu` artifact precompiled on an x86-64 host and on an AArch64 host is byte-identical. The image built on x86-64 runs FW5 and FW11 on AArch64. |
| **FW20** invalid foreign module variant or fields | a manifest that ignores a foreign-module shape error | The typed manifest decoder refuses an unknown `ForeignModule` variant and a `Contained` row with `archives` in place of its `file` and `sha256` fields. |
| **FW21** a straight-line call past its bound | a return or trap taken with its stored fuel overdrawn | `bound steps 2 { straight(f) }` and `bound steps 2 { straight_trap(f) }` both return `Error(OutOfTime(steps: 2))`. After the call's own step, the entry starts with 17 fuel, passes its entry check and stores about 4,000 fuel consumed at `return` or `unreachable` (FFI-13 Bounds). `bound steps 300 { straight(f) }` returns `Ok(2000)`: 4,000 operators cost at most 4,016 fuel (the pinned cost table), 251 steps, plus the call's one step. |
| **FW22** a trapping call's charge | a trap charged the store's stale fuel, so a caller that catches it and calls again does quadratic work | Let `L` be the fuel of `loop_div(f, 10⁵)` up to its division, counted from `box_traps.s` with the pinned cost table. The bisected smallest `n` with `bound steps n { (isolate { loop_div(f, 10⁵) }, isolate { loop_div(f, 10⁵) }) }` returning two traps, not `OutOfTime`, lies between 2⌈`L` / 16⌉ and 2⌈`L` / 16⌉ + 1,030 (FFI-14 Fuel at a trap: each call is charged within one granule of its work). Charging the stale value puts it near ⌈`L` / 16⌉. |
| **FW23** a bulk operation under a bound | a dropped per-byte cost, or a bulk operation's check placed after it | `bound steps 100 { fill_mem(f, 65536) }` returns `Error(OutOfTime(steps: 100))`: one `memory.fill` of 65,536 bytes costs 65,536 fuel, 4,096 steps, and the entry starts with 16 × 99 + 1 = 1,585 fuel. `bound steps 10⁴ { fill_mem(f, 65536) }` returns `Ok(0xAB)`. |
| **FW24** `OutOfTime` discards the instance | a call stopped by its bound whose instance is entered again | `p = make(f)`, then `bound steps 1000 { spin(f, 10⁹) }` returns `Error(OutOfTime(steps: 1000))`. Then `count(f)` returns 1, and `isolate { free(f, p) }` returns `Ok(Error(causes))` with `Fail("foreign instance discarded")`. |
| **FW25** the pointer index is per instance | one pointer index shared by every instance, so equal addresses collide | Two tasks each call `make(f)` and get `Some(p)`, then `free(f, p)`, which returns 0. Both cells sit at the same address in their own instance (the pool's first cell), and neither `make` fails with `foreign pointer already held`. |
| **FW26** a handle's generation is checked | a handle of a discarded generation that releases a live cell at the same address | `p = make(f)`, then `isolate { trap(f, 1) }`, then `q = make(f)`, which is `Some` at `p`'s address in the new generation. `isolate { free(f, p) }` returns `Ok(Error(causes))` with `Fail("foreign instance discarded")`, and `live(f)` then returns 1: `q`'s cell is still in use. |
| **FW27** a guest overflow replays | a stack overflow point that depends on the run, not the build | `hirsel test --replay` of a test that runs `isolate { recurse(f, 10⁷) }` under `bound steps 10⁹`, beside a task that prints a line, on a 2-thread scheduler, replays `Sealed`: the steps at the overflow, and so the slice ends around it, are those of the recording. |
| **FW28** Wasmtime's signal handlers beside the runtime's | a guest fault taken as a host crash, or a Hirsel stack overflow taken as a guest trap | On a 2-thread scheduler, task A runs `isolate { trap(f, 2) }` 1,000 times and prints each primary cause, while task B recurses in Hirsel with no bound. Every line A prints is `contained trap: memory out of bounds`, and B's unbounded recursion fails with `Stack` (FAIL-17), ending the run with status 101 and a first stderr line beginning `Stack at`, not a signal. |
| **FW29** fuel exhaustion without a bound | a guest fuel trap that aborts the process | On the slow tier, with no bound current, `isolate { tgrow(f, 2^33) }` exhausts Wasmtime's `2^64 − 1` fuel in about ten minutes (each refused `table.grow` is charged `2^32 − 1`). Its isolate returns the primary cause `Fail("contained trap: out of fuel")` and the program prints a line after it. |
| **FW30** host memory refusal in contained code | an instantiation `mmap`/`mprotect` `ENOMEM` or `OutOfMemory` error that aborts in `trap_text` instead of stopping as `HostMemory` | Under `RLIMIT_AS` = 2 GiB (below Wasmtime's 4 GiB + 64 MiB linear memory reservation), a program prints a line and calls `count(f)`. Under `RLIMIT_DATA` = 96 MiB, it runs one task that calls `count(f)`, so its slot is cached, and then a task under a 192 MiB grant whose instance takes that slot and calls `grow(f, 2048)`. Each stops with status 102 and `run stop: HostMemory` (FFI-13 Bytes, FAIL-15), not `SIGABRT`. |
| **FW31** a reused linear-memory slot | a cached slot that keeps the previous instance's bytes, its size or its open pages, or that charges a grow against the previous grant | A task under 4 MiB calls `count(f)`, `grow(f, 16)` and `fill_mem(f, 1114112)`, which reads back `0xAB` at page 1. The next task, under `bound bytes 196640` (its initial 131,104 bytes plus one page), takes that slot and reads `count(f)` = 1, `fill_mem(f, 0)` = 0 and `grow(f, 0)` = 2 pages, then `grow(f, 1)` = 2 and `grow(f, 1)` = −1. |
