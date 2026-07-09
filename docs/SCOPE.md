# Scope: v1 vs deferred

Living document, updated as design decisions land. v1 = the smallest hirsel that Sam daily-drives from his phone.

## v1 (agreed so far)

- Hirsel Host: single Rust binary embedding lash (sqlite store, inline effects). One repo: host + PWA + (later) templates. [ADR-0001]
- Agent: one lash session, RLM mode, compacted via Agent Frames. [ADR-0002]
- Sub-agents: Claude Code + Codex via native `SubagentDriver` implementations (headless JSONL, full-auto, permissions bypassed at spawn) — both drivers in v1. No ACP, no MCP in this path. Division of labor / racing is prompt convention. [ADR-0003]
- No task abstraction; recovery of dead sub-agent work is Agent judgment. [ADR-0004]
- Agent wires its own wakes via lash process/trigger/wake machinery; system prompt carries the conventions. [ADR-0005]
- Chat + Inbox: single chat thread; inbox items = markdown + Anchor + requires-response + optional Quick Replies; open/archived lifecycle; replies are anchor-refed chat messages. requires-response surfaces as in-app notification/badge only.
- Memory: none beyond the session itself. The Agent self-compacts via the RLM `continue_as` control op (fresh Agent Frame seeded by the Agent's own summary).
- PWA: mobile-friendly web first — SolidJS + the lashapp component system (Tailwind 4, Kobalte, lashapp ui primitives/theme); native/mobile-specific work deferred.
- Transport: transport-agnostic message-stream protocol; v1 over WSS (caddy + bearer token), iroh as milestone two. [ADR-0006] PWA static files served from caddy on the VM (same origin), so no external static hosting until iroh.
- Client protocol (small and boring): client `hello{last_seen_msg_id}` → atomic `hello_ok{latest_msg_id, messages, inbox, processes}` → streams `msg`, `msg_removed`, `turn_event`, `agent_activity`, `inbox_upsert`, `process_upsert`, `blob_ok`, and correlated `error`. Client sends `send_message{client_id, body, ref?, attachments?, mode?}`, `upload_blob`, `cancel_turn`, `cancel_queued`, `read_item`, and `archive_item`.
- Agent-driven e2e runbooks, figments-style: `e2e/RULES.md` + one `runbook.md` per scenario, executed by a testing agent against a host debug surface (reset, inject owner message, read chat/inbox, gate on async) — no PWA in the loop.

## Slices

1. **Prove the loop (ugly client).** Barebones PWA (chat box + inbox list) over WSS → Agent (RLM, minimal system prompt, spawn/inbox/shell Lashlang bindings) → delegate a real repo task → driver runs it full-auto → terminal event wakes Agent via its own wake wiring → Inbox Item with requires-response + Quick Reply → tap → Agent receives the anchor-refed reply. Every architectural bet fires once.
2. **Self-hosting.** The Agent improves its own PWA, tasked from the phone.

## Deferred (explicitly, with reason)

- **UI rendering / templates** — deferred whole; v1 chat renders markdown + Inbox Quick Replies only. Direction when it revives is now settled by research (docs/research/generative-ui-2026-07.md, 2026-07-09): the industry standardized on exactly the original hirsel design — HTML templates in sandboxed iframes with a JSON-RPC-over-postMessage bridge (MCP Apps, the official MCP extension shipped by ChatGPT/Claude/Copilot; HTML is the only standardized content type, JSON DSLs were explicitly not adopted). Plan: adopt the MCP Apps *shape* without the MCP protocol — agent-authored HTML template files in git, rendered via srcdoc + sandbox + strict CSP in the PWA, a small ui/*-style JSON-RPC bridge relayed over our own message stream (`ui/message` = tap-becomes-chat-message; stable template-instance IDs + re-pushed data payloads = update-in-place live views). Earlier Datastar-morph idea superseded; json-render/@json-render/solid held in reserve for per-turn LLM-emitted UI if templates ever aren't enough.
- **Voice / audio** — hold-to-talk, streaming to VM, server-side STT. Deferred whole.
- **Any memory system** — markdown notebook, lash observational-memory plugin, and the grounded-memory-units design are all deferred. Revisit when in-context + `continue_as` demonstrably hurts.
- **Web Push (VAPID)** — deferred; requires-response items surface only in-app until push earns its keep.
- **ACP driver** — only if an ACP-native agent becomes worth supporting. [ADR-0003]
- **Restate-backed durability** — sqlite is enough for single-player; EffectHost boundary keeps the door open. [ADR-0001]
- **Multi-device / additional node keys** — single phone + laptop browser is fine to start.
