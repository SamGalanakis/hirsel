# Product

## Register

product

## Platform

web

> Note: the web app is hirsel's desktop client and the protocol's reference implementation. The mobile end-state is a native Android app (Jetpack Compose over a shared Rust `hirsel-client-core`, ADR-0010), built outside impeccable. impeccable governs this web surface; `web` here selects web design rules rather than a native (Material 3 / HIG) rulebook.

## Users

Exactly one: Sam — the author of lash, a Rust systems person who lives in terminals and drives coding agents all day. hirsel is his single-player, phone-first personal agent: one human, one long-lived agent, one VM. His context switches between glancing at a phone (is anything blocked on me? what's my agent doing?) and working at depth on a desktop (reading a turn timeline, driving a Side Chat, reviewing delegated work). Assume total fluency — CLI keyboard conventions are features not barriers, density is welcome, hand-holding is insulting. Not a product for sale; no multi-tenancy, no permission gating, no onboarding funnel.

## Product Purpose

hirsel is the interface to a personal agent that works asynchronously on Sam's behalf. The agent talks to him directly (Chat), sends him async work as **Pings** (named items with a one-line description, open→done, reply auto-resolves) surfaced in a **Tray**, delegates real work to **Sub-agents** shown live as a **turn timeline** (prose → tool call → result), runs background **Processes** (monitors, timers) that can wake it, and can be pulled into a focused **Side Chat** (Slack-style thread) whose **Conclusion** becomes Sam's reply. Success is that hirsel is legible and glanceable — Sam can tell in one look whether anything needs him, watch the agent work when he wants to, and stay out of it when he doesn't. It interrupts him only when genuinely blocked on him.

## Brand Personality

**Calm terminal.** Dark, quiet, precise — a professional instrument in the lineage of a good TUI (lash-tui, Linear, Superhuman), not a chat toy. Three words: understated, legible, exact. Monospace earns its place for ids, commands, tool names, and keyboard hints; motion is restrained; the surface is information-dense but never noisy. Confidence comes from typography and spacing, not decoration. The agent is capable and understated; the UI never performs. Emotional goal: the quiet competence of a tool that respects your attention — calm, never cheer.

## Anti-references

- **Corporate SaaS chat** (Intercom / Zendesk): chirpy bubbles, marketing tone, rounded friendliness, "How can I help you today!" energy.
- **Consumer assistant** (Siri / Alexa / ChatGPT mobile app): mascot personality, over-explaining, hand-holding, empty enthusiasm.
- **Notification slot machine**: badge spam, red dots everywhere, engagement-bait urgency. hirsel signals "needs you" with one restrained accent and stays silent otherwise.

## Design Principles

- **Glanceable on a phone, deep on a desktop.** Same information; presentation earns the width (phone shelf/sheet ↔ desktop rails/split). Neither surface is a compromise of the other.
- **The agent is the interface.** No chrome implying a dispatcher, settings sprawl, or a product to sell — the conversation and its artifacts (Pings, timeline, Processes) are the product.
- **Restraint as respect.** One accent for "needs you," muted for everything else. Silence is a feature; the UI interrupts only when the agent is genuinely blocked on the Owner.
- **Keyboard-grade and thumb-grade.** CLI composer semantics on desktop, first-class touch on phone — both first-class, neither an afterthought.
- **Show the work.** Delegation, tool calls, and process state are visible and legible (turn timeline, Processes tab) — the instrument is inspectable, not a black box.

## Accessibility & Inclusion

Single known user, no stated disability requirements, but hold to WCAG AA regardless: the dark palette must keep AA contrast for body and secondary text (no gray-on-gray murk), the single "needs you" state must be distinguishable without relying on color alone (weight/label, not just a hue), all keyboard flows must have visible focus, and everything honors `prefers-reduced-motion`. Density must not come at the cost of tap-target size on the phone surface.
