# C02-01 independent disposition — reject as stated

Worker proposes preserving exact persisted Host provider/model for selector-free child followups instead of following current host configuration. Source mechanism is accurate but semantic premise is not established.

Current explicit Host assignment rejects model/variant/cwd and says "host delegation uses configured provider/model; CLI selectors do not apply" at scoped_tools.rs:489–493. The accepted-backend and stored-backend wording does not promise per-child Host model freezing: a child remains Host when the globally configured Host changes. PRODUCT/ADR0016 promise immutable execution settings per accepted turn, which capture already enforces. They do not freeze settings across later turns.

The proposed one-line change would activate stale copied Host provider/model and can make later turns fail provider validation after a host restart or ignore Owner model changes. This is a behavioral policy change unsupported by the claimed docs, not an independently verified bug fix.

There may be representational waste in using full ThreadExecution::Host values to mean a Host preference that intentionally follows configuration. No material active failure beyond this redundant payload was demonstrated, so do not promote a second type/table refactor merely to remove fields. Record skip unless fresh materiality review identifies stronger evidence. C02's accepted material result remains F01/#16.

Coordinator reread storage/thread_execution.rs, storage/thread_delegation.rs, scoped_tools.rs, subagent_models.rs, tool_defs.rs, PRODUCT and ADR0016. No tests/source changes. Snapshot 3ee0621/tree a4aac830.
