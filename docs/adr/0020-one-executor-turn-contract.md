# Host, native workers and CLI backends use one turn contract

Accepted 2026-09-13.

Hirsel keeps three execution runtimes. `Host` is the coordinator Lash RLM session, `LashWorker` is the in-process Lash coding worker, and `Cli` runs Claude or Codex through `hirsel-drivers`. Runtime choice remains a product capability; it no longer chooses a timeline dialect.

Every backend translates into the host-owned `ExecutorEvent` vocabulary: started provenance, prose and reasoning deltas, paired tool start/done events, paired code start/done events, a final assistant value, diagnostics, and one terminal outcome (`Done`, `Failed`, `Interrupted`, or `Cancelled`). `TurnIngest` alone batches text, condenses and bounds payloads, appends and broadcasts durable `TurnEventKind` rows, records one `tool_completed` fact per call ID, derives the retained ephemeral activity frame, and projects the terminal chat message and turn state. The existing Lash commit barrier still prevents successful Host completion from overtaking durable observation replay.

The Host adapter translates `RemoteSessionObservationEventPayload`; the native worker validates `TurnActivity` through Lash's remote representation and then applies the same translation; the CLI adapter translates `SubagentEvent`. Adapters do not write activities, frames, timeline rows, or terminal state. Scoped Hirsel MCP callbacks retain their guarded append because the execution lease and tool start must stay on the same side of cancellation, but their completion fact uses the same ingest operation and call-ID key.

The CLI-only progress activity is removed. Provider summaries that are genuine operational warnings become one rate-limited `execution_diagnostic`; prose, reasoning, tools, final output, and running labels come from the turn stream and turn state. The separately implemented Host/native observation-to-activity path is deleted; any retained `agent_activity` frame is derived only by `TurnIngest`.

Executor conformance is a table-driven host test. Every backend must drive the same success, failure, and cancellation scripts through its real adapter and produce identical normalized persisted events, `tool_completed` facts, final tool summaries, terminal state, and broadcast frame sequence. Adding a fourth backend means adding one adapter entry to that table, not a new projection path.
