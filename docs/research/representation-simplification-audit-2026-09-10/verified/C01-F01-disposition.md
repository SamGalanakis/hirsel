# C01-F01 — historical action snapshot identity

Coordinator verdict: skip; latent internal-boundary hardening, insufficient materiality for the proposed durable-shape cutover. Worker report: ../workers/C01-THREAD-IDENTITY.md. Snapshot 3ee0621.

Independently reopened ThreadActionSnapshot/From<Thread>, runtime context, the complete generated-action producer in thread_commands.rs115–245, storage/thread_messages.rs130–225, scripted.rs328–346, and all ThreadActionContext construction / append_thread_owner_request call sites. Repeated the exact six-file worker consumer query: 34 matches.

The nested snapshot can technically disagree with the outer accepted request. However the only production action-context constructor loads current using the outer id and captures that exact Thread. Admission persists both atomically; Thread identity is immutable, and no update path changes one copy. The witness requires an invented in-crate caller supplying contradictory JSON to an internal storage helper; the worker explicitly found no production producer or existing fixture demonstrating this mismatch. The scripted consumer's nested-id usage establishes potential consequence only after that fabricated input.

The snapshot intentionally records historical Thread context. Removing one id and redesigning validation touches the persisted deny-unknown-fields shape, prompt contract and replay/cutover behavior without removing current branching or a demonstrated production defect. Retain as an explicit latent skip rather than a sixth recommendation. This is distinct from F02: F02 has real network producers missing captured history and reachable delayed-frame/reset behavior, already accepted under #19.
