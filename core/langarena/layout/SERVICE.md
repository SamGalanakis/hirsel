> Historical pre-M2 qualification matrix. The final named-commit M2 rows are in [M2.md](M2.md), [M2.tsv](M2.tsv) and [M2-build.tsv](M2-build.tsv). These earlier rows are not the M2 number.

# Versioned graph and document completion matrix

Source/control bundle a4fa21ae; final matched malformed lifetime/refund fix063d47c1.
Both graph twins are approved:925 idiomatic;925 +929 +orchestrator exact R1/R2 diff
read for matched. This is the qualification matrix before the single named-commit
M2 Hirsel-only retake. No new Hirsel sample is taken until that commit is named.

Runtime source base7f8c4f19 plus the controlled own sources, frozen release compiler
cf14072791ca6cfd0e041a618fb22079e84e1542ca1ce5c0d739ac94918bff2f and shipping
fat-LTO runtime626ce1bd419541c3e8d8f0054eb0795c9e83543ff20e8412e7e03c2e9602ed3a.
Observed memory uses the same compiler and diagnostic archive
ddf0c4a0b365fb6f2beb3ba3ffd4f71783ba952af05b0afacd0d053c914253df, metric_version2.
LLVM release pairs with rustc1.99 opt3; Cranelift dev with opt0. Both use checked
arithmetic, baseline x86-64, one CGU and no Rust LTO. One sample per configuration,
CPU16 and2GiB. Existing valid Rust release document samples are retained. Raw rows,
profiles, source snapshots and build metrics remain in the lane evidence directory.
The full working report is SERVICE.md in the lane brief directory.

Fixed graph parameters are generated into both consumed constant files: ring mutation,
u32 edges, full u64 identity/index/generation, stale probe after each removal, LIFO reuse,
256 retained64-node snapshots copying33 nodes/version, one task/thread and no hash Map.
Idiomatic uses std separate payload/generation/free Vecs; matched uses intrusive slots.
Matched models the one root funding domain with Result admission and RAII refunds, no
extra payload/header fields or backing allocation. It claims no foreign-payer/task model.
Rust args/println and Hirsel's String/write envelope remain included in process totals.

## Per-phase speed and RSS

Matched admission bookkeeping is lighter than Hirsel's; reviewer929 estimates10^5–10^6 instructions per snapshot phase favouring Rust, which raises Hirsel/Rust. This is an analytic bias estimate, not a measurement.

| family / phase | backend | H I / RSS KiB | matched I / RSS KiB | H / matched | idiomatic I / RSS KiB | H / idiomatic |
|---|---|---:|---:|---|---:|---:|
| graph exclusive | llvm | 3,412,081 / 1792 | 5,131,927 / 1792 | 0.665 | 3,004,334 / 2048 | 1.136 |
| graph exclusive | cranelift | 9,820,085 / 1792 | 38,309,751 / 2048 | 0.256 | 14,035,120 / 2048 | 0.700 |
| graph churn | llvm | 9,314,861 / 1792 | 9,089,927 / 2048 | 1.025 | 7,213,566 / 2048 | 1.291 |
| graph churn | cranelift | 54,306,844 / 1792 | 70,136,109 / 2304 | 0.774 | 52,029,677 / 2048 | 1.044 |
| graph rc | llvm | 5,737,026 / 2304 | 4,397,494 / 2304 | 1.305 | 4,113,051 / 2304 | 1.395 |
| graph rc | cranelift | 9,799,580 / 2304 | 9,457,203 / 2304 | 1.036 | 7,151,998 / 2304 | 1.370 |
| graph arc | llvm | 6,243,734 / 2304 | 4,395,195 / 2304 | 1.421 | 4,097,085 / 2304 | 1.524 |
| graph arc | cranelift | 10,174,017 / 2304 | 9,846,574 / 2560 | 1.033 | 7,558,828 / 2560 | 1.346 |
| graph lifetime | llvm | 22,867,495 / 2304 | 21,234,189 / 2304 | 1.077 | 16,616,142 / 2304 | 1.376 |
| graph lifetime | cranelift | 82,512,365 / 2304 | 125,955,233 / 2304 | 0.655 | 78,954,693 / 2304 | 1.045 |
| document ordinary | llvm | 176,896,277 / 2304 | 156,948,240 / 2304 | 1.127 | 156,948,240 / 2304 | 1.127 |
| document ordinary | cranelift | 435,471,578 / 2304 | 372,105,684 / 2560 | 1.170 | 372,105,684 / 2560 | 1.170 |
| document sized | llvm | 83,994,291 / 1792 | 68,996,955 / 2048 | 1.217 | 156,948,240 / 2304 | 0.535 |
| document sized | cranelift | 474,697,919 / 1792 | 1,088,554,813 / 2048 | 0.436 | 372,105,684 / 2560 | 1.276 |
| document streaming | llvm | 73,164,480 / 1792 | 59,559,859 / 2048 | 1.228 | 156,948,240 / 2304 | 0.466 |
| document streaming | cranelift | 444,120,556 / 1792 | 1,026,510,876 / 2048 | 0.433 | 372,105,684 / 2560 | 1.194 |
| document json_parse | llvm | 281,675,039 / 2048 | 279,727,598 / 2048 | 1.007 | 202,735,758 / 2560 | 1.389 |
| document json_parse | cranelift | 1,798,368,545 / 2304 | 2,370,121,983 / 2048 | 0.759 | 874,994,801 / 2816 | 2.055 |
| document json_print | llvm | 96,295,408 / 2304 | 99,949,397 / 2560 | 0.963 | 85,547,504 / 2816 | 1.126 |
| document json_print | cranelift | 427,650,018 / 2560 | 727,780,809 / 2304 | 0.588 | 1,353,817,501 / 3072 | 0.316 |

## Observed charge, retained backing and region slack

Independent interval/lifetime peaks are not additive. Charge is MEM-19 reservations, including288 startup; retained is requested live native backing including prefixes/chunks, not RSS; slack is unassigned region chunk tails, not spare List capacity or inferred trim waste. All46 observed runs pass. Both engines agree on every peak below. At main return each phase has charged288/retained57/slack0; after run_end all three current fields are0. Complete per-backend run_start/startup/main/run_end values and independent interval peaks are retained in service-*-memory/results.jsonl, not inferred from RSS.

| phase | peak_root_reserved_bytes / peak_charged_bytes | peak_retained_bytes | peak_slack_bytes |
|---|---:|---:|---:|
| exclusive | 98592 | 131555 | 0 |
| churn | 213280 | 229907 | 0 |
| rc | 273664 | 613019 | 0 |
| arc | 273664 | 613019 | 0 |
| lifetime | 273664 | 613019 | 0 |
| ordinary | 368896 | 621659 | 0 |
| sized | 162760 | 163130 | 0 |
| streaming | 524608 | 524979 | 0 |
| json_parse | 341848 | 342214 | 0 |
| json_print | 538456 | 538918 | 0 |

## Separate early-Capacity and malformed controls

No Rust Capacity ratio: portable reservation/refund laws have no std allocator failure equivalent. Graph malformed has approved idiomatic outputs, but its speed is not combined with valid work. These phases are independently checked, not a blended score. Each row below gives LLVM / Cranelift shipping I, RSS, then the agreeing observed peaks charge/retained/slack.

| phase | LLVM I / RSS KiB | Cranelift I / RSS KiB | observed peak charged / retained / slack B |
|---|---:|---:|---:|
| arc_capacity | 737,576 / 1792 | 747,568 / 1792 | 536 / 1011 / 0 |
| churn_capacity | 727,742 / 1792 | 737,453 / 1792 | 496 / 755 / 0 |
| document_malformed | 726,552 / 1792 | 728,089 / 1792 | 296 / 487 / 0 |
| exclusive_capacity | 725,166 / 1792 | 726,878 / 1792 | 336 / 547 / 0 |
| graph_malformed | 725,247 / 1792 | 727,573 / 1792 | 400 / 611 / 0 |
| json_capacity | 4,710,285 / 2048 | 26,841,701 / 2048 | 196896 / 197139 / 0 |
| json_reject | 731,984 / 1792 | 733,591 / 1792 | 4392 / 4791 / 0 |
| json_utf8 | 729,158 / 1792 | 730,333 / 1792 | 4384 / 4579 / 0 |
| ordinary_capacity | 727,941 / 1792 | 734,158 / 1792 | 544 / 920 / 0 |
| rc_capacity | 734,625 / 1792 | 745,877 / 1792 | 536 / 1011 / 0 |
| sized_capacity | 728,786 / 1792 | 736,733 / 1792 | 424 / 66203 / 65411 |
| streaming_capacity | 728,997 / 1792 | 735,921 / 1792 | 704 / 66203 / 65128 |

## Laws and record mode

15 source laws and2 private MAX-generation retirement laws pass each evaluator/native engine. Both source layouts use charge-v1 budgets; no native physical sizes are pinned. Four separate native ABI CountOverflow controls pass. Both Arc recording-sink runs verify8512new/512share/9024release/8512last; they have no Rust event-recording ratio.

| Arc recording mode | shipping I | shipping RSS KiB | observed peak charged / retained / slack B |
|---|---:|---:|---:|
| llvm | 6,608,674 | 2560 | 273664 / 613019 / 0 |
| cranelift | 10,816,311 | 2560 | 273664 / 613019 / 0 |

## Current build cells

The first service-build-* rows used capped's default4GiB, so they are excluded; the fixed2GiB service-build2-* rows replace only that invalid configuration. The old JSON idiomatic dependency-path failure is likewise excluded. All pinned serde dependencies are now built locally at opt3/opt0 using the cached serde_core build-script output, no network/Cargo overrides. Graph2 final refcount/zero-refund changes precede these rows. All72 table cells pass,69 new samples plus3 references to the same backend-independent Hirsel check. No green runtime sample was repeated for this setup correction.

Each group builds a temporary same-module family entry: graph's four phases in order; all three document variants in one64-iteration schedule; JSON parse then print with separate reads/lifetimes and original outputs. These are compilation workloads, not blended runtime scores. Fresh has a new native/checked-product store, separate from check's store. Reuse retains that build record; Rust performs a warm fresh-process recompile with incremental disabled. Thus reuse counts are absolute, not a Hirsel/Rust incremental ratio. Edit changes one entry-body +0 to+1 on both sides, with exact +1 output checked. All built fresh/reuse/edit executables pass the independent checksums. Rust std/serde libraries are prebuilt; H loads/checks source core/std as stated by the request. Only complete first-party source is counted, not a selected hot function.

| group / backend | H check/lower I / request code lines / K per line | H check process I | Rust check I matched / idiomatic | H fresh I | Rust fresh I matched / idiomatic | H / fresh matched | H / fresh idiomatic | H reuse I | H edit I | Rust edit I matched / idiomatic | H / edit matched | H / edit idiomatic |
|---|---:|---:|---:|---:|---:|---|---:|---:|---:|---:|---|---:|
| graph llvm | 64,649,751 / 4556 / 14.190 | 145,921,362 | 177,955,896 / 75,254,266 | 2,750,568,197 | 1,179,246,689 / 814,189,828 | 2.332 | 3.378 | 112,129,235 | 2,749,210,179 | 1,178,993,603 / 815,210,422 | 2.332 | 3.372 |
| graph cranelift | 64,649,751 / 4556 / 14.190 | 145,921,362 | 177,976,397 / 75,259,619 | 484,541,192 | 505,263,398 / 293,767,154 | 0.959 | 1.649 | 113,293,342 | 406,618,922 | 505,482,411 / 293,679,988 | 0.804 | 1.385 |
| documents llvm | 62,033,501 / 4500 / 13.785 | 141,326,684 | 56,764,338 / 57,198,340 | 2,372,401,201 | 433,227,066 / 281,725,987 | 5.476 | 8.421 | 109,037,993 | 2,369,272,600 | 434,285,408 / 282,449,291 | 5.456 | 8.388 |
| documents cranelift | 62,033,501 / 4500 / 13.785 | 141,326,684 | 56,766,398 / 57,209,295 | 467,778,430 | 242,099,121 / 198,941,645 | 1.932 | 2.351 | 109,930,558 | 400,977,860 | 241,988,795 / 198,975,490 | 1.657 | 2.015 |
| json llvm | 57,959,053 / 4367 / 13.272 | 133,486,556 | 103,369,804 / 33,477,167 | 6,783,577,275 | 852,199,017 / 1,546,738,965 | 7.960 | 4.386 | 113,800,647 | 6,772,773,459 | 852,444,611 / 1,546,108,663 | 7.945 | 4.381 |
| json cranelift | 57,959,053 / 4367 / 13.272 | 133,486,556 | 103,375,043 / 33,469,073 | 588,273,156 | 341,185,770 / 220,571,933 | 1.724 | 2.667 | 116,645,174 | 408,345,780 | 341,146,585 / 220,569,657 | 1.197 | 1.851 |


Cranelift JSON parse is2.055x serde. Its one period50003 profile has868samples, so
shares are leads: at24.08%, write_value9.22%, memmove7.83%, unresolved7.72%, skip6.57%,
float.scan6.45%. The emitter/helper findings are routed904/889; no body or source
benchmark workaround is made. Complete build RSS, wall time, line counts and internal
stage counters remain in the raw JSON alongside these aggregate build rows.

Use `service.py --matrix --stage runtime` and a separate `--stage observe` pass at the
M2 commit named by the orchestrator, retaining these qualified Rust rows. Record mode
remains a separate `--events --phase arc` configuration. Numeric LAYOUT3 stays920-owned.
Deliver the named-commit rows to889; no additional Hirsel samples precede that pass.
