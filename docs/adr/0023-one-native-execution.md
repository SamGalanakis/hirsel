# One Native execution

Accepted 2026-09-13.

A Thread runs on exactly one of three backends: **Native**, the **Claude CLI**,
or the **Codex CLI**. Native is Hirsel's own RLM session on one provider
instance and one model. The earlier distinction between a coordinator session
and a separate native Lash coding worker (ADR-0019, ADR-0021) is deleted: it
asked the Owner to answer a question about Hirsel's internals — which of our two
sessions should hold this Thread — in order to get a file edited. The word
"coordinator" is gone from the product, and so is the "native worker" settings
row, its provider capture, its catalog entry, its session type and its prompts.

The public target is `ThreadExecutionTarget::{Native{provider_id, model},
Cli{agent, model, variant}}`. `Host` and `Lash` are removed with no shim. The
Owner names a Native route in Thread Info's "Runs on" row and the Agent names
the same thing as `threads.delegate` with `agent: "native"`; one resolver
validates both against the same provider roster, so they accept and refuse
identically. Native takes no reasoning variant.

One session means one tool surface. `hirsel_tool_definitions` advertises the
Thread tool set and the four coding operations (`read`, `edit`, `write`,
`exec_command`) together, so the advertised surface, the rotation fingerprint
and the execute routing cannot drift apart. `cwd` survives as captured state,
not as part of the public target: the coding operations need a directory to be
rooted at, a per-lane binding re-roots them at each admission and reaps the
previous root's children, and it remains execution context rather than a
filesystem sandbox.

Delegation inherits. A child that names neither provider nor model runs where
its parent runs; the Settings default answers only when the parent has no Native
route of its own, and an explicit `provider_id` or `model` still wins. The
default answer to "where does this run" is "here".

The stored turn capture encodes the backend discriminant, so this is a breaking
schema change (`SCHEMA_VERSION` 9 → 10) with no migration, per the project's
current-only schema rule.
