# Product

## Register

product

## Platform

web

> Note: the web app is hirsel's desktop client and the protocol's reference implementation. The mobile end-state is a native Android app (Jetpack Compose over a shared Rust `hirsel-client-core`, ADR-0010), built outside impeccable. impeccable governs this web surface; `web` here selects web design rules rather than a native (Material 3 / HIG) rulebook.

## Users

Exactly one: Sam — the author of lash, a Rust systems person who lives in terminals and drives coding agents all day. hirsel is his single-player, phone-first personal agent: one human, one long-lived agent, one VM. His context switches between glancing at a phone (is any task blocked on me? what is moving?) and working at depth on a desktop (shaping task-generated UI, reading a turn timeline, reviewing delegated work). Assume total fluency — CLI keyboard conventions are features not barriers, density is welcome, hand-holding is insulting. Not a product for sale; no multi-tenancy, no permission gating, no onboarding funnel.

## Product Purpose

hirsel is the interface to one personal agent that works asynchronously on Sam's behalf. The product has one durable user-facing object: the **task**. A task is an addressable unit of work with current state, a constrained JSON-generated interface, and any conversation that changed it. The wire's typed Events are task updates; their stable id and anchor bind generated UI and owner messages to the same task.

Hirsel is always the interlocutor and is globally aware of every task conversation. Opening a task focuses the subject and generated interface, never the agent or the conversation universe. With no task focused, Hirsel is ambient across everything; that state is expressed by the absence of focus rather than a named mode. The standing composer inherits the field's focus and contains no scope control or instructional placeholder. Background processes, tools, model settings, and raw timelines remain inspectable utilities, not destinations or additional product objects. Success is that Sam can see what is moving, dive into one task, and return to the ambient whole without reconstructing context or entering a nested thread.

## Brand Personality

**Calm terminal.** Dark, quiet, precise — a professional instrument in the lineage of a good TUI (lash-tui, Linear, Superhuman), not a chat toy. Three words: understated, legible, exact. Monospace earns its place for ids, commands, tool names, and keyboard hints; motion is restrained; the surface is information-dense but never noisy. Confidence comes from typography and spacing, not decoration. The agent is capable and understated; the UI never performs. Emotional goal: the quiet competence of a tool that respects your attention — calm, never cheer.

## Anti-references

- **Corporate SaaS chat** (Intercom / Zendesk): chirpy bubbles, marketing tone, rounded friendliness, "How can I help you today!" energy.
- **Consumer assistant** (Siri / Alexa / ChatGPT mobile app): mascot personality, over-explaining, hand-holding, empty enthusiasm.
- **Notification slot machine**: badge spam, red dots everywhere, engagement-bait urgency. hirsel signals "needs you" with one restrained accent and stays silent otherwise.

## Design Principles

- **Glanceable on a phone, deep on a desktop.** Same information; presentation earns the width (phone shelf/sheet ↔ desktop rails/split). Neither surface is a compromise of the other.
- **Tasks are the only durable objects.** No Feed, Chat, Side Chat, evidence space, agent thread, or Canvas becomes a parallel destination. Generated UI and conversation live inside the task they affect.
- **The agent is the interface.** Hirsel remains one globally aware interlocutor. Utilities are summoned and dismissed; they never become the information architecture.
- **Restraint as respect.** One accent for "needs you," muted for everything else. Silence is a feature; the UI interrupts only when the agent is genuinely blocked on the Owner.
- **Keyboard-grade and thumb-grade.** CLI composer semantics on desktop, first-class touch on phone — both first-class, neither an afterthought.
- **Show the work on demand.** Delegation, tool calls, and process state remain inspectable, but the resting surface shows tasks and the exact judgment or action each task needs.

## Accessibility & Inclusion

Single known user, no stated disability requirements, but hold to WCAG AA regardless: the dark palette must keep AA contrast for body and secondary text (no gray-on-gray murk), the single "needs you" state must be distinguishable without relying on color alone (weight/label, not just a hue), all keyboard flows must have visible focus, and everything honors `prefers-reduced-motion`. Density must not come at the cost of tap-target size on the phone surface.
