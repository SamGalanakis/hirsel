# Core milestone corpus

These nine programs define the M1 runtime workloads before the new compiler exists.
The corpus does not contain Hirsel source. The spec lane owns that source.

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
