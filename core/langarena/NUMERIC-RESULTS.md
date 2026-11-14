# M2 numeric and whole-suite results

STATUS | M2 e96338cf41d7, canonical artifact identities match | 46/46 numeric H samples; 8/8 whole50 H compiler samples | Rust samples reused; M1 twin930 delta preserved separately

One sample per Hirsel side, CPU16,2GiB, instructions:u, unchanged run.js. All23 numeric matched twins are approved. The headline geomean uses22 tasks excluding SHA256; T4/T8/T16 are independent configurations. No old Hirsel row enters the M2 result. The complete counts, RSS, roots, elapsed times and all four identity fields are in M2-RUNTIME.tsv and M2-BUILD.tsv.

| Task | LLVM upstream / matched | Dev upstream / matched | Compiler profile/hash; runtime profile/hash |
|---|---:|---:|---|
| Base64::Decode | 18.723 / 1.072 | 3.607 / 1.708 | release/2f1f30544428; shipping-release-fat-lto/95dd816277b9 |
| Base64::Encode | 37.872 / 1.662 | 8.273 / 1.999 | release/2f1f30544428; shipping-release-fat-lto/95dd816277b9 |
| Binarytrees::Arena | 2.793 / 1.724 | 6.115 / 1.719 | release/2f1f30544428; shipping-release-fat-lto/95dd816277b9 |
| Binarytrees::Obj | 1.130 / 1.130 | 1.465 / 1.482 | release/2f1f30544428; shipping-release-fat-lto/95dd816277b9 |
| Brainfuck::Array | 0.961 / 0.919 | 0.585 / 0.613 | release/2f1f30544428; shipping-release-fat-lto/95dd816277b9 |
| Brainfuck::Recursion | 1.136 / 1.138 | 0.722 / 0.777 | release/2f1f30544428; shipping-release-fat-lto/95dd816277b9 |
| CLBG::Fannkuchredux | 1.333 / 1.054 | 0.204 / 0.199 | release/2f1f30544428; shipping-release-fat-lto/95dd816277b9 |
| CLBG::Mandelbrot | 1.422 / 1.179 | 5.297 / 2.480 | release/2f1f30544428; shipping-release-fat-lto/95dd816277b9 |
| CLBG::Nbody | 1.821 / 1.829 | 1.850 / 1.704 | release/2f1f30544428; shipping-release-fat-lto/95dd816277b9 |
| CLBG::Spectralnorm | 2.338 / 2.338 | 1.325 / 1.393 | release/2f1f30544428; shipping-release-fat-lto/95dd816277b9 |
| Etc::GameOfLife | 0.454 / 1.144 | 0.359 / 0.364 | release/2f1f30544428; shipping-release-fat-lto/95dd816277b9 |
| Etc::NeuralNet | 1.646 / 1.646 | 1.998 / 1.998 | release/2f1f30544428; shipping-release-fat-lto/95dd816277b9 |
| Etc::Sieve | 0.812 / 0.812 | 0.689 / 0.696 | release/2f1f30544428; shipping-release-fat-lto/95dd816277b9 |
| Etc::TextRaytracer | 2.402 / 1.605 | 3.591 / 1.649 | release/2f1f30544428; shipping-release-fat-lto/95dd816277b9 |
| Hash::CRC32 | 1.006 / 0.996 | 0.485 / 0.475 | release/2f1f30544428; shipping-release-fat-lto/95dd816277b9 |
| Hash::SHA256 | 0.967 / 0.935 | 0.725 / 0.971 | release/2f1f30544428; shipping-release-fat-lto/95dd816277b9 |
| Matmul::Single | 0.924 / 0.924 | 0.708 / 0.555 | release/2f1f30544428; shipping-release-fat-lto/95dd816277b9 |
| Matmul::T16 | 1.697 / 0.946 | 0.885 / 0.513 | release/2f1f30544428; shipping-release-fat-lto/95dd816277b9 |
| Matmul::T4 | 1.697 / 0.946 | 0.886 / 0.513 | release/2f1f30544428; shipping-release-fat-lto/95dd816277b9 |
| Matmul::T8 | 1.697 / 0.946 | 0.886 / 0.513 | release/2f1f30544428; shipping-release-fat-lto/95dd816277b9 |
| Sort::Merge | 1.723 / 1.448 | 1.611 / 0.987 | release/2f1f30544428; shipping-release-fat-lto/95dd816277b9 |
| Sort::Quick | 0.982 / 0.970 | 0.314 / 0.317 | release/2f1f30544428; shipping-release-fat-lto/95dd816277b9 |
| Sort::Self | 1.156 / 1.152 | 0.630 / 0.480 | release/2f1f30544428; shipping-release-fat-lto/95dd816277b9 |

| Engine / cohort | Tasks | Geomean upstream | Geomean matched | Compiler profile/hash; runtime profile/hash |
|---|---:|---:|---:|---|
| llvm 22-task comparison headline | 22 | 1.761267 | 1.204164 | release/2f1f30544428; shipping-release-fat-lto/95dd816277b9 |
| llvm 23 tasks including SHA256 | 23 | 1.715949 | 1.191012 | release/2f1f30544428; shipping-release-fat-lto/95dd816277b9 |
| cranelift 22-task comparison headline | 22 | 1.182886 | 0.840032 | release/2f1f30544428; shipping-release-fat-lto/95dd816277b9 |
| cranelift 23 tasks including SHA256 | 23 | 1.157970 | 0.845349 | release/2f1f30544428; shipping-release-fat-lto/95dd816277b9 |

SHA256 was excluded while LLVM hoisted one invariant outlined call; M2 inlines it and performs all1440 hashes, so it is now an ordinary row in the23-task figure. The22-task headline remains for comparison with the prior LLVM1.854702/1.268045 and dev1.159039/0.823097 upstream/matched figures. Current assembly evidence is in m2-sha.s, outer counter40cb34–40cb45, state reset40cb53/40cb59, byte loop40cc50–40ccbb.

Hoist-removing land: open for905; reading952f724a,647e5d9c and35d5d9dc did not uniquely attribute the changed inlining decision without separate compiler controls.

| Whole50 compiler row | Hirsel instructions | Rust instructions, reused | H/R total | H/R per source line | Status | Compiler profile/hash; runtime profile/hash |
|---|---:|---:|---:|---:|---|---|
| llvm check-fresh | 266939180 | 1162440012 | 0.229637 | 0.164375 | ok | release/2f1f30544428; shipping-release-fat-lto/95dd816277b9 |
| llvm build-fresh | 60977197237 | 21448145129 | 2.843006 | 2.035038 | ok | release/2f1f30544428; shipping-release-fat-lto/95dd816277b9 |
| llvm build-reuse | 269902683 | 21450577652 | 0.012583 | 0.009007 | ok | release/2f1f30544428; shipping-release-fat-lto/95dd816277b9 |
| llvm build-edit | 58797655037 | 21443311370 | 2.742004 | 1.962846 | ok | release/2f1f30544428; shipping-release-fat-lto/95dd816277b9 |
| cranelift check-fresh | 266942393 | 1162808542 | 0.229567 | 0.164325 | ok | release/2f1f30544428; shipping-release-fat-lto/95dd816277b9 |
| cranelift build-fresh | 2165458282 | 4295242593 | 0.504153 | 0.360875 | ok | release/2f1f30544428; shipping-release-fat-lto/95dd816277b9 |
| cranelift build-reuse | 261904547 | 4295129320 | 0.060977 | 0.043648 | ok | release/2f1f30544428; shipping-release-fat-lto/95dd816277b9 |
| cranelift build-edit | 408960223 | 4297980920 | 0.095152 | 0.068114 | ok | release/2f1f30544428; shipping-release-fat-lto/95dd816277b9 |

Physical loaded H source counts are7428 lines,7429 for the edit; Rust counts5317/5318. The earlier H count5846 predates formatting and library-source changes; the per-line figures compare languages within M2, not fixed-source compiler versions.

The M2 commit includes the known71c7b740 check/lower regression of0.5–1.1K instructions per own line; these compiler rows retain it. Check rows are reported as uncensored results when complete. This whole50 Rust denominator is the unchanged upstream idiomatic tree; numeric/text matched-twin source edits do not enter it. Fresh/reuse/edit/cache behavior follows core/MEASURE.md; reused Rust builds have incremental compilation off.

Full M2 identities: compiler_build_profile release; compiler_sha256 2f1f30544428e06b3d89bef6ea21cde83d03db780949f339030307055aad9557; runtime_archive_profile shipping-release-fat-lto; runtime_archive_sha256 95dd816277b936c865ccba458657918917889d6cd86a109f8343c1f03f3c8bb7. Every row uses source e96338cf41d71efd2076a9902068048a37b742c8.

M1 is separate: lands14/15 reached EXIT0;930 corrections and slice checks are preserved WIP98ac04db while the frozen M2 retake runs. No M1 matched ratio is approved. Canonical BFS stays unchanged; the scratch branch and four-way extra-row table remain in M1-PARITY.md.

## Superseded pre-M2 results

The following rows were measured8416d12b / post-fill3200f088, landed f23e1a64; they are historical and do not enter M2.

# Numeric LangArena measurements

One sample per configuration, CPU 16, 2 GiB, process-tree instructions:u, unchanged upstream run.js. Both approved Rust denominators are reused byte-for-byte from their original reports; all 23 numeric twins were independently reviewed by921. Ratios are Hirsel/Rust instruction counts; lower is better. Elapsed time, peak RSS and absolute counts are in runtime-refresh.tsv; whole50 raw rows in build-refresh.tsv. Seven fill-profile configurations were retaken on BOTH engines after9cf5a4d9; only their post-fill rows are current. Initial samples remain in runtime-before-fill.tsv. Default logical root 64 MiB, Obj 128 MiB and Arena 256 MiB as previously admitted.

| Task | LLVM upstream / matched | LLVM compiler; runtime | Dev upstream / matched | Dev compiler; runtime |
|---|---:|---|---:|---|
| Base64::Decode | 18.723 / 1.072 | release/39fc8735c829; shipping-release-fat-lto/fc1c2affb172 | 3.766 / 1.784 | release/39fc8735c829; shipping-release-fat-lto/fc1c2affb172 |
| Base64::Encode | 38.464 / 1.688 | release/39fc8735c829; shipping-release-fat-lto/fc1c2affb172 | 10.308 / 2.490 | release/39fc8735c829; shipping-release-fat-lto/fc1c2affb172 |
| Binarytrees::Arena | 3.201 / 1.976 | release/39fc8735c829; shipping-release-fat-lto/fc1c2affb172 | 6.729 / 1.891 | release/39fc8735c829; shipping-release-fat-lto/fc1c2affb172 |
| Binarytrees::Obj | 1.130 / 1.130 | release/39fc8735c829; shipping-release-fat-lto/fc1c2affb172 | 1.462 / 1.480 | release/39fc8735c829; shipping-release-fat-lto/fc1c2affb172 |
| Brainfuck::Array | 1.001 / 0.958 | release/39fc8735c829; shipping-release-fat-lto/fc1c2affb172 | 0.550 / 0.577 | release/39fc8735c829; shipping-release-fat-lto/fc1c2affb172 |
| Brainfuck::Recursion | 1.069 / 1.070 | release/39fc8735c829; shipping-release-fat-lto/fc1c2affb172 | 0.746 / 0.802 | release/39fc8735c829; shipping-release-fat-lto/fc1c2affb172 |
| CLBG::Fannkuchredux | 1.333 / 1.054 | release/39fc8735c829; shipping-release-fat-lto/fc1c2affb172 | 0.169 / 0.165 | release/39fc8735c829; shipping-release-fat-lto/fc1c2affb172 |
| CLBG::Mandelbrot | 1.423 / 1.179 | release/39fc8735c829; shipping-release-fat-lto/fc1c2affb172 | 4.798 / 2.246 | release/39fc8735c829; shipping-release-fat-lto/fc1c2affb172 |
| CLBG::Nbody | 1.821 / 1.829 | release/39fc8735c829; shipping-release-fat-lto/fc1c2affb172 | 1.828 / 1.684 | release/39fc8735c829; shipping-release-fat-lto/fc1c2affb172 |
| CLBG::Spectralnorm | 2.338 / 2.338 | release/a6ab35232a91; shipping-release-fat-lto/fc1c2affb172 | 1.297 / 1.364 | release/a6ab35232a91; shipping-release-fat-lto/fc1c2affb172 |
| Etc::GameOfLife | 0.454 / 1.144 | release/39fc8735c829; shipping-release-fat-lto/fc1c2affb172 | 0.309 / 0.314 | release/39fc8735c829; shipping-release-fat-lto/fc1c2affb172 |
| Etc::NeuralNet | 1.638 / 1.638 | release/39fc8735c829; shipping-release-fat-lto/fc1c2affb172 | 1.919 / 1.919 | release/39fc8735c829; shipping-release-fat-lto/fc1c2affb172 |
| Etc::Sieve | 0.816 / 0.816 | release/a6ab35232a91; shipping-release-fat-lto/fc1c2affb172 | 0.743 / 0.750 | release/a6ab35232a91; shipping-release-fat-lto/fc1c2affb172 |
| Etc::TextRaytracer | 2.402 / 1.605 | release/39fc8735c829; shipping-release-fat-lto/fc1c2affb172 | 3.796 / 1.743 | release/39fc8735c829; shipping-release-fat-lto/fc1c2affb172 |
| Hash::CRC32 | 1.019 / 1.009 | release/39fc8735c829; shipping-release-fat-lto/fc1c2affb172 | 0.446 / 0.436 | release/39fc8735c829; shipping-release-fat-lto/fc1c2affb172 |
| Matmul::Single | 0.924 / 0.924 | release/a6ab35232a91; shipping-release-fat-lto/fc1c2affb172 | 0.661 / 0.519 | release/a6ab35232a91; shipping-release-fat-lto/fc1c2affb172 |
| Matmul::T16 | 1.697 / 0.946 | release/a6ab35232a91; shipping-release-fat-lto/fc1c2affb172 | 0.851 / 0.492 | release/a6ab35232a91; shipping-release-fat-lto/fc1c2affb172 |
| Matmul::T4 | 1.698 / 0.946 | release/a6ab35232a91; shipping-release-fat-lto/fc1c2affb172 | 0.851 / 0.492 | release/a6ab35232a91; shipping-release-fat-lto/fc1c2affb172 |
| Matmul::T8 | 1.697 / 0.946 | release/a6ab35232a91; shipping-release-fat-lto/fc1c2affb172 | 0.851 / 0.492 | release/a6ab35232a91; shipping-release-fat-lto/fc1c2affb172 |
| Sort::Merge | 1.773 / 1.490 | release/39fc8735c829; shipping-release-fat-lto/fc1c2affb172 | 1.545 / 0.947 | release/39fc8735c829; shipping-release-fat-lto/fc1c2affb172 |
| Sort::Quick | 0.984 / 0.971 | release/39fc8735c829; shipping-release-fat-lto/fc1c2affb172 | 0.280 / 0.282 | release/39fc8735c829; shipping-release-fat-lto/fc1c2affb172 |
| Sort::Self | 3.022 / 3.011 | release/a6ab35232a91; shipping-release-fat-lto/fc1c2affb172 | 0.652 / 0.496 | release/a6ab35232a91; shipping-release-fat-lto/fc1c2affb172 |

Headline geomeans exclude Hash::SHA256 and always use 22 configurations, including independent T4/T8/T16 samples. Each Hirsel threaded variant spawns and joins 4, 8 or 16 tasks on the fixed three-worker runtime; CPU 16 measures total work rather than throughput scaling.

llvm: **1.854702x upstream / 1.268045x matched**, versus superseded 3.157368 / 2.158666.
cranelift: **1.159039x upstream / 0.823097x matched**, versus superseded 1.256696 / 0.892449.

| Separate task | LLVM upstream / matched | LLVM compiler; runtime | Dev upstream / matched | Dev compiler; runtime |
|---|---:|---|---:|---|
| Hash::SHA256 | 0.003194 / 0.003089 | release/39fc8735c829; shipping-release-fat-lto/fc1c2affb172 | 0.725198 / 0.971593 | release/39fc8735c829; shipping-release-fat-lto/fc1c2affb172 |

LLVM hoists Hirsel's pure outlined hash out of the invariant loop while Rust inlines and repeats it. This measures elimination of redundant work. The earlier direct one-hash scope remains 14,500,023 Hirsel / 15,000,028 Rust instructions; current compression-loop disassembly remains 14.5/15 instructions per byte. It is excluded from the headline throughput comparison.

Whole-suite all 50 compiler comparisons use the release compiler on8416d12b, with shipping artifacts. Rust check/build samples are reused; checks are now complete and uncensored. Explicit std/app source 5846 lines, Rust 5317; the neutral edit adds one line. Core is prebuilt for native build, and no std-source exclusion is claimed.

| Engine / scenario | H instructions | rustc instructions | Ratio | H RSS KiB | rustc RSS KiB | Compiler; runtime |
|---|---:|---:|---:|---:|---:|---|
| llvm / check-fresh | 281424316 | 1162440012 | 0.242098 | 57856 | 176864 | release/39fc8735c829; shipping-release-fat-lto/fc1c2affb172 |
| llvm / build-fresh | 62764104753 | 21448145129 | 2.926319 | 267908 | 322860 | release/39fc8735c829; shipping-release-fat-lto/fc1c2affb172 |
| llvm / build-reuse | 275042205 | 21450577652 | 0.012822 | 45568 | 322836 | release/39fc8735c829; shipping-release-fat-lto/fc1c2affb172 |
| llvm / build-edit | 60551920252 | 21443311370 | 2.823814 | 276976 | 322484 | release/39fc8735c829; shipping-release-fat-lto/fc1c2affb172 |
| cranelift / check-fresh | 281440378 | 1162808542 | 0.242035 | 57600 | 176320 | release/39fc8735c829; shipping-release-fat-lto/fc1c2affb172 |
| cranelift / build-fresh | 1946527590 | 4295242593 | 0.453182 | 77312 | 285132 | release/39fc8735c829; shipping-release-fat-lto/fc1c2affb172 |
| cranelift / build-reuse | 285282513 | 4295129320 | 0.066420 | 46080 | 284916 | release/39fc8735c829; shipping-release-fat-lto/fc1c2affb172 |
| cranelift / build-edit | 436046805 | 4297980920 | 0.101454 | 79616 | 285364 | release/39fc8735c829; shipping-release-fat-lto/fc1c2affb172 |

The whole-suite LLVM fresh/edit ratios rose from2.7961/2.6349 to2.9263/2.8238. Dev fresh/edit fell from0.5548/0.1306 to0.4532/0.1015. Exact command arrays and absolute counters sent905 in followups.md for BUILD-COST.md. Shared host page caches are warmed, never flushed; one sample supplies no wall-time confidence interval.

Changed loop counts and paired disassembly are in KERNELS.md and current-kernel-counts.json. Current H/R loop counts are Matmul12/13, Spectral34/15 and transpose42/17.5, Nbody68/40, Mandel26/22, Neural incoming18/16, Ray first all-miss pixel141/96, Self accepted partition candidate35/13, post-fill Sieve mark5/6 (formerly8/6), SHA14.5/15 per byte. Sieve fill is now memset instead of 17 instructions/byte; LLVM matched 2.254 baseline,2.082 immediately before fill, 0.816 after it. These are representative instruction paths, not population PMU averages.

Five worst LLVM matched numeric configurations, one run.js sample each. Approved Rust sides reused; profiles are separate period50003 instructions:u runs, CPU16/2GiB. Every row uses release compiler and shipping-release-fat-lto runtime.

| Task | H / matched instructions | H instructions | Rust instructions | Top resolved symbol | Share | Compiler SHA / runtime SHA | Route |
|---|---:|---:|---:|---|---:|---|---|
| Sort::Self | 3.011129 | 14849216016 | 4931444949 | std@1/list/sort_range[i32] | 78.03% | a6ab35232a91 / fc1c2affb172 | 918 / 913 / 905 |
| CLBG::Spectralnorm | 2.337844 | 34600511502 | 14800176910 | langarena@1/clbg/times | 99.90% | a6ab35232a91 / fc1c2affb172 | 905 / 912 / 889 |
| Binarytrees::Arena | 1.975719 | 16561094175 | 8382314224 | std@1/pool/update[langarena@1/binarytrees/ArenaNode,()].register | 7.48% | 39fc8735c829 / fc1c2affb172 | 922 / 883 / 905 |
| CLBG::Nbody | 1.829094 | 20545413100 | 11232563824 | hf1fccb5459df2291496ab76c0525a4deb3b16a55ff8f7e448b64d1dab1ee45d6 | 99.96% | 39fc8735c829 / fc1c2affb172 | 883 / 905 |
| Base64::Encode | 1.688305 | 130032436788 | 77019520447 | langarena@1/base64/encode | 66.24% | 39fc8735c829 / fc1c2affb172 | 889 / 905 |

Binarytrees::Arena attribution is incomplete. The highest raw frame is kernel-labelled unresolved IP0xffffffffb6a00ba0 at 25.73%, above the highest resolved frame pool.update at 7.48%; 74.57% of profile samples are unresolved/kernel-labelled. No cause is assigned to those samples. pool.update was identified exactly from InstanceKey digest with type arguments ArenaNode and(), and retains multiple copies of its 88-byte Slot across its callback. Full disassembly retained losslessly. Other shares are per profile, never aggregated or presented as wall time.

Base64::Encode additionally spends 33.69% in core.string.push_char. Sort additionally spends 21.68% in its comparator closure. Nbody's top symbol is the inlined clbg dispatch; KERNELS.md isolates its 68-instruction planet-pair path. Spectral's top symbol is times; paired nontranspose term34/R15 and transpose42/R17.5. No task source workaround was introduced.

All full identities and raw names are in WORST-LLVM-MATCHED.tsv/json.


Artifact identities, full SHA256 values:

- compiler_build_profile=release; compiler_sha256=39fc8735c8295b5177d1b7cf8e96aef3eb9639df16ca604da5eb1b0947a45ab1; runtime_archive_profile=shipping-release-fat-lto; runtime_archive_sha256=fc1c2affb1729609e2f2fb3af403d3657156d68d7d2cd7c2dc6b655c7846691e.
- compiler_build_profile=release; compiler_sha256=a6ab35232a91029c7e2e1d331dbdc8f614c00f1c5c81bb30a40a820fa550491a; runtime_archive_profile=shipping-release-fat-lto; runtime_archive_sha256=fc1c2affb1729609e2f2fb3af403d3657156d68d7d2cd7c2dc6b655c7846691e.


## Superseded table, report commit 5c7097fcad48

| Task | Dev test | LLVM test | Root / blocker | LLVM upstream/matched | Dev upstream/matched | Compiler profile/hash; runtime profile/hash |
|---|---|---|---|---|---|---|
| Binarytrees::Obj | OK | OK | root: 128 MiB (default 64) | 1.165/1.165 | 1.731/1.751 | release/3a7b4a7027da; shipping-release-fat-lto/a18f790fcd68 |
| Binarytrees::Arena | OK | OK | root: 256 MiB (default 64) | 3.325/2.052 | 6.899/1.939 | release/3a7b4a7027da; shipping-release-fat-lto/a18f790fcd68 |
| Brainfuck::Array | OK | OK | 64 MiB | 1.044/0.998 | 0.552/0.578 | release/3a7b4a7027da; shipping-release-fat-lto/a18f790fcd68 |
| Brainfuck::Recursion | OK | OK | 64 MiB | 1.149/1.151 | 0.820/0.883 | release/3a7b4a7027da; shipping-release-fat-lto/a18f790fcd68 |
| Base64::Encode | OK | OK | 64 MiB | 88.168/3.870 | 11.224/2.711 | release/3a7b4a7027da; shipping-release-fat-lto/a18f790fcd68 |
| Base64::Decode | OK | OK | 64 MiB | 24.829/1.422 | 3.772/1.786 | release/3a7b4a7027da; shipping-release-fat-lto/a18f790fcd68 |
| Hash::SHA256 | OK | OK | 64 MiB | 0.004/0.004 | 0.790/1.059 | release/3a7b4a7027da; shipping-release-fat-lto/a18f790fcd68 |
| Hash::CRC32 | OK | OK | 64 MiB | 1.027/1.017 | 0.512/0.501 | release/3a7b4a7027da; shipping-release-fat-lto/a18f790fcd68 |
| Sort::Quick | OK | OK | 64 MiB | 1.048/1.035 | 0.369/0.372 | release/3a7b4a7027da; shipping-release-fat-lto/a18f790fcd68 |
| Sort::Merge | OK | OK | 64 MiB | 1.815/1.526 | 1.583/0.970 | release/3a7b4a7027da; shipping-release-fat-lto/a18f790fcd68 |
| Sort::Self | OK | OK | 64 MiB | 3.954/3.939 | 0.854/0.650 | release/3a7b4a7027da; shipping-release-fat-lto/a18f790fcd68 |
| Etc::Sieve | OK | OK | 64 MiB | 2.254/2.254 | 1.060/1.071 | release/3a7b4a7027da; shipping-release-fat-lto/a18f790fcd68 |
| Etc::GameOfLife | OK | OK | 64 MiB | 0.455/1.144 | 0.331/0.337 | release/3a7b4a7027da; shipping-release-fat-lto/a18f790fcd68 |
| Etc::NeuralNet | OK | OK | 64 MiB | 3.658/3.658 | 2.006/2.006 | release/3a7b4a7027da; shipping-release-fat-lto/a18f790fcd68 |
| Etc::TextRaytracer | OK | OK | 64 MiB | 5.957/3.981 | 3.868/1.776 | release/3a7b4a7027da; shipping-release-fat-lto/a18f790fcd68 |
| Matmul::Single | OK | OK | 64 MiB | 3.304/3.304 | 0.685/0.538 | release/3a7b4a7027da; shipping-release-fat-lto/a18f790fcd68 |
| Matmul::T4 | OK | OK | 64 MiB | 4.787/2.667 | 0.878/0.508 | release/3a7b4a7027da; shipping-release-fat-lto/a18f790fcd68 |
| Matmul::T8 | OK | OK | 64 MiB | 4.787/2.667 | 0.878/0.508 | release/3a7b4a7027da; shipping-release-fat-lto/a18f790fcd68 |
| Matmul::T16 | OK | OK | 64 MiB | 4.787/2.667 | 0.878/0.508 | release/3a7b4a7027da; shipping-release-fat-lto/a18f790fcd68 |
| CLBG::Fannkuchredux | OK | OK | 64 MiB | 1.078/0.852 | 0.176/0.172 | release/3a7b4a7027da; shipping-release-fat-lto/a18f790fcd68 |
| CLBG::Mandelbrot | OK | OK | 64 MiB | 4.647/3.851 | 4.803/2.248 | release/3a7b4a7027da; shipping-release-fat-lto/a18f790fcd68 |
| CLBG::Nbody | OK | OK | 64 MiB | 5.414/5.438 | 1.847/1.701 | release/3a7b4a7027da; shipping-release-fat-lto/a18f790fcd68 |
| CLBG::Spectralnorm | OK | OK | 64 MiB | 5.597/5.597 | 1.326/1.394 | release/3a7b4a7027da; shipping-release-fat-lto/a18f790fcd68 |

HEADLINE numeric geomeans over the fixed22 tasks excluding Hash::SHA256: llvm/idiomatic 3.157x, llvm/matched 2.159x, cranelift/idiomatic 1.257x, cranelift/matched 0.892x.

Sensitivity including Hash over all23 tasks: llvm/idiomatic 2.353x, llvm/matched 1.633x, cranelift/idiomatic 1.232x, cranelift/matched 0.899x.

Hash::SHA256 is separate: LLVM hoists its pure outlined hash to one call and multiplies by1440; both Rust binaries inline and repeat the hash, so this is redundant-work elimination rather than hash throughput. HASH-AUDIT.md retains the IR proof and one-hash14.50M/15.00M scoped counters.

Whole-suite all50 native build instruction ratios below; upstream twin only. Compiler release/3a7b4a7027da and shipping runtime/a18f790fcd68 apply to every row. Loaded source5929 Hirsel lines (std/app source; core prebuilt),5317 Rust; body edits add one line each.

| Scenario | LLVM upstream ratio | Cranelift upstream ratio | Compiler profile/hash; runtime profile/hash |
|---|---|---|---|
| build-fresh | 2.7961 | 0.5548 | release/3a7b4a7027da; shipping-release-fat-lto/a18f790fcd68 |
| build-reuse | 0.0138 | 0.0726 | release/3a7b4a7027da; shipping-release-fat-lto/a18f790fcd68 |
| build-edit | 2.6349 | 0.1306 | release/3a7b4a7027da; shipping-release-fat-lto/a18f790fcd68 |


The old run/check notes and all raw counters remain in the lane report under `/workspace/code/hirsel-briefs/langs-numeric-920/`. Old check ratios were censored by std.arena gaps. Current check rows are complete. The Rust twins of all23 numeric configurations are approved by langs-text-921.

Runtime command: first `python3 core/tools/measurement_artifacts.py`, then `python3 core/langarena/run.py --compiler <helper compiler> --runtime <helper runtime> --compiler-profile release --module <module> --task <task> --backend <llvm|cranelift> --idiomatic <approved Rust binary> --resume <previous report> --out <new external directory>`. Use the admitted `--root-mib 128` for Obj and256 for Arena. Build/check command replaces the module/task arguments with `--module suite --task All::Build --build-only --suite`, resuming the previous whole-suite report. Frozen initial source is8416d12b; fill follow-up source3200f088 includes9cf5a4d9. Final post-fill samples replace the initial fill-set samples, never averaged.
