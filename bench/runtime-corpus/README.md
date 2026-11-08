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

The host rules require one sample per side. This is a sample, with unmeasured
spread, and cannot establish timing stability. On other hosts, `--repeats N`
supports alternating pair order, medians and percentage min-max spreads for
both metrics. Raw perf counters include their running percentages and SMT
sibling activity. `instructions:u` counts the complete process tree and is
the primary comparison; perf's elapsed summary is secondary wall time. The
counted process includes taskset's placement/exec before the workload. Linux
6.8 `tools/perf/builtin-stat.c:872-884` starts the child before its elapsed
clock. Pinning observer and child together lets a short child finish before
that clock starts, corrupting both duration_time and elapsed summary. The
observer therefore runs on another CPU. Its CPU is recorded with each sample.
Within-session measurements do not remove shared-host or build-order effects.

| Workload | Fixed input | Cost profile and checked result |
|---|---|---|
| JSON | 2,000 rows, 2 epochs, 10 round trips each | DOM parsing, allocation and serialization; sum of serialized lengths |
| LogParser | 10,000 log lines, 10 scans per pattern | Regex parsing/search, match allocation and count map; total of 13 pattern counts |
| gzip | 1 MiB of `a`, gzip level 9, reads of 4,096 bytes | Streaming DEFLATE/Huffman/LZ77 and CRC validation; decoded length and sum of every byte |
| float_math | 100,000 x 64 loop steps, 2 epochs | floor/ceil/trunc/round/sqrt/abs and floating-point accumulation; exact final IEEE bits |
| priority_queue | min-heap capacity 1,024, 131,072 items, LCG seed 42 | Combined push/pop and drain; ordered returned-value checksum |
| map_u64 | 200,000 keys, 2 epochs | Hash table growth, insertion and lookup; sum of values plus map length |
| strbuild | 200,000 decimal fields, 2 epochs | Formatting, buffer growth and UTF-8 finalization; FNV of the whole string |
| channel | 10,000 round trips | Two channels and a worker; sum of every reply, worker joined |

Each row has equal weight. The command prints per-row Hirsel/Rust instruction
and wall ratios, their geometric means and maxima. The instruction budget is
geomean <=1.5 and max <=3. A budget miss is informational and returns zero for
a complete, correct baseline. Missing counters, failed executions and unequal
output return nonzero, retain raw evidence, and make both aggregates incomplete.
There is no passing-subset geomean. Exact stdout compares each workload's
declared checksum, not every intermediate value. JSON's length checksum does
not compare object ordering or prove complete semantic equivalence by itself.

The Hirsel sources remain in their existing benchmark directories. Only the
LogParser entry point is new; it calls the existing LangArena module and
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
work across their normal scheduling APIs, not equal scheduler internals.
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
