# Constrained JSON Thread instruments and plugin views

Threads carry a constrained JSON component tree in `instrument`; plugin views carry the same validated display vocabulary in `spec`. The host validates producer-authored instruments and structured actions. Clients render declared controls, and generated Thread actions carry the instrument revision they were displayed from. Undeclared/stale actions fail before accepting work. A continuation updates the existing Thread; only a control explicitly marked to settle completes it.

The component vocabulary is intentionally closed. It supports text, layout, statuses, options and forms without executing generated code. Plugin view events remain separate current capabilities; canvas/chat view placements address the coordinator. There is no Event/Ping inventory, Event action alias, or side-session placement.

Explicit artifacts are a separate global content feature, documented in [ADR0017](0017-explicit-global-artifacts.md). Artifact previews run in client isolation and have no backend tool bridge.
