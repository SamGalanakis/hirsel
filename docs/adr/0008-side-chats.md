# Side threads: seeded forks that produce the Owner's reply

> **Superseded for product UI and default runtime (2026-07-23):** Hirsel no longer exposes Side Chats or seeded conversational forks. A Task dive shows related conversation in its margin; its removable composer scope lets the Owner speak either about that Task or to global Hirsel without leaving the Task. Legacy side-session host frames/debug routes are disabled by default and run only with `HIRSEL_COMPAT_SIDE_SESSIONS=1`; their Lash session and TTL reaper start lazily on the first legacy open frame. See `PRODUCT.md`, `DESIGN.md`, and `docs/product-direction.md`.
>
> **Sunset criterion:** delete the retained frames, storage, manager, and
> compatibility runbook together once the oldest supported client release no
> longer emits a side-session frame and the compatibility suite is removed
> from the supported protocol matrix. Sub-agent/process execution is a
> separate global-Agent capability and is not part of that decision.

A Task can be opened into an ephemeral side thread scoped to that Task. The fork is a seeded scope, not a transcript copy: a fresh Lash session receives the Agent prompt, Task and Anchor exchange, and a bounded window of recent global context. The side Agent drafts a conclusion; the Owner edits and confirms it; the result lands in the main thread as the Owner's Anchor-refed reply. Confirming never settles the Task—only an explicit Event action does—then the ephemeral side transcript is discarded. This preserves deep dives without turning navigation into a nested labyrinth or losing the global orchestrator's context.
