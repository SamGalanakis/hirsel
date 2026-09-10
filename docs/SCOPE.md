# Current scope

Hirsel is a single Owner's workspace of nested Threads with scoped agents coordinating their own subtrees. PRODUCT.md, DESIGN.md and ADR 0016 describe the current domain.

- Threads own conversation, explicit instruments, execution turns and factual activity. Settlement, attention, visibility and read state remain independent.
- The web client provides an unaddressed overview, a compact icon rail and nested Thread drawer with independent pin shortcuts, a framed focused conversation, per-message execution disclosures, search, lifecycle controls and independent drafts.
- Explicit global artifacts support Solid 2, HTML and UTF-8 files. Markdown files render safely; source downloads remain available. Previews have local interaction without network or backend capabilities.
- Generated Thread instruments retain constrained JSON vocabulary and displayed revision checks. Current plugin Canvas Views, Processes and Settings remain operational surfaces.
- The current-only protocol uses tagged authentication, required history identity and owning Thread/turn IDs. Reconnect fetches the selected Thread; reset isolates old drafts and invalidates pending operations. Human artifact references use required `artifact_ids`: preview stages one removable history-and-Thread-bound draft context, and message acceptance atomically records validated references. Browsing alone grants no agent access.
- Current validation is frontend unit/contract tests plus isolated Thread/artifact browser smoke. Native clients use the same Thread contract.

Agents receive host-enforced access to their own Thread and descendants through caller-relative tools; IDs and paths cannot widen that scope. Humans can navigate the full tree. Delegation creates focused direct-child conversations with isolated accepted briefs and durable reports to their recorded parent. Explicit artifact references allow shared current-content access and CAS edits without exposing a foreign conversation. No separate Project or Namespace entity, artifact ownership/revisions, or artifact JavaScript backend bridge is introduced.

Agent-invoked plugins receive Thread and KV capabilities bound to the active execution. Plugin activity requires `NewActivity::new(thread_id, kind, data)` with an explicit existing destination; host-managed daemon capabilities remain separate. No Thread ID, including 0, is reserved or used as an implicit destination.

Background scheduling and execution processes have explicit Thread destinations. Completing a process or turn does not settle work. Deployment, provider calls and data resets require their own explicitly authorized scope.
