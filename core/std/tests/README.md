# Native standard library contracts

Each hand-derived oracle is a comment before its private function:

    // expect: run example() = 7
    fn example()->u64 { ... }

The walker uses the real driver product path, Cranelift dev and LLVM release.
It prints every selected case as PASS, FAIL or NOT RUN with its first cause and
owner. Zero matches fail; a missing native run never counts as passing.

    python3 core/tools/std_native.py --compiler <hirsel-core> --runtime <core-rt.a>         --filter file --backend both --report /tmp/file-native.json

A filter is a module/function prefix (file, file:read) or source path prefix.
Repeat --filter for several prefixes. The compiler and runtime are Buck targets
hirsel-core-driver-bin-hirsel-core and hirsel-core-rt[staticlib].

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
test/helper functions; type/const/pub additions, duplicate names and names already
in the original module are refused. A reason comment is required. Use
--filter pool_internal[:function] for a file named pool_internal.hirsel.

Marked files are test-build inputs only. They are excluded from the ordinary
std_seed loader; published std, the cost corpora, M1 and LangArena load library
files only. Unmarked tests keep their tests.<stem> module and public-only access.
No production test hook or language visibility exception is introduced.
