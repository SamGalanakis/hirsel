# Scoped survey and exclusion map

Hirsel is a Rust Host/SQLite agent workspace with a Solid web client and a shared Rust/native Android client. Threads are durable conversations with parent/child delegation, per-thread agents, tools and generated instruments; globally shared artifacts can be referenced and showcased. Scope here is domain terminology, ongoing organizers versus finishable work, completion authority, and the sidebar UX that expresses them. Preserve stable identity/message ownership, explicit completion separate from execution/attention/read state, and scoped access/reconnect consistency. No runtime, CLI, migration, security or generic UI audit in this round.

## Current open GitHub issues

- #50: Research ongoing conversations, organizers and finishable work terminology (this prospect tracking target; no design implementation approved)

- #49: Adapt the thread sidebar, chat and artifact layout to available width
- #48: Render visual artifacts by default with a Rendered / Source toggle
- #46: Replay durable turn timelines in the native client and Android chat
- #45: Persist the inline turn timeline across reload and reconnect
- #44: Add and execute agent-driven chat and artifact runbooks
- #43: Diagnose and fix artifact creation failure in live conversation
- #42: Recognize Linear issue links in conversations and Related
- #41: Use one native Android build recipe for CI and release
- #40: Remove retired Task harness and obsolete smoke runbook
- #39: Project current Thread status in mock create receipts
- #38: Apply WebSocket auth throttling to stable peer IPs
- #37: Reject duplicate field names within a view form
- #36: Keep device push registration current across token refresh and history reset
- #35: Include captured history in actual FCM notification data
- #34: Keep provider drafts open after a rejected or timed-out write
- #33: Remove the nonfunctional client Debug mode setting
- #32: Preserve view recency when reconnecting
- #31: Remove unsupported chat placement from the view contract
- #30: Keep prompt save status visible beside its close control
- #29: Validate required provider fields when reading editable config
- #28: Preserve shell diagnostics when a command times out
- #27: Reject invalid monitor conditions before installation
- #26: Describe monitor timestamps as activity rather than firing
- #25: Resolve blob locations from the current data root
- #24: Keep active uploaded image documents out of inline app-origin delivery
- #23: Correlate Thread action results with the originating Thread
- #22: Reject unknown view-only stores before schema initialization writes
- #21: Recognize authentication rejection after reconnect
- #20: Evict removed views from hello deduplication
- #19: Bind Thread mutations to captured history end to end
- #18: Make stderr-drain regression deterministic and clean up its child on failure
- #16: Admit new Thread work after interrupted CLI turns on restart
- #15: Combine schema and codebase audits and implement verified improvements
- #14: Show one persistent artifact beside each Thread conversation
- #13: Replace Inspect execution with clear turn progress and visible failure outcomes
- #12: Restrict pins to top-level Threads and retain one nested list
- #11: Block artifact iframe self-navigation from making network requests
- #10: Support opening, previewing and downloading artifacts in Android
- #9: Preserve whitespace in artifact exact-match edits
- #8: Add configurable Thread icons for Owners and agents
- #6: Acknowledge subagent terminal delivery only after durable handling

## Current accepted constraints and excluded ground

- PRODUCT.md and ADR0016 are current: a Thread owns its messages, turns and activity. Projects are ordinary root Threads, with no separate Project/Namespace entity. Parentage is immutable and agent access follows descendants. Identity exists before execution. Settlement/reopen is explicit; reading, replies, successful turns, inactivity and PR updates do not auto-settle. Attention, reading, visibility, execution and settlement are independent. The user now questions ongoing versus finishable work vocabulary and UX: alternatives may revise this doctrine, but must identify that change. The root's Conversation/Task hypothesis is unadopted and already means one Thread model with modes at any depth. Optional tracking may be structurally equivalent. No taxonomy/lifecycle implementation is authorized.
- ADR0017 and current PRODUCT: artifacts are globally shared mutable referenced results, with at most one showcase per Thread. No artifact ownership/revision redesign. Root pins, nesting, icons, showcase and source/rendered work is implemented or tracked in PR #17 and #8/#12/#14/#48; do not re-recommend it.
- ADR0001/0002/0003/0014 retain the Host/SQLite, Lash RLM, native Codex/Claude and in-tree plugin architecture. Do not introduce a workflow/retry engine (the surviving ADR0004 constraint). Native Rust shared core remains ADR0010; native UI history #46 and artifacts #10 are excluded.
- Prior rounds in `docs/research/prospect-subagent-drivers-2026-09-09` and `docs/research/representation-simplification-audit-2026-09-10` cover runtime delivery, schemas, reconnection, tool availability, skill invocation, provider forms and related work. Do not duplicate them.
- Chronology matters: `docs/research/prospect-task-model-2026-09-09/report.md` recommended global conversation/Task margins and rejected per-Thread chat. ADR0016 subsequently accepted per-Thread conversations and superseded those recommendations. Never cite that older report as current doctrine. Its verified separation of identity, attention, execution and completion remains useful established ground, not a novel finding. Read prior considered/not-adopted lists with this supersession in mind.
- #49 covers coordinated responsive sidebar/chat/artifact layout, wide-open defaults with explicit preference, and status wrapping. It is owned by a separate delivery lane. Do not rediscover layout mechanics. Findings may extend #49 only with semantic labels, actions or grouping following an adopted domain choice.
- #13's inline chronological work/tool/response timeline, immediate queue feedback and tool-result readability are already implemented; do not re-audit them. Persistent last-turn completion as inventory status and generic Settle on ongoing project conversations are this round's new domain UX question, adjacent to #13/#49.
- #50 tracks this report, not approval of a design or authorization to implement one. No new tickets for unapproved choices.

Every reader, doctrine reviewer and verifier receives the exclusion map: skip covered work; name adjacent issues and report only the actual delta. Record considered but unadopted practices without inventing settled rulings. The initial five readers received the initial map; #50 was added for the doctrine reviews. The clarification that modes already mean one model was supplied explicitly to the separate verifiers.
