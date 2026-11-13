# Core and standard library contracts

Each hand-derived oracle is a comment before its private function:

    // expect: run example() = 7
    fn example()->u64 { ... }

The walker uses the real driver product path, Cranelift dev and LLVM release.
It prints every selected case as PASS, FAIL, NOT RUN or NOT APPLICABLE with its first cause and
owner. Zero matches fail; a missing native run never counts as passing.

    python3 core/tools/std_native.py --compiler <hirsel-core> --runtime <core-rt.a>         --filter file --backend both --report /tmp/file-native.json

The same walker discovers core/core/tests and core/std/tests. A filter is a module/function prefix (file, file:read, core.list, core.list:law) or source path prefix. Existing std filters retain their spelling; std.file also works.
Repeat --filter for several prefixes. The compiler and runtime are Buck targets
hirsel-core-driver-bin-hirsel-core and the shipping hirsel-core-rt-archives.
Use core/tools/measurement_artifacts.py to locate the release compiler and archive.

No-argument scalar tests, f(dir), f(out) and f(err) are supported. Dir tests receive
a fresh empty confined host directory and run locally on a host with openat2,
Linux >= 5.6 and a seccomp profile permitting the syscall. Expected failure syntax
is "= fail Bounds" (or Fail, Overflow etc); the cause and process status are checked.

A stream contract can state exact output before its run oracle:

    // host: first_write 1
    // expect: stdout "éx"
    // expect: run flush_suffix(out) = true

This scripted host commits the first byte of the first stream write, then returns
Other(5), written=1. Subsequent writes use the production provider. It tests
arbitrary byte suffixes, including partial UTF-8 scalars, without timing races.
"// host: first_write 2 file" similarly commits two file bytes before the error.
"// host: remove_tree_prefix a" removes exactly child a, then fails; remaining
children stay intact. These scripts exist only in the native test link.

Private-state tests may use this FIRST LINE:

    // test-of: std.pool
    // test-reason: public APIs cannot seed a private slot at MAX generation
    // expect: run retirement_at_max() = true
    fn retirement_at_max()->bool { ... }

For that case only, the walker concatenates pool.hirsel and the marked test source
as ONE module and builds its native adapter there. The file may add only private
test/helper functions with bodies; type/const/pub additions, duplicate names and names already
in the original module are refused. A reason comment is required. Use
--filter pool_internal[:function] for a file named pool_internal.hirsel.

Marked files are test-build inputs only. They are excluded from the ordinary
std_seed loader; published std, the cost corpora, M1 and LangArena load library
files only. Unmarked tests keep their tests.<stem> module and public-only access.
No production test hook or language visibility exception is introduced.

Core collection and kernel laws live in `core/core/tests/*.hirsel`. A core private
law uses the same FIRST-line marker, with its core module:

    // test-of: core.deque
    // test-reason: a hole state cannot be constructed through the public API
    // expect: run hole_round_trip() = true
    fn hole_round_trip()->bool { ... }

The walker splices `core/core/deque.hirsel` and this file into one temporary core
module. The same private-function, collision and reason rules apply. An unmarked
core test stays a separate `core.tests.<stem>` module and sees only public library
items. For native execution a temporary application entry calls the core test's
adapter, so the ordinary published-core body path is exercised. No test declaration
is added to the installed product. Recursive core directory discovery excludes its
root tests subtree; bundled products, std seed, M1 library rows and LangArena use
library sources only. Real library source refusals still fail those paths.

Build the additional test backend `hirsel-core-contract-eval-bin-hirsel-core-contract-eval`
when evaluator-only laws are selected. This helper checks and assembles a single
source entry with the existing front test assembly, then evaluates it; selection,
expectations and reports remain in this walker. Example invocation:

    python3 core/tools/std_native.py --compiler <hirsel-core> --runtime <core-rt.a> --evaluator <hirsel-core-contract-eval> --filter core.deque --backend both --report /tmp/deque-laws.json

A whole test file may declare evaluator-only laws:

    // test-of: core.deque
    // test-backend: evaluator
    // test-reason: native slot operations deliberately have no misuse guards
    // expect: run take_empty() = fault "take of the empty slot at +0"
    fn take_empty()->u64 { ... }

`fault` requires an exact JSON string and is permitted only with the evaluator
marker. The ordinary `= true`, `= 7` and `= fail Bounds` oracles also work there.
Evaluator-only laws take no host arguments. A matching evaluator fault is a run;
a front refusal, unavailable body or backend crash is not a matching fault.
The report contains one evaluator result and explicit `NOT APPLICABLE` rows for
each selected native engine, with the reason. Those rows have `passed: null`,
`applicable: false`, and count as neither passing nor unavailable native tests.
A missing evaluator backend or empty filter is an error. There is no native skip
marker and no retry.

For every ordinary law with no host-capability argument, select the evaluator
as a third backend. `all` runs evaluator, Cranelift dev and LLVM release; `both`
continues to mean the two native engines. Evaluator-only selection needs neither
a compiler nor a runtime archive:

    python3 core/tools/std_native.py --backend evaluator --evaluator <hirsel-core-contract-eval> --report /tmp/evaluator-laws.json
    python3 core/tools/std_native.py --backend all --compiler <hirsel-core> --runtime <shipping-core-rt.a> --evaluator <hirsel-core-contract-eval> --filter core.walker_finalizer

The evaluator uses the same source adapter and exact return/output/failure oracle,
including private same-module laws. Its demand walk includes type finalizers and
their callees. It runs the form's checked evaluator, including loan modes. A
backend crash or checking refusal is NOT RUN, never an expected model fault.
Every capability-taking law has an explicit evaluator NOT APPLICABLE row with
`native_only: true`, `ran: false`, and `passed: null`. Each backend prints selected,
ran, passed, not-run, not-applicable, native-only counts and elapsed seconds.
Evaluator disagreements remain failing findings, routed to core-form-883 for
evaluation or core-check-895 for checking/assembly. Do not exclude them. The
existing evaluator-only fault marker and automatic execution on native selections
are unchanged. Filters that match nothing fail on every backend.
