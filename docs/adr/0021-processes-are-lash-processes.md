# Processes are Lash processes

Accepted 2026-09-13.

The coordinator creates arbitrary Lashlang `process` declarations directly in its TypeScript RLM program. It registers them on Lash trigger subscriptions and their bodies call the same ordinary Hirsel tools as the surrounding coordinator turn. Hirsel does not provide a second process engine, a shell-process wrapper, or built-in recurring jobs. The previous host-owned monitor layer, its tools, storage, schema, runtime engine, and protocol discriminator are deleted.

Hirsel contributes the trigger vocabulary that depends on its domain. `timer.Schedule` produces `timer.Tick`; Lash already supplies `cron.Schedule` and `cron.Tick`. `thread.Reported`, `thread.Completed`, `thread.Messaged`, and `thread.Turned` produce `thread.Report`, `thread.Complete`, `thread.Message`, and `thread.Turn`. Their descriptors address a Thread id, while their bounded events carry that id, its title, and the durable fact's payload. Emission reuses the same self-and-descendants scope enforced by `threads.*`, so a subscription cannot observe a peer or ancestor event.

The Processes view projects each owning Thread's Lash process registry together with its trigger subscriptions. It shows process name, trigger, lifecycle, last firing, and bounded terminal outcome, and exposes fenced process cancellation and trigger disable. The focused Thread sees only itself and its visible subtree. Both the coordinator and native Lash coding worker use the TypeScript RLM posture with process and trigger abilities. The worker still has only its dedicated read/edit/write/command tool profile, and Hirsel creates no default processes for either session.

Delivery revision (2026-09-13): a process wake or terminal completion, failure, or cancellation is solicited work for the registering Thread and bypasses ADR-0015 triage. Hirsel stages a durable receipt, appends exactly one structured conversation message, then accepts a normal turn through the existing durable, idempotent Thread queue and fencing rules. The turn receives that message as context and may remain quiet. `origin` carries process identity, typed human trigger metadata from the registration snapshot retained with the trigger delivery, outcome, raw JSON result, and any error; `body` contains only the plain result or error. The web conversation renders a compact note in the ConversationNote family. Receipt retries cannot append another message or enqueue another turn, including after the original turn finishes.

Lash durably owns process registrations, events, queued wakes, and trigger subscriptions in the Thread lane's process and trigger stores. Hirsel reopens lanes that have persisted process state, resumes trigger-source polling, reconstructs the registry projection, and retries any staged message not yet appended. It does not infer that abandoned or interrupted work should restart. The Hirsel history schema is current-only; removing the old table requires the normal same-schema deployment or fresh-data handling rather than a compatibility shim.

Projection revision (2026-09-13): each process name has one stable row per
Thread, folding its subscriptions and executions. Waiting means an enabled
subscription can fire again; a suspended execution is an active incarnation
and renders as running. Completed one-shot timers are tombstoned with Lash
Delete only after successful delivery, preserving their durable registration
snapshot. Cron and interval subscriptions remain enabled between runs. Cancel
targets the newest active incarnation; Disable targets the latest enabled
recurring subscription with its revision fence. The row keeps the last firing
time and completed result, and clients remove rows that disappear from the
authoritative projection.

Execution revision (2026-09-13): the coordinator / native-worker split this
record describes is superseded by ADR-0023. There is one Native session per
Thread, holding the full Thread tool set and the four coding operations
together, so "both sessions use the TypeScript RLM posture" now reads as one
session with one posture. Nothing about process declarations, the trigger
vocabulary, delivery, durability or the projection changes: processes are still
Lash processes, owned by a concrete Thread, and Hirsel still creates none by
default.
