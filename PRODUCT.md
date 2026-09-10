# Hirsel

Hirsel coordinates work through conversational **Spaces** and **Tasks**. A Space is an ongoing place for context and organization; a Task is a finishable outcome. Both share one durable Thread identity, conversation, generated instrument, execution turns and factual activity. Humans can browse the whole tree.

## Work and conversation

Each Space or Task has a stable numeric identity, title, description and optional instrument. It appears in the inventory before any message, decision or execution is required. “Household” can be a Space containing a “Buy groceries” Task; the shopping list, store decision and progress stay in that Task’s conversation. Thread names their common identity, not a third kind.

The composer sends to the explicitly selected Thread. Messages belong to exactly one Thread. There is no reserved coordinator ID, default recipient, Project or Namespace entity. Every actual ID, including a retained ID 0, is an ordinary Thread. New histories start empty. A valid route or saved selection from the same history restores context; otherwise the Owner chooses or creates a Thread before composing.

Both kinds can exist at the top level. A Space may contain Spaces or Tasks; a Task may contain Tasks only. Each Thread has an immutable nullable parent. Only top-level Threads can be pinned; pinning keeps that root at the top of the list, appearing once with its children beneath it. Pinning does not change parentage, activity, read state or lifecycle. Nested conversations keep focused work under the conversation that requested it; a parent can continue while children run.

Each Thread displays a small avatar. The Owner can choose an emoji or symbol through **Change icon** in the Thread menu, or restore the generated default. Agents can set `icon` when creating or updating an accessible Thread; omitting it preserves the current choice and `null` restores the default. Icon edits use revision checks and do not start execution or mark the conversation read.

## Independent state

- **Pinning:** top-level Threads stay at the top of the list, ordered by pin time and independent of lifecycle.
- **Task completion:** open or explicitly done by the Owner through **Mark done** / **Reopen**. Spaces have no completion state. Completing a child never completes its parent.
- **Attention:** quiet or needs Owner input. It can change repeatedly during a Thread's lifetime.
- **Visibility:** archived, snoozed or visible. Hiding a Thread never silently settles it.
- **Read state:** seen or unseen; reading never completes work.
- **Execution:** queued, running, completed, failed, cancelled or interrupted turns. Completing a turn never completes a Task. Idle Spaces do not retain a successful-turn completion badge; current execution, queues and needs-input remain visible.

Thread inventory membership does not depend on whether content is information, a question or a digest. There is no separate Event/Ping work inventory.

## Instruments

The Agent composes constrained JSON UI on an existing Thread. Updates preserve identity and conversation. A generated `continue` action runs the next stage in that Thread; a control explicitly marked to complete is valid only on a Task and requires an Owner action. Reading, ordinary replies and progress are lifecycle-neutral. Only currently displayed actions are accepted, with revision checks preventing stale instruments from silently acting on newer state.

## Agent and processes

Each Thread executes in its own lane with its own history and accepted assignment. Delegation creates or addresses a direct child, carrying a focused brief and explicit artifact references rather than the parent transcript. The child records progress and a durable terminal handoff to the requesting parent. Human input directly in a child also reports to its structural parent. Parent reports carry the actual child and turn IDs; they never become the parent's execution transcript.

Agent tools resolve caller-relative references from trusted execution context. An agent can read and organize itself and its subtree, dispatch only to direct children, and report upward through its recorded requester. Ancestor identities provide navigation context, not access to ancestor, peer or unrelated project conversations. Numeric IDs cannot bypass these checks. Humans retain the full tree. These are application resource boundaries, not a global filesystem or multiuser security model.

Lash, Claude and Codex remain supported execution choices. CLI processes and monitors are implementation resources with concrete Thread origins, not a second independent work inventory. Background work requires an explicit destination. Per-Thread FIFO and bounded cross-Thread concurrency preserve request identity. Archive and snooze pause automatic report-triggered execution while retaining reports for later delivery; settlement is independent.

## Clients

Web and native clients share the same Thread contract. The inventory, focused conversation, instrument and streaming activity use durable IDs. Reconnection obtains persisted Thread state, messages, turns and activity. Live frames carry owning Thread and turn identities so delayed activity cannot appear inside an unrelated conversation.

[ADR 0016](docs/adr/0016-threads-own-conversation.md) records conversation ownership; [ADR 0018](docs/adr/0018-spaces-and-tasks.md) refines it with Spaces and Tasks. Kind is persisted and enforced by the Host for all clients and tools. The Owner can change kind without changing identity or conversation when the existing parent and immediate children permit it. A done Task must be reopened before becoming a Space. Conversion never moves or changes children, and agents cannot convert kinds or mark work done.

## Artifacts

An Artifact is an explicitly published result with a stable global ID and mutable content. It has no owning Thread. Agent tools create, edit, list and show artifacts within their Thread scope; explicit message, activity or validated brief references grant access. Create, edit and show place a compact reference card in the addressed conversation. Authorized references permit edits with current-content compare-and-swap; shared outputs are deliberately mutable across referencing Threads, without exposing those other conversations. Multiple Threads may reference the same artifact. Cards always resolve the latest content, including cards in earlier messages. There is no revision history.

Initial formats are self-contained Solid 2 JSX, HTML, and UTF-8 files. Previews support local interaction only, in an isolated browser surface without network access or a Hirsel backend/tool bridge. Files can be read and downloaded. Publication is deliberate; ordinary messages, attachments, execution output and generated instruments do not automatically become artifacts.

Conversation is the resting surface. Its Artifacts view lists the results referenced in that Thread; the global Artifacts inventory links each result back to its conversations. Opening or expanding a result never changes the Thread addressed by the composer. Each turn's work stays beside its exact request or response: a concise status precedes one inline chronological sequence of reasoning, tool activity and provisional prose. Individual tool rows expand to their bounded input and result; raw turn and event data is available through the Technical details overflow. Completed, failed and stopped timelines replay from durable storage after reconnect, while plain replies have no empty work control. Thread instruments retain their existing action validation and revision checks; they are distinct from artifacts.

A human can discuss a globally listed artifact in any selected Thread. Previewing it stages a visible, removable About context in that Thread’s draft; a different preview replaces the context. Closing the preview preserves it, while switching Threads does not transfer it. Only submitting the message creates the explicit artifact reference that lets the addressed agent read/edit that shared result. Plain numeric text and preview browsing create no grant. Context is cleared with the submitted draft, retained in exact pending/retry messages, and discarded on history reset.

## Related references

Related is the per-Thread companion to Conversation. It groups explicitly saved web links and Thread references with the existing artifact references; the global Artifacts inventory stays available. Recognizing a URL in prose creates no saved record or artifact. Native links preserve authored labels and browser navigation, with local resource icons and adjacent Open, Copy and Add to Related actions. URLs can be added directly, and a title or number search adds Thread references. Removing an item removes only its association with this Thread. Thread references use current titles and do not create hierarchy edges or access grants. The original conversation and artifact references remain unchanged.

Saving or removing a link addresses the captured Thread and history, never whichever Thread happens to be selected later. Related does not send a message, start work, alter a draft or grant resource access. Its list survives reconnect through authoritative snapshots. Artifact previews keep their existing isolation, recipient and draft behavior; they expose no Related action bridge.

Portable Thread references are ordinary Markdown links to `/t/{id}?history={uuid}` on this app’s origin. A matching authoritative hello is required before selecting the recipient or resolving its title. An incomplete, malformed, old-history or missing destination stays unaddressed with a return to the Thread overview. Local `#id` shorthand remains available within the current history.

## Showcased artifact

A Thread can showcase at most one artifact. The web workspace displays it beside the conversation with independent scrolling; phones expose a dedicated showcase view. The Owner can promote an artifact from its reference menu, replace it, or remove the showcase. These actions preserve the chat draft and do not start a model turn. Changing the showcase never deletes the artifact or its existing references.

The Thread's own agent and agents in its ancestor Threads can set or remove the showcase. An agent must already have access to the chosen artifact; showcasing it grants access through that Thread while the pointer remains. Other references retain their own access independently. The showcase follows the artifact's latest content and survives reconnect. Browsing another artifact or staging About context does not replace it.
