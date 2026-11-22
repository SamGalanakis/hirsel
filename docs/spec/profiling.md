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

## Query results

Every JSON query includes `quality` and `result`. `quality` contains the
`run_meta` fields, an `unresolved_pct`, a `host_stack_share` between zero
and one, a `verdict` and `warnings`. A share is null when no samples were
captured. Query results encode unsigned integers above 2^53 as decimal
strings. `summary` groups hotspots by event and shows at most five rows
per event. Function grouping deduplicates the definition across all
instances in a stack.

`prof export` supports the pprof format. It preserves mapping addresses,
inline frame order, and one value per event. Values that exceed the signed
64-bit fields in pprof are refused.
