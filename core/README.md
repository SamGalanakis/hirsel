# Core milestone corpus

These nine programs define the M1 runtime workloads before the new compiler exists.
Hirsel packages live under `programs/m1/<workload>/`. Each `main.hirsel` is the
idiomatic entry; `matched.hirsel`, where present, is the matched entry. Sources use
the canonical declarations in `core/` and the seed API in `std/`; shared fixture
I/O, explicit Capacity conversion and resource traces live in `programs/m1/support.hirsel`.
`programs/m1/manifest.json` maps packages, modules and exact dependencies for the
real parser/resolver. Each package uses `charge-v1`. Per-workload `DERIVATION.md`
contains the hand-check output and named-site charge calculation. These programs
cannot execute until the new compiler's memory, task and native slices land.

Each `corpus/m1/<workload>/SPEC.md` defines its algorithm, inputs, sizes, failure sites,
observable output and small hand check. `inputs/` and `EXPECTED/` are committed fixtures;
`cases.json` selects the input and variant for each case. Exit codes are 0 for success,
2 for domain or I/O errors, 3 for Capacity and 4 for cancellation. Stderr is empty.
Cleanup events appear before the final `failure=` line. Resources close exactly once.
No depth limit, typed count-limit error, copy-on-write or shared mutex is part of M1.

`tools/reference.py` is an independent Python oracle. It uses Python's JSON decoder,
OrderedDict, complete BFS paths, value arrays and byte-origin sets rather than the Rust
storage implementations. Running it without flags verifies the committed inputs and
expected outputs. `--write` deliberately changes those files; it never runs Rust or
Hirsel. Each SPEC states the derivation of every input and expected file by case family.
Checksums are unsigned sums of UTF-8 output bytes, without modulo reduction. They are
known-answer checks, not collision-resistant integrity checks. Small cases also show
full values, paths or pretty output. Arithmetic is checked in both Rust profiles;
only the matched map's private hash uses explicit wrapping arithmetic.

The Rust workspace is `twins/`. Every workload has an idiomatic and a matched binary.
Shared `program.rs` supplies the same parsing, control flow and instrumentation;
layout-specific files supply the storage implementation. `files` deliberately has
identical twins, since three OS handles have no alternative payload layout. The
persistent map uses Arc; text uses task-local Rc. No counted storage is used in the
other idiomatic payloads. A named Capacity injection occurs before the named allocation
or commit. It models portable admission, not a host allocator OOM or a guess at future
canonical byte charges. Ordinary Rust allocation exhaustion remains environmental.

Build locally with the Cargo shim on PATH, using the pinned toolchain:

```sh
cargo +1.99.0 build --manifest-path core/twins/Cargo.toml --locked --offline --workspace --all-targets --target x86_64-unknown-linux-gnu --target-dir /tmp/core-twins
python3 core/tools/reference.py
python3 core/tools/verify.py --target-dir /tmp/core-twins
cargo +1.99.0 build --manifest-path core/twins/Cargo.toml --locked --offline --workspace --all-targets --release --target x86_64-unknown-linux-gnu --target-dir /tmp/core-twins
python3 core/tools/verify.py --target-dir /tmp/core-twins --profile release
```

The CI workflow runs these output comparisons in both profiles. It does not rely on
Rust's empty unit-test harnesses. `MEASURE.md` defines the ratio protocol and the
committed Rust numbers. `SPEC-COMPARISON.md` lists the gaps in the spec draft.

To run the new-source acceptance gate once:

```sh
tools/test-filter //:hirsel-core-front-unittest -- tests::m1_program_manifest --exact --nocapture
```

It fails on a parse or resolve refusal, a formatter refusal or non-idempotence.
It also visits the real declaration/body checker and prints every current refusal
and implementation gap. Manifest `checker: "report"` makes that slice diagnostic;
change it to `"require"` when those checker constructs are implemented. This is not
native execution evidence. The manifest includes all seventeen available entries;
JSON's matched blob entry awaits the compact-sequence rules and separate fixture change.
`files` shares one implementation across its two entries, as its Rust twins do.
The host outcome contract records the required mapping from `support.Error.code`
to 2, 3 or 4. PROG-3 itself defines an error outcome and does not specify OS exit
status or error rendering; that mapping still needs driver support.

Matched rope and versions keep private slab indices and explicit edge counts in
one owning store. No index escapes the store, and roots are explicitly released
at the specified observation boundaries. Their fallible staging rolls back new
edges. The idiomatic variants use Rc and Arc and count reachable identities for
the same liveness observations. Every DERIVATION includes both representations'
charges and the independent small expected output.
