# SHA-256 comparison

`bench.hirsel` and `core/tools/sha256_oracle.rs` hash an evolving byte image.
The two arguments are input size and iteration count. Each iteration starts a
new hash, then stores digest byte zero into input byte zero, preventing hoisting.
Only the final byte is printed. Inputs are `(17*i + 3) mod 256`.

Build the Rust partner and check every committed differential vector with:

```
python3 core/tools/sha256_reference.py --out /tmp/sha256-reference
```

This uses the pinned Rust compiler and the workspace's Buck `sha2` library with
default backends: SHA-NI when the CPU has it, selected at run time. Hirsel's
runtime makes the same choice, so both sides use the CPU's SHA-256 instructions
where they exist. Rust is built at opt-level 3. Its executable uses `-no-pie` for Buck's static rlibs;
startup is included in both samples and amortized over 100,000 hashes.

Build the Hirsel entry as `bench@1/bench/main`, supplying this source as module
`bench` of package `bench@1`, with a dependency on `std@1` and the canonical
core/std sources. Use LLVM release, the release compiler, the shipping runtime,
and a Stdout grant. The contract walker in `core/tools/std_native.py` also
supplies examples of source manifests and native toolchain arguments.

For each binary, run one serial sample at 64 bytes and 4,096 bytes:

```
HIRSEL_MEMORY_LIMIT=2G taskset -c 16 tools/capped perf stat -e instructions:u -- BINARY SIZE 100000
```

Require matching outputs. Divide instructions by `SIZE * 100000` and compare
Hirsel with Rust at each size. `HIRSEL_SHA256=portable` measures Hirsel's
portable compression instead. Counts include process setup and loop overhead,
and exclude compilation.

Run the SHA-256 laws with the existing contract walker, `--filter sha256
--backend all`, and the compiler, runtime, evaluator and pinned Clang paths.
These laws enter the evaluator law suite and both native sweep suites through
ordinary test discovery.
