| Program | NEW 7b43077d warm closure G i / m | M2 warm closure G i / m | Change | Compiled units old / M2 |
|---|---:|---:|---:|---:|
| bfs | 2.860650 / 3.463439 | 3.559616 / 4.045167 | +20.251% | 2 / 2 |
| generics | 2.673385 / 2.899431 | 3.593486 / 3.568844 | +28.523% | 2 / 2 |
| lru | 3.395897 / 3.423479 | 5.202738 / 4.585730 | +43.539% | 2 / 2 |
| files | 3.696234 / 3.730719 | 4.237810 / 4.236779 | +14.106% | 2 / 2 |
| json | 4.859251 / 5.363596 | 7.185140 / 8.625906 | +54.664% | 2 / 2 |
| pipeline | 6.601202 / 6.735389 | 7.919155 / 8.062845 | +19.836% | 2 / 2 |
| request-scratch | 2.980735 / 3.079737 | 3.832503 / 3.969931 | +28.743% | 2 / 2 |
| rope | 3.180580 / 3.370212 | 3.953887 / 3.864773 | +19.354% | 2 / 2 |
| versions | 2.253859 / 2.034139 | 3.206805 / 2.788216 | +39.809% | 2 / 2 |
| Total, 18 builds | 66.601935580 | 86.439330720 | +29.7850% | 18 / 18 |

NEW sample, taken once after the documents attribution under the 05:17 ruling. Exact7b43077d compiler, runtime archive rule, sources and harness commands are unchanged. Full manifest closure prewarmed, fresh application store, original order, one compiled unit per measured build. No source or compiler repair was needed. CPU16/jobs1/2GiB and shared serial lock; load rule enforced. The old artifact build was100% cache hits and succeeded; an unsupported multi-output --out request then returned a CLI error, so the known built output paths were used without rebuilding.

Compiler8991813bb6eee06de0a4bca1af6ad80124161f05428fe7f1b8f2d3968aa3cd69; old runtime31750a97151c00750b8bd3274ced40c5bfb59cfc42aada9466fa4fa02701a054. That is the unchanged old archive rule, before the later standalone shipping-fat-LTO rule. This historical comparison includes source/checker/runtime/emitter changes, not an isolated emitter regression. M2 stays frozen. The target is73.262129138G; M2 is18.0% over it. Hard ceiling86.604G is never raised.

Raw warm-7b/rows.json, warm-counts.json, warm-7b-artifacts.json; /tmp/core-llvm-905-warm-7b.log.

| Program | LLVM I/R idiomatic / matched† | Cranelift dev I/R idiomatic / matched† | LLVM RSS KiB idiomatic / matched | Dev RSS KiB idiomatic / matched |
|---|---:|---:|---:|---:|
| bfs | 2.358 / 1.299 | 2.127 / 0.929 | 3,852 / 3,084 | 4,108 / 3,344 |
| generics | 1.331 / 1.199 | 1.570 / 0.773 | 3,584 / 3,840 | 3,840 / 3,840 |
| lru | 0.981 / 1.202 | 0.680 / 0.967 | 2,560 / 2,560 | 2,816 / 2,816 |
| files | 1.035 / 1.052 | 0.524 / 0.084 | 3,072 / 3,072 | 3,072 / 3,072 |
| json | 0.721 / 1.401 | 1.060 / 0.693 | 2,560 / 2,560 | 2,816 / 2,560 |
| pipeline | 1.049 / 1.089 | 0.812 / 0.432 | 3,328 / 3,072 | 3,328 / 3,328 |
| request-scratch | 1.385 / 1.497 | 1.419 / 0.629 | 3,328 / 3,328 | 3,584 / 3,584 |
| rope | 0.974 / 0.814 | 0.755 / 0.899 | 2,816 / 2,560 | 3,072 / 2,816 |
| versions | 1.675 / 1.177 | 1.925 / 1.088 | 2,304 / 2,304 | 2,304 / 2,304 |

† Every matched ratio is diagnostic. The corrected Rust samples are from76d9b6ce and are NOT YET APPROVED by independent re-check. Idiomatic Rust samples are unchanged and reused from the original905 measurement. No Rust side is rerun.

M2 source commit `e96338cf41d71efd2076a9902068048a37b742c8`. Release compiler `2f1f30544428e06b3d89bef6ea21cde83d03db780949f339030307055aad9557`; shipping fat-LTO runtime `95dd816277b936c865ccba458657918917889d6cd86a109f8343c1f03f3c8bb7`. Both equal the shared M2 identities. LLVM uses release optimization, Cranelift dev uses no optimization; the compiler and runtime artifacts are the same for both. No later emitter code is included.

One serial CPU16 sample per primary normal-warm program/layout/engine and per build, load<=32,2GiB, jobs1. All232case observations EACH engine match expected stdout/stderr/exit and changed/deleted files. The full corpus/inputs is unchanged; common904 core/tools/m1.py fixture closure and file oracle are used by the established905 process-counter wrapper. Each engine cache is seeded with a BFS build; later roots include first-use library/native objects. Application request/native stores are fresh. GNU time reports the largest process RSS, not summed RSS or portable logical charge. Process-tree instructions include startup, I/O, cleanup and wrapper children. Shared pages are not flushed.

Early completed cold/edge PMU samples are retained and excluded from the primary table; the remaining edge/refusal checks are untimed. No completed sample was repeated. Every row carries source/compiled-artifact identities; exact counts, Rust denominators, instruction/RSS ratios and sampling labels are in M1-NATIVE-ROWS.tsv.

| Program | LLVM instructions idiomatic / matched | Rust opt3 instructions idiomatic / matched† | Dev instructions idiomatic / matched | Rust opt0 instructions idiomatic / matched† |
|---|---:|---:|---:|---:|
| bfs | 83,723,408 / 103,505,846 | 35,508,929 / 79,670,111 | 355,517,351 / 462,406,788 | 167,169,145 / 497,608,903 |
| generics | 64,420,665 / 62,133,808 | 48,392,128 / 51,805,067 | 168,884,068 / 162,555,448 | 107,545,718 / 210,284,195 |
| lru | 41,951,356 / 50,669,043 | 42,765,587 / 42,163,522 | 183,924,646 / 241,968,745 | 270,280,877 / 250,181,817 |
| files | 1,816,139 / 1,816,108 | 1,754,138 / 1,725,526 | 9,241,930 / 9,242,068 | 17,625,727 / 109,618,772 |
| json | 97,249,049 / 71,071,009 | 134,876,624 / 50,736,327 | 350,359,664 / 300,353,208 | 330,602,276 / 433,461,835 |
| pipeline | 16,757,164 / 11,778,290 | 15,967,092 / 10,815,924 | 87,655,869 / 78,080,214 | 107,893,727 / 180,947,450 |
| request-scratch | 44,578,881 / 47,456,269 | 32,198,051 / 31,708,639 | 105,751,747 / 111,828,282 | 74,527,438 / 177,861,620 |
| rope | 13,101,077 / 11,963,581 | 13,448,566 / 14,691,547 | 55,735,776 / 64,735,837 | 73,786,326 / 72,014,258 |
| versions | 5,309,981 / 3,553,311 | 3,171,034 / 3,017,795 | 14,425,058 / 12,195,990 | 7,493,981 / 11,205,793 |

| Program | LLVM RSS ratio idiomatic / matched† | Dev RSS ratio idiomatic / matched† |
|---|---:|---:|
| bfs | 0.792 / 0.861 | 0.845 / 0.933 |
| generics | 1.000 / 1.000 | 1.071 / 1.000 |
| lru | 1.111 / 1.000 | 1.222 / 1.100 |
| files | 1.333 / 1.001 | 1.200 / 0.923 |
| json | 1.000 / 1.111 | 1.100 / 1.000 |
| pipeline | 0.867 / 1.001 | 0.812 / 0.998 |
| request-scratch | 1.182 / 1.182 | 1.273 / 1.273 |
| rope | 1.000 / 1.000 | 1.000 / 1.000 |
| versions | 1.286 / 1.125 | 1.286 / 1.000 |

| Program | 7b43077d original build G i / m, 47 objects total | M2 same BFS-seed recipe G i / m | Compiled objects i / m | First-use library objects i / m | Warm library closure G i / m, policy new at M2 |
|---|---:|---:|---:|---:|---:|
| bfs | 3.186868 / 3.676415 | 3.558232 / 4.059819 | 1 / 2 | 0 / 1 | 3.559616 / 4.045167 |
| generics | 3.074593 / 2.958054 | 3.997007 / 3.584059 | 9 / 2 | 8 / 1 | 3.593486 / 3.568844 |
| lru | 4.235917 / 3.828352 | 5.985364 / 4.980881 | 15 / 9 | 14 / 8 | 5.202738 / 4.585730 |
| files | 4.106837 / 3.794002 | 4.634054 / 4.225973 | 15 / 1 | 14 / 0 | 4.237810 / 4.236779 |
| json | 7.323766 / 8.343653 | 9.520682 / 10.035476 | 33 / 27 | 32 / 26 | 7.185140 / 8.625906 |
| pipeline | 8.839693 / 7.072426 | 8.255076 / 8.233775 | 8 / 5 | 7 / 4 | 7.919155 / 8.062845 |
| request-scratch | 3.441668 / 3.206203 | 4.325636 / 4.115790 | 7 / 3 | 6 / 2 | 3.832503 / 3.969931 |
| rope | 3.737597 / 3.530463 | 4.461092 / 3.964069 | 15 / 5 | 14 / 4 | 3.953887 / 3.864773 |
| versions | 2.309632 / 2.065104 | 3.244233 / 2.785657 | 2 / 1 | 1 / 0 | 3.206805 / 2.788216 |
| Total, 18 builds | 78.731243633 | 93.966875247 | 160 | 142 | 86.439330720 |

The M2 row of record is 93.966875247 G / 160 objects, beside the original control's 78.731243633 G / 47 objects. No emitter-regression percentage is inferred across those cache states. There are 18 application units plus 142 cold library objects at M2, against 18 plus 29 in the control. Caller-owned private variants stay inside the application release units.

The initial explanation that the cache recipe differed was not established. The retained control script map-inputs/m1-combined.py and the M2 runner both seed only BFS before the manifest-order sample. The M2 compiler classifies more bodies as published imports: the BFS prewarm checks 3 files/23 bodies and compiles 16 objects, versus 16 files/179 bodies and 6 objects in the control. Later first-use library bodies are real work under that literal recipe. No confidence resampling is needed.

The supplementary M2 row is 86.439330720 G / 18 objects, labelled "warm library closure, policy new at M2". No percentage against 7b43077d is inferred from it until the new warm control exists. The working ceiling remains 86.604 G absolute, and future lands use this warm-closure preparation with one compiled unit per measured build and a same-base delta. The attempted configuration correction was incorrectly labelled equivalent; that claim is withdrawn. Raw supplemental rows remain in m2-build-warm/ and build-fully-warm-supplementary.json. No runtime sample was repeated.

All observations use exact M2 sources and artifacts. This historical comparison includes checker, source, runtime and emitter changes; it does not isolate an emitter delta. The known 71c7b740 check/lower regression remains included, owned by 915.
