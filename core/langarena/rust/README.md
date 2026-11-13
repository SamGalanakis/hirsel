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
