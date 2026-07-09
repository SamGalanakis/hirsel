# Hirsel Context

Hirsel is a single-player, phone-first personal agent system: one human, one agent, one VM. It is not a product; permission gating and multi-tenancy are explicitly out of scope.

## Language

**Hirsel Host**:
The single long-running process on the VM that runs everything server-side: embeds the lash runtime, terminates the owner's transport connection, owns the Event Log, and supervises Sub-agents.
_Avoid_: server, backend, daemon

**Agent**:
The one long-lived conversational intelligence the Owner talks to. It is the interface — no router or meta-agent above it; everything else in the system is a tool it calls.
_Avoid_: assistant, main agent, orchestrator

**Sub-agent**:
An external coding agent (Claude Code, Codex) driven by the Agent through a Sub-agent Driver. Sub-agents do work; they never talk to the Owner directly. Sub-agent sessions are cattle: what persists is the Agent's own transcript of what it delegated, and a dead session is re-spawned only if the Agent decides it still wants the work.
_Avoid_: worker, child agent, ACP session

**Sub-agent Driver**:
The per-agent-CLI adapter inside the Hirsel Host that spawns, prompts, interrupts, and streams events from one kind of Sub-agent over its native headless protocol. Drivers normalize agent-specific output into hirsel's own sub-agent event model.
_Avoid_: ACP client, connector

**Owner**:
The single human the system serves.
_Avoid_: user (ambiguous with sub-agent tooling), client

**Chat**:
The single live conversation thread between the Owner and the Agent. There is exactly one; parallel workstreams do not get parallel threads.
_Avoid_: conversation list, session (that's a lash/Sub-agent word)

**Inbox**:
Retired term — see Ping and Tray.
_Avoid_: using it at all

**Ping**:
The one thing the Agent sends the Owner outside the live Chat exchange: a short name (its `@name` handle), a one-line description, markdown content, an Anchor, a requires-response flag, and optionally Quick Replies. Covers blocking asks and soft FYIs alike. Lifecycle: open (unread → read as the seen-state) → done. The Owner replying to the Anchor resolves a Ping automatically; the Agent resolves moot Pings via `pings.resolve`; the Owner can mark done explicitly. One terminal state — no separate deleted/archived. Anything richer than markdown-plus-buttons is a UI template's job.
_Avoid_: inbox item, task (that's nothing in hirsel — see ADR-0004), notification, card, archived, deleted

**Anchor**:
The Chat message a Ping points back to — the place where the pings tool was called. Responding to a Ping means sending a normal Chat message that refs its Anchor, WhatsApp-quote style, so multiple pending questions coexist unambiguously.
_Avoid_: ref (as a noun), thread

**Tray**:
The collapsible surface under the Chat where Pings live. Badge and Done section live on/in it.
_Avoid_: inbox, inbox tab, drawer

**Side Chat**:
An ephemeral conversation forked from the main Chat and scoped to one Ping: a fresh session seeded with the item, its Anchor exchange, and a bounded window of recent chat — same Agent persona, transcript never enters the main session. Concluding it discards it.
_Avoid_: thread (standing), fork (implies full copy), sub-conversation

**Conclusion**:
The Owner's answer to a Ping produced by a Side Chat: drafted by the side agent, edited/confirmed by the Owner, delivered into the main Chat as the Owner's Anchor-refed reply (and the Ping resolves).
_Avoid_: summary, verdict, resolution

**Quick Reply**:
An optional flat list of value/label buttons on a Ping. Tapping one sends its value as an ordinary Anchor-refed Chat message; freeform reply is always available alongside. Not a form engine — no typed fields, no validation.
_Avoid_: response spec, field spec, action button
