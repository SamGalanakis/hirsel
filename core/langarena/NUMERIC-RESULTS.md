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
