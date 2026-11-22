# CPU profiling

## Native unwind information

LLVM emits asynchronous `.eh_frame` unwind information for defined Hirsel
functions on x86-64 and AArch64. Hirsel failures still use the call ABI.
Linker garbage collection discards the unwind records of discarded code.
Release builds can reuse whole modules on both architectures. x86-64 ELF
also supports splitting and reusing individual definitions. AArch64 emits
and reuses whole modules only.

Cranelift emits `.eh_frame` for every native definition, including the entry
adapter and foreign-call glue. It uses PC-relative FDE addresses so the
same objects link into position-independent executables on both targets.

Both architectures mark `fiber_start` and `switch` as stack boundaries. Their
unwind records use the active stack pointer as the CFA and mark the return
address undefined. An unwinder stops there, including during a stack switch.
LLVM's inline stack-probe loops can use a temporary CFA register (`r11` on
x86-64 and `x9` on AArch64). A sample inside such a range with that register
absent is `eval_error` with stack root `failed`.

Across 26 x86-64 AWFY and LangArena executables, the final builds had an FDE
for each of the 1,420 Hirsel function symbols and all 52 startup symbols,
including `main` and `__hirsel_entry`. Compared with the pre-change builds,
`.eh_frame` grew by 119,500 bytes (4.53% of Hirsel function text) and
`.eh_frame_hdr` grew by 12,216 bytes. The largest instruction increase across
the 72 workloads was 0.3401% (AWFY Json), within the 0.5% Guard B threshold.
Cross-linked AArch64 images verified FDE coverage for whole and reused
modules with both backends.

## DWARF and symbol resolution

Both compilers extract source-relative debug rows when they emit native code.
The LLVM release cache identifies locations by emitted instruction order, so
source-coordinate edits can reuse code while rebinding its debug rows.
The link step resolves these rows to file, line and column, with the inlined
call chain and Hirsel definition path where available. It writes a separate
ELF DWARF file beside the executable. A GNU build ID selects
`.build-id/XX/REST.debug`, where `XX` is the first byte in hex. If the image
has no GNU build ID, its exact byte digest selects `.hirsel-debug/DIGEST.debug`.
`--debug-lines on` also embeds the DWARF. Object emission is the same with
either flag setting.

With cold native caches, always-on extraction and binding increased combined
release build time for the x86-64 corpus from 268.18 s to 270.50 s (+0.87%).
AWFY with embedded DWARF took 123.00 s before and 119.18 s after. LangArena
with default flags took 145.18 s before and 151.32 s after.

A resolver reports `full_dwarf` when the image embeds DWARF or the detached
file matches its GNU build ID or exact image digest. `symtab_only` provides
ELF function names when the image has a symbol table. `offsets_only` reports
the mapping and relative PC when neither source is available.

## Profile file

A `.hprof` is a self-contained SQLite database with `application_id =
0x48505246` and `user_version = 1`. Readers open the current version read-only.
Source snippets and symbol names are recorded in the database so it can travel
without the source tree, executable or debug files.

The database has the following tables and columns.

| Table | Columns |
| --- | --- |
| `run_meta` | `key, value` |
| `mapping` | `id, start_addr_hex, end_addr_hex, file_offset, path, build_id, image_digest, start_ns, end_ns` |
| `thread` | `id, pid, tid, name, start_ns, end_ns` |
| `function` | `id, def_path, instance_label, name, module_kind` |
| `frame` | `id, mapping_id, rel_pc, function_id, file, line, col, inlined` |
| `callsite` | `id, parent_id, frame_id, depth` |
| `cpu_sample` | `id, ts_ns, thread_id, task_id, cpu, callsite_id, raw_pc_hex, period, stack_root, unwind_status` |
| `sample_counter` | `sample_id, event, value` |
| `source_snippet` | `file, start_line, end_line, text` |
| `fn_cpu_summary` | `function_id, event, self_total, inclusive_total, sample_count` |
| `line_cpu_summary` | `file, line, function_id, event, self_total, sample_count` |
| `codegen_remark` | `function_id, file, line, col, kind, outcome, evidence_level, count` |
| `peak_snapshot` | `id, metric, scope_id, ts_ns, total_bytes` |
| `peak_callsite` | `snapshot_id, callsite_id, live_bytes, live_count` |
| `mem_alloc_agg` | `callsite_id, allocation_kind, observed_count, observed_bytes, estimated_count, estimated_bytes, freed_count, lifetime_ns` |
| `budget_scope_summary` | `scope_id, parent_id, scope_kind, task_id, callsite_id, enter_ts, exit_ts, limit_val, peak_val, refused` |

`run_meta` has the keys `hprof_version`, `capture_mode`, `target`, `cmdline`,
`build_id`, `image_digest`, `symbol_tier`, `clock`, `primary_event`,
`primary_unit`, `freq_hz`, `fallback_reason`, `start_ns`, `duration_ns`,
`samples_total`, `samples_lost`, `stacks_truncated`, `unwind_failed`,
`unresolved_samples` and `host_root_samples`.

Addresses in `*_hex` columns are hexadecimal text. Unsigned numeric values
above the SQLite signed integer range use exact decimal text. Time is in
nanoseconds. An unresolved sample has no function for its leaf frame.

A callsite identifies a frame in its caller chain. A self total counts the
event periods attributed to the leaf frame. An inclusive total counts each
sample's event period once per function on that chain, even when recursion
or inlining repeats the function. The function sample count uses that same
membership rule. Line summaries attribute self totals only.

## Linux sampling

`hirsel prof run` builds a native executable and opens the perf events before
releasing the child to exec. Linux enables the events at exec. Sampling uses
`CLOCK_MONOTONIC`, per-CPU mmap rings, and a timestamp merge across CPUs. The
capture records kernel-reported losses. The default frequency is 997 Hz and
the default user stack snapshot is 32 KiB.

The default event is `cycles`. The profiler uses `cpu-clock` if the PMU lacks
hardware cycles and records the reason in `fallback_reason`. Each `--pmu`
event contributes its own samples with that event's period in
`sample_counter`. Instruction profiles use the `instructions` event directly.

The unwinder reads ELF CFI with gimli and uses the captured stack and
registers. It leaves the sampled instruction pointer intact and subtracts
one byte once from caller return addresses. x86-64 captures IP, SP and BP.
AArch64 captures PC, LR, SP and X29.

`stack_root` is `fiber`, `host`, `truncated` or `failed`. A host stack
can contain a Foreign or OS operation whose calling fiber is unreachable.
`unwind_status` is `ok`, `fiber_root`, `truncated_stack`, `missing_cfi`,
`unknown_mapping` or `eval_error`. An undefined return address ends the
walk. Missing CFI may permit frame-pointer recovery, but the sample retains
`missing_cfi` and a `failed` root. Expression rules and CFA rules that
require uncaptured registers, such as a stack-probe scratch register,
produce `eval_error` with a `failed` root.

`symbol_tier` is `full_dwarf` for matched detached or embedded DWARF,
`symtab_only` for ELF symbols, or `offsets_only` if only mapping-relative
addresses are available. A function's `module_kind` is `hirsel`, `runtime`,
`foreign`, `kernel` or `unknown`.

`hirsel prof attach` enables mapping, command and task records before it
reads the target's thread list or mappings. It snapshots the vDSO build ID
and enables CPU sampling after recording the initial mappings.
The profiler opens per-CPU events until the CPU count times the target's
thread count plus one reaches 1000, when it uses one set of events per thread
that follows the thread across CPUs.

## Replay memory and budgets

`prof replay --mem` observes a serial replay using the recorded tape's
divergence check. `--mem=sampled` samples backing allocations with a 512 KiB
mean geometric byte interval. An allocation of `n` bytes is included with
probability `1 - (1 - 1/524288)^n`. The estimate weights its count and bytes
by the inverse of that probability. `--mem=exact` records every observed
backing lifetime and tracks the peak membership in bounded runtime storage.
The offline importer unwinds the captured registers and stack bytes with
the same CFI evaluator as CPU profiles, then aggregates by callsite.

Memory uses metric version 2. `charged_bytes` counts MEM-19 reservations
from the root, including bounds, tasks, grants and startup. `retained_bytes`
counts live backing requests. `slack_bytes` counts unassigned region chunk
tails. The metrics have separate snapshots because their peaks can occur
at different times. Bytes that cannot be assigned to an allocation site,
including root reservations, appear under an unattributed callsite.

Before any runtime credit increases a payer's available balance, the
observer samples the balance. It also samples at scope exit. This captures
the peak after inline debits without emitting `observe_charge`. Attribution
of those debits is at refill or scope granularity. An elided bound shares
the Capacity raise with other operations. Such raises can appear as
`capacity_unattributed`, with a raise stack and null parent and limit.
Exact source-scope coverage requires the separate `--mem=exact-logical`
observation mode, which is reserved for future work.

Budget rows retain all observed refusals and the 20 highest near-misses
at or above 80% of their limit. An unopened or still-open scope has a null
exit timestamp. A recorded parent may fall below the reporting threshold.

Exact capture can generate a stack snapshot for each allocation. Set
`HIRSEL_PROFILE_CHUNK_BYTES` to size the two per-worker chunks that all
runtime streams share. The default is 65536 bytes per chunk. A full chunk
can cause event loss, reported in `mem_records_lost`. Runtime state uses at
most 65536 entries per collection by default. `HIRSEL_PROFILE_MEM_ENTRIES`
can raise that limit up to 1048576. State loss is reported separately.
Dropping allocation or free events marks callsite aggregation inexact.
The final runtime peak counters are independent of those transport losses.

The memory observer depends on one replay worker owning region tails and
observing the order of payer credits. Live workers can change both
concurrently, and forcing them to run serially would change scheduling.
`prof run --mem` and `prof attach --mem` ask for a tape and a replay capture.

## Query results

Every JSON query includes `quality` and `result`. `quality` contains the
`run_meta` fields, an `unresolved_pct`, a `host_stack_share` between zero
and one, a `verdict` and `warnings`. A share is null when no samples were
captured. Query results encode unsigned integers above 2^53 as decimal
strings. `summary` groups hotspots by event and shows at most five rows
per event. Function grouping deduplicates the definition across all
instances in a stack. A frame with a source location and no function name
appears as `<file>:<line>` in hotspots.

`annotate FILE:LINE` uses an exact stored path if present. Otherwise it accepts
a unique path suffix on component boundaries. An ambiguous suffix reports the
matching paths, even if only one contains observations for the requested line.

`annotate` aggregates CPU events by source line and event. It aggregates
compiler remarks by source line, kind, outcome and evidence level. It
aggregates backing allocation totals by source line and allocation kind.
The join reports each CPU sample period once even when the line has several
remarks or kinds of allocation. Lines with no CPU observations have an empty
`cpu` array.

LLVM release builds record source remarks when `CounterRangePass` folds a
proven overflow flag or a branch to a known Bounds cause, or when the cold
step-sync pass coalesces a flush, refill and reload. The rows use
`evidence_level = 'source_remark'`. They describe those pass decisions.
Counts can include multiple compiled instances at one source position.
The linked image's debug product binds the remarks to the current source
coordinates and carries them into the profile even when the line gets no
CPU samples.

`prof export` supports the pprof format. It preserves mapping addresses,
inline frame order, and one value per event. Values that exceed the signed
64-bit fields in pprof are refused.

## Runtime archives

The default runtime archive compiles out runtime profiling. Installations
register separate profiling and profiling-with-recording archives for each
target. The recording entry reads and clears recording environment variables
even when there is no recording destination. Live profiling can use the
profiling archive without paying for that startup path.

The chosen archive's content identity participates in native object, release
part, release std and executable assembly cache keys. Runtime archive
selection preserves semantic code addresses and `hirsel_code_revision`.

## Task, host-stack and I/O observations

`hirsel prof run --tasks` adds task lifetimes, state and dispatch intervals,
measured wakes, host-stack intervals and provider I/O operations. A task row
has a scheduler-local runtime ID and a profile-wide ID. A causal edge records
the witnessed source, destination and object. Timer, external and unresolved
wakes end the measured chain. Latest reads and publishes do not park.

Each worker appends to two preallocated host chunks outside the program's
memory budget. `HIRSEL_PROFILE_CHUNK_BYTES` sets each chunk's size in
bytes for all runtime streams, from 65,536 bytes (the default) to 67,108,864
bytes. A drain thread writes sealed chunks. A full buffer drops the
new record and the profile reports the per-worker total as
`runtime_events_lost`. An incomplete stream reports that its final loss count
may be missing. Observations take no scheduler turn and add no tape event.

For a host-stack sample, import pairs the thread ID and monotonic timestamp
with the calling task and source site in `host_stack_interval`. The launch
records executable mappings from `/proc/self/maps` before program operations.
Live profiles use perf mapping records for libraries loaded by audited C.
If sampling fails, the profiler tries a mapping-only perf event. If that also
fails, only the startup mappings are available. A PC outside the known
mappings has status `unknown_mapping`.

`io_op` records the request's site, delivered byte count and error. Time spent
in the wait, issue, resume, deliver and abort callbacks contributes to
`callback_ns`. Time waiting for readiness or a deadline contributes to
`pending_ns`. The total operation interval can also include scheduler delay.

## Tape replay

`hirsel prof replay --tape TAPE --tasks` records the same runtime task
observations during serial replay. The regular replay verifier checks
revision, event positions and outcome. A refusal or divergence leaves no
profile. Replay records no CPU samples. Its I/O timing describes the
replay callbacks and passes through the tape provider.

Replay starts with executable mappings from `/proc/self/maps`. FFI-8
refuses programs that reach Foreign code before replay, and the replay
runtime does not dynamically load libraries.

## Task latency

`hirsel prof tail` counts completed tasks and tasks censored by a missing
start or end. The p50, p95 and p99 are nearest-rank percentiles in
nanoseconds over completed lifetimes. Each lifetime runs from task creation
through its end. With no completed lifetime, the percentiles are null.
The result includes measured causal edges, the count of wakes whose source
does not identify an observed task and any truncation from `--limit`.
External and timer wakes stop causal provenance. The query reports
individual measured edges and leaves critical paths unspecified.
