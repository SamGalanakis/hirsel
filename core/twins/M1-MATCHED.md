# Corrected M1 matched Rust samples

All nine matched roots were audited against the corresponding `core/programs/m1/*/matched.hirsel`; the detailed site table is in the lane brief's `M1-PARITY.md`. Idiomatic twins, Hirsel programs and EXPECTED are unchanged. Independent review is pending. The orchestrator will collect the unchanged Hirsel sides at the M2 measurement commit. These Rust counts are ready for that pairing; no result ratios are published here.

232/232 observations match exact EXPECTED stdout, stderr, exit and file changes: 58 applicable cases, both profiles, cold/warm. Each is one sample. A separate JSON packed-layout/duplicate/surrogate/invalid-number law passed. All 18 final builds' rustc dep-info source sets match `core/tools/measure.py:sources`, including the new helper/parser/sort modules. The source-line denominator includes exactly these modules.

The table shows normal warm instruction counts and peak process RSS in KiB. Both profiles use pinned Rust1.99.0, x86-64, overflow checks, CGU1, no incremental/debug info/LTO; dev opt0/debug assertions and release opt3/no debug assertions. Full flags, binary/source hashes, rustc version and hash, helper artifact identities, all cases and cache labels are in `M1-MATCHED-ROWS.tsv` and `M1-MATCHED-ENVIRONMENT.json`. Hirsel artifact identities describe the checked reference toolchain; these rows execute Rust only.

| Program | Dev instructions / RSS KiB | Release instructions / RSS KiB | Rust compiler SHA256 | Reference compiler profile/hash; runtime profile/hash |
|---|---:|---:|---|---|
| bfs | 470,837,848 / 3072 | 81,625,450 / 3072 | f3834d26669b | release/cf14072791ca; shipping-release-fat-lto/626ce1bd4195 |
| generics | 132,715,652 / 3840 | 53,303,233 / 3840 | f3834d26669b | release/cf14072791ca; shipping-release-fat-lto/626ce1bd4195 |
| lru | 194,632,204 / 2304 | 37,298,460 / 2304 | f3834d26669b | release/cf14072791ca; shipping-release-fat-lto/626ce1bd4195 |
| files | 7,135,912 / 2560 | 1,432,651 / 2304 | f3834d26669b | release/cf14072791ca; shipping-release-fat-lto/626ce1bd4195 |
| json | 338,534,536 / 2304 | 40,096,431 / 2304 | f3834d26669b | release/cf14072791ca; shipping-release-fat-lto/626ce1bd4195 |
| pipeline | 61,440,781 / 2816 | 10,814,840 / 2560 | f3834d26669b | release/cf14072791ca; shipping-release-fat-lto/626ce1bd4195 |
| request-scratch | 97,214,469 / 2560 | 35,840,051 / 2560 | f3834d26669b | release/cf14072791ca; shipping-release-fat-lto/626ce1bd4195 |
| rope | 55,898,420 / 2560 | 12,281,906 / 2304 | f3834d26669b | release/cf14072791ca; shipping-release-fat-lto/626ce1bd4195 |
| versions | 8,633,335 / 2048 | 2,900,797 / 1792 | f3834d26669b | release/cf14072791ca; shipping-release-fat-lto/626ce1bd4195 |

The safe provider adaptations remain explicit: Files initializes its 4096-byte read buffer and cannot observe real close errors through safe File drop; Pipeline models pending cancellation outcomes/custody through gates because std channels have no cancellation API. LRU/request use Rust HashMap for Hirsel Map, so internal hashing/probing differ. Named corpus capacity refusals match source ordering; Rust does not reproduce Hirsel's ambient root funding. The independent review must assess these limits, rather than treating passing output checks as proof of complete source parity.

Reproduce with pinned rustc and the flags in each row, then run `core/corpus/m1/<program>/cases.json` in private directories. Every status, stdout/stderr and changed/deleted file must equal the committed EXPECTED. Measurement uses process-tree `perf stat -e instructions:u,duration_time`, GNU time and `tools/capped` at2GiB on CPU16, with no host page-cache flush. The lane's `m1-parity/collect.py` preserves the exact commands/raw observations outside git.
