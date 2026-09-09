# Hirsel

Hirsel is a personal Agent that coordinates work across durable **Threads**. The Owner talks to one globally aware Agent. Each Thread owns its conversation, generated instrument, execution turns and factual activity.

## Work and conversation

A Thread is an ongoing subject or unit of work. It has a stable numeric identity, title, description and optional instrument. It exists and appears in the inventory before any message, decision or execution is required. “Buy groceries” creates a Thread; the shopping list, store decision and progress all stay there.

The composer sends to the focused Thread. Messages belong to exactly one Thread. References and cross-thread mentions provide context; they never transfer message ownership. The Agent can explicitly inspect other Thread histories and keeps global orchestration context. The coordinator Thread, ID 0, holds coordination and unaddressed background activity.

## Independent state

- **Settlement:** open or explicitly settled by the Owner. Reopening preserves history and identity.
- **Attention:** quiet or needs Owner input. It can change repeatedly during a Thread's lifetime.
- **Visibility:** archived, snoozed or visible. Hiding a Thread never silently settles it.
- **Read state:** seen or unseen; reading never completes work.
- **Execution:** queued, running, completed, failed, cancelled or interrupted turns. Completing a turn never settles the Thread.

Thread inventory membership does not depend on whether content is information, a question or a digest. There is no separate Event/Ping work inventory.

## Instruments

The Agent composes constrained JSON UI on an existing Thread. Updates preserve identity and conversation. A generated `continue` action runs the next stage in that Thread; a control explicitly marked to complete settles it. Reading, ordinary replies and progress are lifecycle-neutral. Only currently displayed actions are accepted, with revision checks preventing stale instruments from silently acting on newer state.

## Agent and processes

One Agent orchestrates all Threads. Sub-agent and monitor processes are execution resources associated with work, not competing durable work objects. The Agent delegates slow work and remains available for new requests. Cross-thread requests queue at a strict turn boundary; one execution never silently merges Owner conversations from different Threads.

Background results become activity in their addressed Thread, or coordinator activity when ownership is unknown. A triage fork may record or escalate evidence; it cannot settle work. Recovery preserves queued Owner requests and marks interrupted execution for judgment rather than automatically restarting it.

## Clients

Web and native clients share the same Thread contract. The inventory, focused conversation, instrument and streaming activity use durable IDs. Reconnection obtains persisted Thread state, messages, turns and activity. Live frames carry owning Thread and turn identities so delayed activity cannot appear inside an unrelated conversation.

The source decision is [ADR 0016](docs/adr/0016-threads-own-conversation.md). It supersedes earlier Task/Event/Ping vocabulary and the global-conversation ownership model.

## Artifacts

An Artifact is an explicitly published result with a stable global ID and mutable content. It has no owning Thread. Agent tools create, edit, list and show artifacts; create, edit and show place a compact reference card in the addressed conversation. Multiple Threads may reference the same artifact. Cards always resolve the latest content, including cards in earlier messages. There is no revision history.

Initial formats are self-contained Solid 2 JSX, HTML, and UTF-8 files. Previews support local interaction only, in an isolated browser surface without network access or a Hirsel backend/tool bridge. Files can be read and downloaded. Publication is deliberate; ordinary messages, attachments, execution output and generated instruments do not automatically become artifacts.

Conversation is the resting surface. Its Artifacts view lists the results referenced in that Thread; the global Artifacts inventory links each result back to its conversations. Opening or expanding a result never changes the Thread addressed by the composer. Execution details remain under Inspect execution. Thread instruments retain their existing action validation and revision checks; they are distinct from artifacts.
