# Upstream Rust twin

Sources: kostya/LangArena 3c9f84ddaeec834c0631747cb789e8cfd602f993.
The source files and test/run configurations are unchanged. Cargo profiles add
the fixed checked-arithmetic settings from core/MEASURE.md. Dependencies use
the supplied upstream lockfile. Run `core/langarena/rust/vendor.sh` once to
populate `core/langarena/rust/vendor/`, which is ignored by git. Build from
this directory with `rustup run 1.99.0 cargo build --offline --locked`,
or add `--release`. Upstream uses edition 2021; its unchanged patterns do
not compile under edition 2024. Matched twins use edition 2024.
The measurement runner checks the vendor files against the locked checksums
and refuses an absent or changed dependency before measuring.

The task named Hash::SHA256 uses upstream SimpleSHA256, an eight-lane rolling
hash. It is not a cryptographic SHA-256 implementation. The Hirsel task must
preserve that algorithm to match the upstream checksum.

The numeric run configurations use the default 64 MiB logical root except:

- Binarytrees::Obj, root: 128 MiB (default 64).
- Binarytrees::Arena, root: 256 MiB (default 64).

Pass the documented driver limit through `run.py --limit 134217728` or
`--limit 268435456`, equivalently `--root-mib 128` or `--root-mib 256`. These are the smallest admitting powers of two for the
unchanged upstream inputs on both engines. Preserve the default-root refusal;
report peak charge and peak RSS beside each Rust twin. A larger root funds live
data and simultaneous old/new growth blocks, never unreclaimed dead storage.

After all task sources are available, `run.py --build-only --suite --module suite`
generates one dispatcher reaching all 50 tasks for the whole-suite idiomatic
comparison. Supply the compiler, runtime, backend, upstream binary and external
output directory as for a runtime sample. Dependency crates remain prebuilt.
Build reports list source sets and count only loaded source packages in each
per-line denominator, matching the driver's reported file and byte counts.
`--resume` reuses completed samples; changed root funding preserves Rust samples
and runs only the missing Hirsel side.

Result measurements require a release compiler and the shipping fat-LTO runtime.
Build them with:

    python3 core/tools/measurement_artifacts.py > /tmp/measurement-artifacts.json

Pass `--compiler-profile release` and the runtime archive reported by the
configured shipping target, or its `hirsel-core-rt-archives/` view. The runner
refuses another archive before creating a report directory. `--diagnostic`
explicitly permits other toolchains; use `--compiler-profile checked
--runtime-profile checked` to label those observations. Every report records
both profiles and SHA-256 identities. Diagnostic ratios are excluded from results.
Changing either identity invalidates only the Hirsel side of `--resume`; correct
Rust samples remain reusable. `python3 core/tools/test_measurement_artifacts.py` exercises the
profile refusal, shipping view, explicit diagnostic and archive-change controls.
