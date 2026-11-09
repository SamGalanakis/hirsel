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
