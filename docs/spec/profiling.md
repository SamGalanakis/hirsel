# CPU profiling

## Native unwind information

LLVM emits asynchronous `.eh_frame` unwind information for defined Hirsel
functions on x86-64 and AArch64. Hirsel failures still use the call ABI.
Linker garbage collection discards the unwind records of discarded code.
Release builds can reuse whole modules on both architectures. x86-64 ELF
also supports splitting and reusing individual definitions.

Both architectures mark `fiber_start` and `switch` as stack boundaries. Their
unwind records use the active stack pointer as the CFA and mark the return
address undefined. An unwinder stops there, including during a stack switch.
LLVM's inline stack-probe loops can use a temporary CFA register (`r11` on
x86-64 and `x9` on AArch64). A sample inside such a range with that register
absent is `eval_error` with stack root `failed`.

Across 26 x86-64 AWFY and LangArena executables, LLVM and fiber unwind
metadata added 112,792 bytes of `.eh_frame` (4.26% of Hirsel function text)
and 11,680 bytes of `.eh_frame_hdr`. Every one of the 1,408 Hirsel function
symbols in those images had FDE coverage. The largest instruction increase
across the 72 checked workloads was 0.0793%.

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
