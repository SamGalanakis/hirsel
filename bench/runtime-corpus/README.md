# Fixed native runtime corpus

`tools/runtime-corpus --output /tmp/runtime-corpus-$(date +%s)` builds eight
native LLVM Hirsel programs and optimized safe Rust twins, checks identical
stdout, measures both processes on CPU2 and appends results to
`~/.local/state/hirsel/runtime-corpus.jsonl`. Linux perf access is required.
The command prepares Buck2 and uses the release toolchain and native runtime
archives. Rust targets use Cargo's release flags through Buck2; JSON reuses the
existing O3 checked serde_json reference. No Cargo invocation is needed.

`--cpu` selects an allowed CPU. CPUs 12-15 and 28-31 are refused on this host.
Every workload process is pinned to that CPU, including child threads. Perf
runs on a separate allowed CPU. Runs
use `tools/capped`, 2 GiB and one CPU, with no time limit. The ordinary micro
timing lock serializes cooperating measurements. Process startup, preparation,
work, output and teardown are included. No server or load client is needed.

Corpus version 3 gates elapsed wall time, measured with Python's monotonic
clock. The observer waits for perf's `--pre` handshake inside `tools/capped`,
starts the timer, and releases that hook. It stops on the `--post` handshake,
after perf has reaped the workload and closed its counters. Admission and
observer launch are outside this boundary; exec, allocation, preparation,
work, output, child joins and teardown are inside. The boundary also includes
perf setup after `--pre` and the final `/proc/stat` snapshot. These small fixed
costs remain in both timings and in each startup estimate; none is subtracted.
Linux v6.8 `tools/perf/builtin-stat.c:951-968` brackets `__run_perf_stat`
with these hooks; `:892` reaps the child with `wait4`. Perf's own elapsed
summary starts after workload release and is ignored.

Every sample records process-tree user plus system CPU time from perf's
`wait4` summary, inclusive `instructions`, `instructions:u`, context switches,
counter running percentages, CPU utilization and SMT sibling activity.
These are diagnostics. Only wall time controls the 1.5x geometric mean and
3x maximum budget. Both participants and their child threads get the same
one-CPU allocation. The observer and Python timer run on a separate allowed
CPU outside the workload CPU's SMT siblings.

The host rules require one fresh sample per side, with unmeasured spread.
Near-threshold single samples are uncertain. On other hosts, `--repeats N`
supports alternating pair order, medians and percentage min-max spreads.
Shared-host load and build-order effects remain possible.

| Workload | Fixed input | Cost profile and checked result |
|---|---|---|
| JSON | 2,000 rows, 64 epochs, 10 round trips each | DOM parsing, allocation and serialization; sum of serialized lengths |
| LogParser | 10,000 log lines, 1,000 scans per pattern | Regex parsing/search, match allocation and count map; total of 13 pattern counts |
| gzip | 1 MiB of `a`, gzip level 9, reads of 4,096 bytes, 2,048 complete decodes | Streaming DEFLATE/Huffman/LZ77 and CRC validation; decoded length and sum of every byte |
| float_math | 100,000 x 64 loop steps, 32 epochs | floor/ceil/trunc/round/sqrt/abs and floating-point accumulation; exact final IEEE bits |
| priority_queue | min-heap capacity 1,024, 131,072 items, LCG seed 42, 2,048 complete experiments | Combined push/pop and drain; ordered returned-value checksum |
| map_u64 | 200,000 keys, 128 epochs | Hash table growth, insertion and lookup; sum of values plus map length |
| strbuild | 200,000 decimal fields, 512 epochs | Formatting, buffer growth and UTF-8 finalization; FNV of the whole string |
| channel | 100,000 round trips | Two channels and a worker; sum of every reply, worker joined |

Each row has equal weight. The command prints raw wall, CPU and counter
values, wall ratios, their geometric mean and maximum. The wall budget is
geomean <=1.5 and max <=3. A budget miss is informational and returns zero for
a complete, correct baseline. Missing counters, failed executions and unequal
output return nonzero, retain raw evidence, and make every aggregate incomplete.
A row shorter than 0.2 seconds on either side, or with a startup estimate of 5% or more, is also incomplete.
There is no passing-subset geomean. Exact stdout compares each workload's
declared checksum, not every intermediate value. JSON's length checksum does
not compare object ordering or prove complete semantic equivalence by itself.

The Hirsel sources remain in their existing benchmark directories. The
LogParser entry point calls the existing LangArena module and
harness with zero warmup. Rust's LogParser is the pinned upstream
`rust/src/benchmarks/log_parser.rs:13-177` at LangArena `3c9f84d`, under
[the existing MIT license](../langarena/LICENSE). Its algorithm is unchanged.
The adapter replaces once_cell Lazy with std LazyLock, supplies configuration
from argv and prints the checksum without timing text. Both participants
compile patterns and generate the log inside the measured process. Hirsel
materializes regex matches while Rust counts its iterator. Both perform all
13 searches, but this library representation difference remains visible.

The gzip input is byte-identical to the fixture embedded in
`micro/programs/extended/gzip_read.hir`; Python's gzip decoder independently
produces exactly 1,048,576 `a` bytes. Rust uses flate2 1.1.9's safe read adapter
and miniz_oxide 0.8.9, with SIMD Adler disabled. crc32fast is built without
`std`, so its runtime SIMD dispatch is disabled for this generic target.
Both CRC implementations use scalar baseline paths. Regex keeps its normal
accelerated byte searches; Hirsel's byte scanner also provides SSE2/AVX2.
The reference code contains no unsafe blocks or explicit SIMD intrinsics.

Channel's Rust twin uses one std thread and mpsc; Hirsel uses cooperative
scoped tasks. Both are pinned to one CPU. This measures equivalent ping-pong
work across their normal scheduling APIs. A single-thread cooperative Rust
twin is follow-up work and is outside this version's geometric mean.
The map pair likewise keeps each language's ordinary map implementation.

See [the source inventory](INVENTORY.md) for every existing benchmark family,
Rust twin, equivalence limits and current execution status. Nonselected rows
are not rerun or counted. Tests for aggregation and incomplete evidence are
`python3 tools/tests/runtime_corpus.py`. They catch misleading passing-subset
results, arithmetic means and hidden single-row regressions; their oracle is
hand-derived ratios, independent of the programs being measured.

For separate build and measurement phases, use `--build-only` followed by
`--skip-build` with the same output directory and unchanged checkout. Use a
new output directory for each session. History records corpus version, source
revision, fixed argv, profile, CPU, raw samples and acceptance summary. Only
compare records with the same corpus version and inputs. Changing any workload,
input or process boundary requires incrementing `VERSION` in the command.

Startup is checked once per executable and row with the same input but zero
repetitions. Channel uses zero round trips and still starts and joins its
worker; LogParser still prepares its log and regexes. This is a conservative
fixed-cost estimate, including the timer boundary's observer overhead. It is
reported as a fraction of total wall time, without subtraction. Both languages
use the same frozen work counts above; there is no per-language calibration.
Gzip validates each complete decode and sums their length and byte checksums.
Priority queue resets the seed and heap, drains it, and sums each complete
experiment's checksum. Their omitted repetition arguments still default to one
for the existing runners.

The version 3 baseline and startup fractions are recorded below. All rows are
fresh runs of this version; version 2 ratios are not rescaled or combined.

Measured 2026-11-09, compiler/runtime base `88f7d616`, workload and runner
revision `ebf700c5`, release profile, CPU2 with observer CPU0, jobs=1, daemon
off, 2 GiB, ordinary cache state. One fresh pair per row and one zero-work
startup sample per side. All 16 workload executions and 16 startup executions
completed; all eight workload stdout pairs matched. Every counter ran at
100%, without multiplexing.

| Workload | Wall H/R (s) | H/R wall | CPU H/R (s) | Instructions H/R | User instructions H/R | Context switches H/R | Startup H/R |
|---|---:|---:|---:|---:|---:|---:|---:|
| json | 2.3951 / 1.7535 | 1.366x | 2.3891 / 1.7428 | 26,443,942,307 / 18,161,966,358 | 25,989,737,039 / 16,107,622,176 | 59 / 7 | 0.42% / 0.39% |
| log_parser | 9.2051 / 4.1881 | 2.198x | 9.1654 / 4.1764 | 136,308,446,701 / 52,150,838,741 | 135,829,781,300 / 52,103,232,720 | 73 / 23 | 0.10% / 0.19% |
| gzip | 5.0002 / 0.8754 | 5.712x | 4.9705 / 0.8731 | 37,084,271,692 / 12,169,379,284 | 29,858,393,467 / 12,158,881,896 | 746 / 8 | 0.08% / 0.40% |
| float_math | 1.5039 / 1.6365 | 0.919x | 1.4994 / 1.6337 | 24,776,008,954 / 23,952,171,852 | 24,753,533,947 / 23,934,421,924 | 36 / 16 | 0.23% / 0.17% |
| priority_queue | 4.6700 / 0.5647 | 8.269x | 4.6628 / 0.5626 | 67,515,635,120 / 7,502,110,616 | 67,461,114,552 / 7,494,712,012 | 57 / 5 | 0.06% / 0.64% |
| map_u64 | 3.0000 / 2.1744 | 1.380x | 2.9954 / 2.1713 | 19,091,206,040 / 17,400,026,098 | 15,423,257,078 / 15,703,146,254 | 38 / 21 | 0.12% / 0.18% |
| strbuild | 4.4955 / 2.4866 | 1.808x | 4.4897 / 2.4768 | 53,843,470,901 / 33,983,181,082 | 51,238,203,950 / 33,955,246,354 | 40 / 13 | 0.16% / 0.15% |
| channel | 0.3983 / 1.2997 | 0.306x | 0.3926 / 1.2977 | 3,849,322,463 / 2,923,312,489 | 3,832,655,065 / 131,722,571 | 36 / 200,184 | 1.03% / 0.28% |
| geomean | | 1.777x | | | | | |
| max | | 8.269x | | | | | |
Wall budget: MISS (geomean <=1.5x, max <=3x).
One sample per side has unmeasured spread; near-threshold results are uncertain.

Gzip and priority_queue exceed 3x. The geometric mean exceeds its limit by
0.277x and the maximum by 5.269x. The fastest participant in any row took
0.3983 seconds; startup fractions range from 0.06% to 1.03%. The performance
miss remains visible; changing the metric does not establish Goal 11.
