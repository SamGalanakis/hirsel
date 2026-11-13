# LAYOUT and arena peak memory

All 13 LAYOUT observed LLVM runs and all 12 arena contracts on each engine pass.
All 12 Rust heap runs pass their unchanged checksums. These are separate diagnostic
runs, with no instruction or RSS recollection. Shipping columns below are retained.

H peak charge is `peak_root_reserved_bytes`, MEM-19 root reservations in bytes,
including 288 bytes of startup and unused bound/builder/task reservations. It excludes
host allocation, physical chunk slack and engine bookkeeping. Rust peak heap is
Massif's requested live heap, including process startup, with `--heap-admin=0`,
`--stacks=no`, `--peak-inaccuracy=0`, `--time-unit=B`. This ratio compares portable
reservations with requested native heap; it is not an RSS ratio. Rust node padding
counts in its heap, while Hirsel's logical charge does not count native padding.

Peaks use source `ab2dbc2a9556`, the unchanged workloads/inputs and reviewed Rust twins.
Observed artifact identities, diagnostic only:

- `compiler_build_profile`: `release`; `compiler_sha256`: `0914b23f7bf01c051d4a8db8ecd0d3b8f97398666e0d6314b31092909c708895`.
- `runtime_archive_profile`: `charge-observation`; `runtime_archive_sha256`: `a93a938698fe0ff344a08ef054e4b989fa074dc4f983d470bd30c036c9b2affd`.

Rust remains rustc 1.99.0, x86-64, opt-level 3, checked arithmetic, one CGU, no LTO.
Observed instructions include sampling overhead and enter no performance comparison.
The instruction/RSS columns use these earlier shipping identities for every row:

Compiler Buck release sha256650eb4ff52a3578946e4318b96de8d2c13ef811650029b10eb9741c972f20176; runtime shipping-release-fat-lto sha25631750a97151c00750b8bd3274ced40c5bfb59cfc42aada9466fa4fa02701a054. Release Buck command and native identities retained in shipping-build.log and shipping-runtime/*.build.stdout. Source2c67c7f2 plus test/runner-only89ff478e, one replacement Hirsel sample per phase. Reused unchanged valid Rust samples. CPU16,2GiB, instructions:u, peak RSS KiB.

| phase | Hirsel I / RSS KiB | H peak charge B | matched Rust I / RSS KiB | Rust peak heap B | charge / heap | H/R I | serde I / RSS KiB | serde peak heap B | charge / serde heap | H/serde I |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| ordinary | 193,849,210 / 2304 | 368,896 | 156,948,240 / 2304 | 359,604 | 1.026 | 1.235 | n/a | n/a | n/a | n/a |
| sized | 114,753,599 / 1792 | 162,760 | 68,996,955 / 2048 | 163,000 | 0.999 | 1.663 | n/a | n/a | n/a | n/a |
| streaming | 103,224,554 / 1792 | 524,608 | 59,559,859 / 2048 | 524,853 | 1.000 | 1.733 | n/a | n/a | n/a | n/a |
| exclusive | 3,190,553 / 1792 | 98,592 | 3,004,259 / 1792 | 131,629 | 0.749 | 1.062 | n/a | n/a | n/a | n/a |
| churn | 9,326,749 / 1792 | 213,280 | 6,472,047 / 2048 | 229,929 | 0.928 | 1.441 | n/a | n/a | n/a | n/a |
| rc | 5,912,866 / 2304 | 273,664 | 4,113,017 / 2304 | 273,926 | 0.999 | 1.438 | n/a | n/a | n/a | n/a |
| arc | 6,410,805 / 2304 | 273,664 | 4,096,545 / 2304 | 273,927 | 0.999 | 1.565 | n/a | n/a | n/a | n/a |
| json_parse | 423,297,897 / 2048 | 341,848 | 279,727,598 / 2048 | 310,946 | 1.099 | 1.513 | 202,735,758 / 2560 | 743,431 | 0.460 | 2.088 |
| json_print | 130,051,246 / 2304 | 538,456 | 99,949,397 / 2560 | 442,018 | 1.218 | 1.301 | 85,547,504 / 2816 | 874,503 | 0.616 | 1.520 |
| json_reject | 730,849 / 1792 | 4,392 | control | n/a | n/a | n/a | n/a | n/a | n/a | n/a |
| json_capacity | 8,873,641 / 2048 | 196,896 | control | n/a | n/a | n/a | n/a | n/a | n/a | n/a |
| charges | 32,611,873 / 2304 | 273,664 | control | n/a | n/a | n/a | n/a | n/a | n/a | n/a |
| lifetime | 23,190,959 / 2304 | 273,664 | 15,906,157 / 2304 | 273,932 | 0.999 | 1.458 | n/a | n/a | n/a | n/a |

No measured row exceeds 1.5 times either Rust twin's peak heap. The largest matched
ratio is JSON print, 1.218. Its 538,456-byte reservation consists of 131,072 bytes of
owned input, the 210,488-byte compact document, 196,608 bytes of coexisting old/new
output buffers during growth, and 288 bytes of startup. Rust's realloc accounts for
one requested buffer at that point. Rc/Arc retain 31 shared nodes, 256 copied paths
of 33 nodes, and 2,048 bytes of snapshot holders: 273,376 plus startup = 273,664.
The streaming peak includes the full 524,296-byte declared maximum and scratch,
even though finish_trim later refunds its unused tail; Rust reserves the same maximum.

The three controls have no equivalent Rust Capacity/malformed workload. Arena tests
likewise have no Rust twin of portable refusal, reservation or completed-view laws;
Rust heap/RSS and charge ratios are n/a. There were no unobserved per-contract arena
RSS samples, so that field stays n/a rather than using observed-run RSS.

| arena contract | Cranelift peak charge B | LLVM peak charge B | unobserved H RSS | Rust RSS / peak heap | charge / Rust heap |
|---|---:|---:|---|---|---|
| sized_document | 304 | 304 | n/a | n/a | n/a |
| streaming_runs_once | 352 | 352 | n/a | n/a | n/a |
| sized_and_unknown_spans | 392 | 392 | n/a | n/a | n/a |
| admission_prefixes | 336 | 336 | n/a | n/a | n/a |
| failed_document_retains_reservation | 304 | 304 | n/a | n/a | n/a |
| streaming_refunds_only_unused_tail | 352 | 352 | n/a | n/a | n/a |
| collect_failure_prefixes | 536 | 536 | n/a | n/a | n/a |
| empty_builders_admit_nothing | 288 | 288 | n/a | n/a | n/a |
| failed_stream_retains_reservation | 304 | 304 | n/a | n/a | n/a |
| streaming_batch_runs_once | 352 | 352 | n/a | n/a | n/a |
| completed_view_survives_failed_stream | 320 | 320 | n/a | n/a | n/a |
| trim_refunds_original_payer | 352 | 352 | n/a | n/a | n/a |

Collected evidence: `peak-first/results.json`, `peak-native/results.json`,
`peak-rust/results.json`, per-run charge JSON and Massif profiles under
`/workspace/code/hirsel-briefs/core-regions-pools-922/`. Existing shipping rows are
in `shipping-runtime/results.json` and the retained Rust sampler JSON files.
`peak.py` reproduces peaks with the canonical artifact-helper JSON and an explicit
observed archive. It never invokes perf or supplies timing/RSS result fields.
