# hirsel-cli

The command line exposes check, native run/test/build, package creation, formatting and JSON through the compiler driver. Invalid arguments exit 2. Launch and native tool failures exit 3. A program failure retains its output and uses the shared program exit policy.

`hirsel run <source|manifest.json>` compiles with Cranelift and launches the native executable. `--backend llvm` selects LLVM. `hirsel test <source|manifest.json> [filter...]` compiles matching tests with LLVM. `--backend cranelift` selects Cranelift. A filter matching no tests fails. Both commands use the same checker and native runtime as `build`.

A run without a launch record uses empty grants. `--grants grants.json` supplies a grants template, and `--run run.json` supplies a bound run record. Trailing arguments after `--` replace template Args. They cannot replace a bound record's Args snapshot. Host admission checks the selected program and its authority before entry. Native run and test accept no simulation, interpreter step budget, race instrumentation or simulation-point options.

`hirsel build [--release | --debug] [--backend llvm|cranelift] <source|manifest.json> -o <exe>` produces a standalone executable. Default builds use LLVM; debug builds default to Cranelift. Release requires LLVM. Executables accept `--hirsel-grants` or `--hirsel-run` at launch.

`hirsel check [--json | --safe] <source|manifest.json>` checks without execution. `hirsel fmt [--check] <source|manifest.json>` formats direct sources. `hirsel new <dir> [--lib]` creates a package with a test and explicit grants. `hirsel precompute <manifest.json> -o <directory>` produces installed package products. `hirsel env --json` reports the selected product location.

`hirsel --help`, `hirsel help <command>` and `hirsel <command> --help` print usage without reading a source. `--timings[=json]` and `--mem-usage` report compiler costs on stderr. Build and pool commands are documented in [building.md](../../docs/building.md).

The interpreter and its Value/host adapters remain internal evaluators and differential-test oracles. They are not launch engines of this command line. Proof and observation commands remain owned by their separate deletion lane.
