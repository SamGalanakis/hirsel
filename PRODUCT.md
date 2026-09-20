# Hirsel

> Space-chat amendment, 2026-09-20: [ADR 0025](docs/adr/0025-project-chats-material-state-and-coordination-delivery.md) supersedes the earlier “no default recipient” landing rule. Route-free entry now restores the last Space for this history or opens Home; explicit bad links still never redirect.

Hirsel coordinates work through conversational **Spaces** and **Tasks**. A Space is an ongoing place for context and organization; a Task is a finishable outcome. Both share one durable Thread identity, conversation, generated instrument, execution turns and factual activity. Humans can browse the whole tree.

## Platform

adaptive

Hirsel ships a web client (`app/`, SolidJS) and a native Android client (`android/`, Jetpack Compose over the shared Rust core). Both speak the same Thread contract.

## Work and conversation

Each Space or Task has a stable numeric identity, title, description and optional instrument. It appears in the inventory before any message, decision or execution is required. “Household” can be a Space containing a “Buy groceries” Task; the shopping list, store decision and progress stay in that Task’s conversation. Thread names their common identity, not a third kind.

The composer sends to an explicit recipient and Messages belong to exactly one Thread. Every Space has a Space chat in its ordinary Thread conversation. Route-free entry restores the last active top-level Space for this history or atomically creates and opens the ordinary top-level Space named Home. Home has an ordinary positive ID and no automatic root grant. Every actual ID, including a retained ID 0, remains an ordinary Thread. Explicit invalid, malformed, missing and old-history links remain unaddressed instead of falling back to Home.

Space recipient, optional Task focus and worker pairing are independent, always-labelled composer facts. “Talk about this” opens a Task's owning Space chat and attaches a bounded title/brief/current-instrument snapshot to the next message. The snapshot is context, not a transcript or reach. “Step in” addresses the Task's own worker and visibly offers “Send after current turn” while work is running; Stop is separate. Drafts are keyed by history and the actual recipient.

Both kinds can exist at the top level. A Space may contain Spaces or Tasks; a Task may contain Tasks only. Each Thread has an immutable nullable parent. Only top-level Threads can be pinned; pinning keeps that root at the top of the list, appearing once with its children beneath it. Pinning does not change parentage, activity, read state or lifecycle. Nested conversations keep focused work under the conversation that requested it; a parent can continue while children run.

Each Thread displays a small avatar. Without a chosen icon it is a quiet monogram of the title. Through **Change icon** the Owner can pick a symbol from a curated vocabulary and a tint for its tile, upload a center-cropped PNG, JPEG or WebP image, or go back to the monogram. Agents can set the same typed symbol/image identity on their own Thread or its subtree, choosing a symbol name and tint or an existing blob or accessible image artifact; a name outside the vocabulary is refused. Icon edits use revision checks and do not start execution or mark the conversation read.

## Independent state

- **Material state:** each Task has a short headline, bounded findings, explicit artifact references, checkpoint time and an independent positive revision. A worker updates it with `threads.state`; conflicts return the current state instead of overwriting it. Lifecycle, instrument, showcase and referenced-artifact changes advance the material revision, while reads and telemetry do not.
- **Rollup headline:** a Task or Space with children keeps its own headline separately and displays a Host-derived child-count rollup. The Host uses fixed status precedence and numeric Thread IDs, never arbitrary child prose. Rollups propagate through ancestors and never complete a parent.
- **Outside changes:** when work in one Space materially changes a reachable Task or explicitly shared artifact associated with another Space, the affected Space chat shows one coalesced “Changed by <Space>” activity. Before its next turn the Host freezes a bounded digest with the rest of the accepted context; it does not wake automatically or grant reach. Failed, cancelled and interrupted turns leave the digest unconsumed for a later turn.
- **Pinning:** top-level Threads stay at the top of the list, ordered by pin time and independent of lifecycle.
- **Task completion:** open or explicitly done by the Owner through **Mark done** / **Reopen**. Spaces have no completion state. Completing a child never completes its parent.
- **Attention:** quiet or needs Owner input. It can change repeatedly during a Thread's lifetime.
- **Visibility:** archived, snoozed or visible. Hiding a Thread never silently settles it.
- **Read state:** seen or unseen; reading never completes work.
- **Execution:** queued, running, completed, failed, cancelled or interrupted turns. Completing a turn never completes a Task. Idle Spaces do not retain a successful-turn completion badge; current execution, queues and needs-input remain visible.

Thread inventory membership does not depend on whether content is information, a question or a digest. There is no separate Event/Ping work inventory.

## Instruments

The Agent composes constrained JSON UI on an existing Thread. Updates preserve identity and conversation. A generated `continue` action runs the next stage in that Thread; a control explicitly marked to complete is valid only on a Task and requires an Owner action. Reading, ordinary replies and progress are lifecycle-neutral. Only currently displayed actions are accepted, with revision checks preventing stale instruments from silently acting on newer state.

## Malleability

Malleability in Hirsel is agent-authored: the Owner shapes the tool by asking for it, not by waiting for a release. The Agent writes instruments, processes, prompt text and grants as durable state, and every one of them is inspectable, editable and deletable in place. Hirsel ships no baked-in structure — no default workflows, digests, timers or recurring processes — and treats each of these artifacts as data rather than code, so changing how the tool behaves never requires a new build.

## Agent and processes

Each Space chat talks with the Owner, resolves the intended Task and dispatches through atomic delegation without doing the work itself. Each Task has a worker with its own history and accepted assignment. These roles are guidance and presentation, never Host enforcement. Delegation creates or addresses a direct child, carrying a focused brief and explicit artifact references rather than the parent transcript. The child records progress and a durable terminal handoff to the requesting parent. Human input directly in a child also reports to its structural parent. Parent reports carry the actual child and turn IDs; they never become the parent's execution transcript.

Agent tools are fenced, not sandboxed. Every Thread and artifact ID is addressable: naming one outside a Thread's reach returns a typed refusal the Agent can read and act on, recorded as a durable refusal in the conversation, never an opaque error pretending the target is not there. Reach is durable, visible state — self and descendants by default, widened by explicit grants that name one other Thread and its subtree. Only the Owner or an ancestor Thread can widen, an ancestor can only hand on reach it already holds and never widens itself, and either can narrow. A Thread messages anything it can reach, but never its own ancestors: work reports to the requester that asked for it. Humans retain the full tree. These are application resource boundaries, not a global filesystem or multiuser security model. [ADR 0022](docs/adr/0022-fences-universal-addressing-and-grants.md) records the model.

Every Thread or artifact an Agent reply touched appears beside that reply as a
compact effect pill, backed by a receipt committed with the mutation, read or
refusal. Pills arrive while the turn is running and remain through failed
turns, replies with no final Agent message, pagination and reload. A receipt is
the durable fact; Open, Archive, Cancel queued and Stop are live Host
projections shown only while true and always name the exact target work. A
refused Thread pill explains the subtree scope before opening Reach and never
retries automatically. Ancestor-fence and artifact refusals explain why no
ordinary guessed grant is offered.

A Thread runs on exactly one of three backends: Native, the Claude CLI, or the Codex CLI. Native and CLI turns translate into one durable executor event contract, so reconnect replay and client rendering do not depend on the backend. Native is Hirsel's own TypeScript RLM session on a chosen provider and model, with process and trigger abilities. It may define arbitrary Lash processes, attach them to available triggers, and call its ordinary tools from process bodies. Hirsel contributes trigger sources and projects the Lash registry; it does not wrap process execution in another engine. Every process has a concrete owning Thread. Wakes and terminal results become bounded conversation messages and follow the same fork-triage rule as other non-owner input. Registered trigger and process state reopens from Lash's durable stores after restart. Recurring processes are created only at an Owner's request.

A Native Thread's TypeScript programs carry the full Thread tool set and the four coding operations—`files.read`, `files.edit`, `files.write`, and `shell.exec`—alongside RLM's process, trigger, finish, and frame-compaction primitives. Every Thread has the same tool surface, and any Thread may select Native, Claude CLI or Codex CLI. A Thread is never handed to a second session to touch a file. Hirsel creates no default processes. Background work requires an explicit destination. Per-Thread FIFO and bounded cross-Thread concurrency preserve request identity.

## Clients

Web and native clients share the same Thread contract. The inventory, focused conversation, instrument and streaming activity use durable IDs. Reconnection obtains persisted Thread state, messages, turns and activity. Live frames carry owning Thread and turn identities so delayed activity cannot appear inside an unrelated conversation.

[ADR 0016](docs/adr/0016-threads-own-conversation.md) records conversation ownership; [ADR 0018](docs/adr/0018-spaces-and-tasks.md) refines it with Spaces and Tasks. Kind is persisted and enforced by the Host for all clients and tools. The Owner can change kind without changing identity or conversation when the existing parent and immediate children permit it. A done Task must be reopened before becoming a Space. Conversion never moves or changes children, and agents cannot convert kinds or mark work done.

## Artifacts

An Artifact is an explicitly published result with a stable global ID and mutable content. It has no owning Thread. Agent tools create, edit, list and show artifacts within their Thread scope; explicit message, activity or validated brief references grant access. Create, edit and show place a compact reference card in the addressed conversation. Authorized references permit edits with current-content compare-and-swap; shared outputs are deliberately mutable across referencing Threads, without exposing those other conversations. Multiple Threads may reference the same artifact. Cards always resolve the latest content, including cards in earlier messages. There is no revision history.

Formats are self-contained Solid 2 JSX, HTML, Markdown, images, UTF-8 files, and `openui` — generated interfaces written as OpenUI Lang against Hirsel's own component vocabulary. An `openui` artifact is data, not a program: Hirsel parses it and draws it natively with the app's own tokens, so it is on-brand by construction, it appears while it is still being written, and a line the parser cannot use is dropped instead of breaking the page. Its buttons, follow-ups and forms send an ordinary Owner message back to the artifact's Thread carrying what was chosen, so an interactive result advances the conversation that produced it rather than opening a private channel. Prefer it for anything the Owner reads or operates; the Solid kind remains for a custom visualisation the vocabulary cannot express. Previews of the executable kinds support local interaction only, in an isolated browser surface without network access or a Hirsel backend/tool bridge. Files can be read and downloaded. Publication is deliberate; ordinary messages, attachments, execution output and generated instruments do not automatically become artifacts.

Conversation is the resting surface. Its Artifacts view lists the results referenced in that Thread; the global Artifacts inventory links each result back to its conversations. Opening or expanding a result never changes the Thread addressed by the composer. Each turn's work stays beside its exact request or response: a concise status precedes one inline chronological sequence of reasoning, tool activity and provisional prose. Individual tool rows expand to their bounded input and result; raw turn and event data is available through the Technical details overflow. Completed, failed and stopped timelines replay from durable storage after reconnect, while plain replies have no empty work control. Thread instruments retain their existing action validation and revision checks; they are distinct from artifacts.

A human can discuss a globally listed artifact in any selected Thread. Previewing it stages a visible, removable About context in that Thread’s draft; a different preview replaces the context. Closing the preview preserves it, while switching Threads does not transfer it. Only submitting the message creates the explicit artifact reference that lets the addressed agent read/edit that shared result. Plain numeric text and preview browsing create no grant. Context is cleared with the submitted draft, retained in exact pending/retry messages, and discarded on history reset.

## Related references

Related is the per-Thread companion to Conversation. It groups explicitly saved web links and Thread references with the existing artifact references; the global Artifacts inventory stays available. Recognizing a URL in prose creates no saved record or artifact. Native links preserve authored labels and browser navigation, with local resource icons and adjacent Open, Copy and Add to Related actions. URLs can be added directly, and a title or number search adds Thread references. Removing an item removes only its association with this Thread. Thread references use current titles and do not create hierarchy edges or access grants. The original conversation and artifact references remain unchanged.

Saving or removing a link addresses the captured Thread and history, never whichever Thread happens to be selected later. Related does not send a message, start work, alter a draft or grant resource access. Its list survives reconnect through authoritative snapshots. Artifact previews keep their existing isolation, recipient and draft behavior; they expose no Related action bridge.

Portable Thread references are ordinary Markdown links to `/t/{id}?history={uuid}` on this app’s origin. A matching authoritative hello is required before selecting the recipient or resolving its title. An incomplete, malformed, old-history or missing destination stays unaddressed and never falls back to a remembered Space or Home. Local `#id` shorthand remains available within the current history.

## Showcased artifact

A Thread can showcase at most one artifact. The web workspace displays it beside the conversation with independent scrolling; phones expose a dedicated showcase view. The Owner can promote an artifact from its reference menu, replace it, or remove the showcase. These actions preserve the chat draft and do not start a model turn. Changing the showcase never deletes the artifact or its existing references.

The Thread's own agent and agents in its ancestor Threads can set or remove the showcase. An agent must already have access to the chosen artifact; showcasing it grants access through that Thread while the pointer remains. Other references retain their own access independently. The showcase follows the artifact's latest content and survives reconnect. Browsing another artifact or staging About context does not replace it.
