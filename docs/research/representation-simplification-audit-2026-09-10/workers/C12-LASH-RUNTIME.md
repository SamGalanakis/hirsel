# C12-LASH-RUNTIME

Snapshot verified before review: `HEAD=3ee0621a603659ab0168f565b99012b642415419`, tree `a4aac830c45398a66591f2c44b707aaf3cef281b`; `git status --porcelain` was empty. All eight owned files were read in full: `lash_runtime.rs`, `lash_runtime/{condense,executor,tool_defs,tool_results,tool_schemas,tests}.rs`, and `tools.rs`. The listed runtime, bridge, storage, monitor, process, thread-tool, and test consumers were inspected with targeted reads. No tests/builds/application commands were run.

## F1 — monitor condition is an invalid, weakly validated product state

**Verdict: recommend. Confidence: high.** The model contract makes `wake_on` and `pattern` independent (`tool_defs.rs:311-325`): all four wake values are accepted and `pattern` is merely an optional string. The output contract repeats the same split (`tool_schemas.rs:71-103`). Parsing only maps the string to `MonitorWakeOn` (`executor.rs:161-170`), and the scoped call passes the two independent values directly (`scoped_tools.rs:100-113`).

Two reachable invalid states follow. `{"wake_on":"changed","pattern":"ready"}` passes schema and parser, is accepted by storage (`storage/monitors.rs:275-287`), and persists an ignored pattern; `{"wake_on":"regex","pattern":"["}` also persists because validation checks only nonempty (`storage/monitors.rs:282-286`), then the runner silently converts the invalid regex to “never wakes” (`monitors.rs:78-90`). The SQL table has no invariant (`storage/current.sql:63-77`). Existing fixtures cover only valid Changed/none (`lash_runtime/resource_tests.rs:34-40`) and Regex/`ready` (`lash_runtime/tests.rs:678-695`); no invalid combination is demonstrated.

No duplicate-copy write path was found: both fields are constructed on one `MonitorRecord` and inserted together (`storage/monitors.rs:29-47`, `81-112`). This is a split state-machine defect, not two independently updated copies. The monitor-path query `rg -n 'wake_on|pattern' crates/hirsel-host/src/lash_runtime crates/hirsel-host/src/tools.rs` returns 17 lines in 7 files; the adjacent conversion/run query `rg -n 'MonitorWakeOn|wake_on|pattern' crates/hirsel-host/src/storage/monitors.rs crates/hirsel-host/src/monitors.rs crates/hirsel-host/src/process_run.rs crates/hirsel-host/src/debug.rs` returns 47 lines in 4 files.

Target the condition as one value at each layer: a wire `oneOf` of `{wake_on: changed|exit_zero|exit_nonzero}` with no `pattern`, or `{wake_on: regex, pattern: nonempty valid regex}`; a host `MonitorCondition::{Changed,ExitZero,ExitNonzero,Regex(ValidatedRegex)}` parsed/compiled before storage; and a durable `CHECK` requiring regex plus nonempty pattern or non-regex plus NULL pattern. Map the enum to existing columns only after validation; have the runner consume the validated variant. Smallest seam is `tool_defs.rs`, `tool_schemas.rs`, `executor.rs`, and their `scoped_tools.rs` callsite, with adjacent storage/schema/run enforcement. This removes both unreachable combinations and silent regex suppression. Risk is intentional rejection of previously accepted ignored/malformed inputs and handling any legacy rows; direct current-schema cutover has no migration path. Add parser/schema cases for all valid variants, extra-pattern rejection, missing/empty/malformed regex rejection, and durable CHECK/runner coverage; inspect only in this phase.

## Explicit skips

- Observation retry, routing, timeline pairing, and terminal publication were inspected; no new finding beyond excluded root #13 mechanisms and the settled runtime ownership.
- Session bootstrap, tool-surface fingerprinting, dynamic plugin catalog, and `ToolSuite` cloning/reset were inspected; fingerprints/names are derived together, and cloned model state shares its `ConfigStore`.
- Shell/process status tuples are weakly representable, but no current owned producer emits `timed_out=true` with an exit status; the conversion concern is in consumer-owned `tools/shell.rs`, so it is skipped.
- Generic tool result schemas, thread/delegation schemas, timers, task cleanup, and view schemas were inspected; remaining concerns are deliberate or assigned to adjacent C05/C08/C23/C24 owners.

After review: `git rev-parse HEAD HEAD^{tree}` still reports the expected commit/tree and `git status --porcelain` remains empty; source is unchanged.
