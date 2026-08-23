# Protocol-Compatibility Runbook Repair Report

Date: 2026-08-23
Branch: `runbook-repair`
Baseline: `6281218` (clean worktree)

## Runbook results

| Runbook | Result | Evidence / reason |
| --- | --- | --- |
| `abandoned-recovery` | needs-a-ruling | Gate `fake-driver Sub-agent running before SIGKILL`: `/debug/processes` stayed `[]` for 180s after the current `gpt-5.6-sol` request. |
| `attachment-agent-behavior` | needs-a-ruling | Gate `Agent quoted text attachment second line`: the persisted reply said `SECOND_LINE=violet-saturn-27`, not the input `SECOND-LINE-TOKEN-8842`. |
| `compaction` | PASS | All gates passed, including post-compaction recall. |
| `delegation-loop` | needs-a-ruling | Gate `Quick Reply auto-resolved Ping`: the anchor-refed reply left the Ping `open`; the current contract settles only through an explicit Event action. |
| `event-archive-undo` | PASS | All gates passed. |
| `event-snooze-sweep` | PASS | All gates passed. |
| `model-selection` | PASS | All four gates passed after the current catalog/model-shape repairs. |
| `multi-turn-memory` | PASS | All gates passed, including recall after restart. |
| `ping-read` | PASS | All gates passed, including read persistence after restart. |
| `pings-lifecycle` | needs-a-ruling | Gate `anchor-refed Owner reply auto-resolved Ping`: the reply preserved its anchor but the Ping stayed `open`; auto-resolution is no longer the current lifecycle contract. |
| `push-discipline` | PASS | All gates passed. |
| `real-subagent` | needs-a-ruling | Gate `real Codex process visible`: `/debug/processes` stayed `[]` for 240s even after changing the stale model request to `gpt-5.6-sol`. |
| `side-chats` | needs-a-ruling | Gate `confirmed conclusion auto-resolved Ping`: the conclusion left the Ping `open`; current `confirm-conclusion` closes the side thread without settling its Task. |
| `views-lifecycle` | needs-a-ruling | Gate `ping-anchored view event resolves Event`: the anchored Event stayed `open`; current lifecycle requires explicit settling. |

## Repairs

- `lib/runbook-lib.sh`: corrected `repo_root()` from `lib_dir/../..` to
  `lib_dir/../../..`. The only derived use is the host's
  `HIRSEL_TEMPLATES_DIR`, which now resolves to the repository `templates/`
  directory.
- `model-selection`: changed Gate 2 from `default_variant` to
  `enabled_variants`. The current registry has `claude-opus-5` with only the
  `high` variant, so the gate now disables that row with
  `enabled_variants:["high"]` and asserts the returned catalog,
  `subagent_models_changed` broadcast, persisted catalog, and fresh
  `hello_ok` snapshot.
- Updated the compatibility debug-surface documentation to describe the
  `enabled_variants` request field.
- Updated stale explicit Codex Sub-agent model requests/assertions from
  `gpt-5.5` to the current `gpt-5.6-sol` lane in `abandoned-recovery` and
  `real-subagent`.

No Host or protocol production code was changed. The seven needs-a-ruling
rows were left intact because their failing behavior is feature/lifecycle or
Agent judgment drift, not a mechanical wire-field mismatch.

## Verification

- All 14 runnable scripts were executed sequentially with output captured
  under `/tmp/hirsel-runbook-repair/`.
- `cargo test --workspace`: PASS.
- Shell syntax and `git diff --check`: PASS.
- Pre-commit: PASS (`uvx pre-commit run --all-files`).

Commit hash: the single commit containing this report (`git rev-parse HEAD`).
