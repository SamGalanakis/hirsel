# LangArena port interface

The reference is kostya/LangArena commit 3c9f84d. A port uses the unchanged
upstream JSON task row, including its checksum, parameters, iterations and optional
warmup_iterations. The default warmup count is max(iterations / 5, 1).

Put a task group in core/langarena/<module>.hirsel. Export a dispatcher with:

    pub fn dispatch(config:String,name:String,warmup:u64,iterations:u64)
        -> Result[u32,helper.Error]

Use direct if/match dispatch, not a stored table of callable constants. Prepare
the benchmark once, run warmups and measured iterations on the same benchmark
state, and return its upstream checksum. The upstream driver resets the random
generator before prepare and again after warmup; use an explicit helper.Lcg
created/reset at those same points. Do not reset accumulated results unless
the upstream task does.

The port's entry is:

    pub fn main(out:Stdout,d:Dir,argv:Args)->Result[(),Exit] waits {
        harness.main(out,d,argv,fn(c:String,n:String,w:u64,i:u64)
            ->Result[u32,helper.Error] {dispatch(c,n,w,i)})
    }

A port that spawns or joins tasks uses `harness.main_wait`. Its callback has the
same arguments and result as the ordinary dispatcher, with `waits` on its type.
The entry borrows its Tasks capability in the scoped closure:

    pub fn main(tasks:Tasks,out:Stdout,d:Dir,argv:Args)->Result[(),Exit] waits {
        harness.main_wait(out,d,argv,
            fn(c:String,n:String,w:u64,i:u64)->Result[u32,helper.Error] waits {
                dispatch_wait(tasks,c,n,w,i)
            })
    }

Here `dispatch_wait(tasks:Tasks,config:String,name:String,warmup:u64,iterations:u64)`
returns `Result[u32,helper.Error] waits`. Both harness entries call the same private
config reader and driver. The existing non-waiting entry and ports need no change.
The correctness runner grants Tasks for either entry.

The command-line arguments are a config filename relative to the granted Dir,
then the exact task name. The harness reads the config, obtains counts and expected
checksum, calls the dispatcher, and prints:

    Base64::Encode checksum=3894061047 expected=3894061047 status=OK

A mismatch prints status=ERR with both values and returns exit status 1. Parse,
input and unknown-task errors return nonzero. No missing task is a passing run.

Build the compiler and runtime with the repository Buck targets
hirsel-core-driver-bin-hirsel-core and hirsel-core-rt[staticlib], using
--show-json-output. Then run the bootstrap on Cranelift dev and LLVM release:

    python3 core/tools/langarena.py --compiler <compiler> --runtime <archive>         --config core/langarena/tests/base64-test.json         --module tests/bootstrap --task Base64::Encode --backend both         --report /tmp/base64-native.json

For a new port change --module and --task; --config may be the complete unchanged
upstream test.js or run.js, array or object form. The Python tool builds through
product.build, uses a fresh confined host directory and checks the exact printed
checksum and process status. --helper-laws builds the bootstrap's independent
LCG/checksum controls. This is a correctness entry; slice F owns the performance
runner core/langarena/run.py and the vendored Rust twin.

tests/bootstrap.hirsel is the initial Base64::Encode witness, not the owner of the
full base64 port. Size30 encodes to40 bytes; 8192 iterations and1638 warmups add
40*9830=393200 to its u32 result. DJB2 of
"encode aaaa... to YWFh...: 393200" is3894061047, the upstream test checksum.

Use `helper.checksum_f64(x)` for upstream `checksum_f64`: it formats exactly
seven decimal places through `float.push_fixed64`, then applies `checksum_str`.
It returns `Result[u32,helper.Error]` and propagates formatting Capacity. The
bootstrap helper laws check independently derived hashes for zero, positive,
negative and fractional values, plus Capacity under a zero-byte bound.

The waiting control in `tests/waiting.hirsel` spawns one child and joins it. The
configured one warmup plus forty iterations supplies 41; the child returns 42.
Run it with the same compiler/runtime arguments as above:

    python3 core/tools/langarena.py --compiler <compiler> --runtime <archive> --module tests/waiting --task Task::Join --config core/langarena/tests/waiting-test.json --backend both --report /tmp/wait.json

Its negative control uses `waiting-mismatch.json` and `--expect-mismatch 42`.
That option requires the exact `checksum=42 expected=41 status=ERR` line and exit
status 1; an arbitrary failure cannot pass the control. Ordinary runs continue to
require the configured checksum, status OK and exit 0.

## Measurement

Runtime comparisons use each task's unchanged upstream `run` configuration on
Cranelift dev versus Rust opt-level 0 and LLVM release versus Rust opt-level 3.
Report upstream and independently reviewed matched twins separately, with one
sample per side, instructions and peak RSS, following `core/MEASURE.md`.
Both the compiler and runtime archive come from the release Buck configuration:
`--target-platforms //tools/buck2:release`. Record their build configuration and
artifact identities. The generated program's `--release` flag alone does not
select a release compiler or runtime. Checked or unverified artifact counters and
profiles are diagnostics; they produce no result ratio. Recollection replaces only
the invalid Hirsel samples and reuses valid Rust samples.
A missing counter, wrong checksum, Capacity or incomplete run produces a blocked
row and no ratio. Ratios above 2 include a period-50003 instruction profile.

Matched build and check comparisons run per module against its matching Rust
root. Report fresh, unchanged reuse and one-body-edit counters, total instructions
and instructions per source line, with the source sets and cache behavior visible.
Several tasks in one module share that module's build/check row.

There is no per-task idiomatic build ratio: unchanged upstream `main.rs` builds
the whole suite. Each task records `idiomatic build: whole-suite only`. After all
50 Hirsel tasks are on main, the numeric lane runs one whole-suite build comparison:
all Hirsel modules in one build against the unchanged upstream Rust tree and its
locked, vendored dependencies. Report totals and instructions per source line,
stating dependency crate lines separately so the denominator is visible. This
whole-suite comparison does not replace the per-task runtime comparisons.

A task proven to exceed the default root with live data uses the driver's documented
`--limit <bytes>` build setting. Choose the smallest power-of-two root that admits
the unchanged input on both engines and record it beside the task's measurements.
Graph::BFS, Graph::DFS and Graph::AStar use root: 128 MiB (default 64).
Maze::Generator, Maze::BFS and Maze::AStar use root: 256 MiB (default 64); their
live cell construction also exceeds 128 MiB. These roots admit the unchanged
upstream run on both engines. Preserve the smaller-root Capacity results.
CacheSimulation uses the default 64 MiB root; Map compaction reclaims its dead slots.
Report peak charged bytes, both sides' peak RSS and their RSS ratio alongside the
instruction ratio; unavailable charge telemetry is explicitly unavailable. When
Hirsel RSS exceeds Rust by about 1.5 times, identify the retained representation or
capacity overhead for the collection and layout owners.

Peak root reservation is collected separately with the documented
`--logical-charge-report <file>` diagnostic build and the optional
`//:hirsel-core-rt-observed-archives` runtime. Record `observation_mode: true`
and the diagnostic compiler/runtime identities. MEM-19 reservation includes
bounds, task reservations, finalizer grants and startup; it excludes physical
chunk slack and engine bookkeeping. It is distinct from peak RSS. Observed
execution counters and timings never enter performance ratios or geomeans.
