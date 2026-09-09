# Threads own conversation and work

Accepted 2026-09-09 following the T3 Code prospect and the Owner's explicit decision to adopt its domain model.

Threads replace notification-shaped Tasks as the durable work identity. Each Message belongs to exactly one Thread; Turns and Activity belong to that same subject. The Orchestrator conversation is a standing Thread for global coordination. The globally aware Agent can read and update other Threads explicitly. A citation is a reference, never a second owner of a Message.

Thread creation establishes visible identity before an execution, decision, or generated instrument exists. Attention, execution, read state, visibility, and active/settled lifecycle are separate dimensions. Settlement and reopening are explicit. Reading, replying, a successful Turn, or a quiet update never settles a Thread. Automatic inactivity/PR settlement is not adopted.

Hirsel retains Lash, SQLite, native Sub-agent Drivers, and generated instruments. This adopts T3 Code's domain boundaries, not its runtime implementation, provider/worktree options, or automatic settlement policy. See docs/research/prospect-task-model-2026-09-09/ for pinned source evidence.

This supersedes the global-transcript/Task-margin ownership rule in the earlier product direction, the typed Event work-object basis of ADR-0012, and the visible Task terminology in ADR-0004/0009/0013. ADR-0004's prohibition on mechanical workflow/retry policy remains. Durable Thread records are product context, not execution specifications.

Existing work and conversation are preserved on upgrade. Unambiguous legacy reply chains are assigned to their Thread; ambiguous or ambient history remains in the Orchestrator conversation. Import is one-time and transactional. New work uses the Thread contract only.
