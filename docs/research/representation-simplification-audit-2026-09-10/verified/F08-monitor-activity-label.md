# F08 — monitor polling is falsely labeled as firing

Recommend only the narrow display correction; medium confidence/materiality. Owner C08; worker C08-02 ../workers/C08-PROCESS-WAKE.md. Source 3ee0621. Reopened monitor_should_wake, both tick storage writers, cancellation writer, process engine wake branch, ProcessInfo projection, ProcessRow and ProcessesView consumer. Exact timestamp query repeated: 38 matches.

A first Changed probe has no prior output and wake=false. Both production paths record last_run_ts/last_event_ts before branching on tick.wake. ProcessRow treats last_event_ts != started_ts as hasFired and displays last fired. Cancellation also advances last_event_ts. The UI thus claims an agent wake after an ordinary non-waking check or cancellation. No test was run; source state transition is direct.

Narrow target: call this field what it is—latest activity/update—and remove hasFired inference. The expanded row already correctly labels the same field Updated. No product/ADR requirement found to display actual wake history, so do not add the worker's proposed last_wake_ts schema/wire/state field or modify wake persistence. If actual firing history becomes a product requirement, that is a separate explicit contract with delivery semantics, not a prerequisite to stop the current false claim. Meaningful validation: a non-waking probe and cancelled monitor must not render last fired; actual activity timestamp remains visible. Scope ProcessRow.tsx plus focused UI assertion; optional stale monitor-only contract cleanup belongs to root's wholehog decision.

Independent materiality priority: low. This is a bounded display/wholehog cleanup correction, not peer severity to durable admission or captured identity defects.
