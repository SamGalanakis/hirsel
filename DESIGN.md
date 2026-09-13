# Hirsel interface

The primary screen is one selected Space or Task conversation, or, unaddressed, an attention queue: the Threads waiting on the Owner first, each with the Agent's actual question, then what is running, then what was active most recently. “Choose a Space or Task” survives only for an account with none. The queue is the same view on phone and desktop. A narrow icon rail and a dense hierarchical inventory of Spaces and Tasks reach every other conversation. The composer exists only for an actual selected Thread and names its conversation.

A Thread row shows its title and useful independent signals: needs your input, unseen activity, execution, snooze and settlement. Informational content is never a reason to hide a Thread. An empty Thread remains real work and shows an empty conversation with its instrument, if any.

The focused surface contains the Thread's messages, current generated instrument, and relevant activity. A message or activity from another Thread cannot appear merely because it arrived while this surface was focused. Cross-thread references navigate to the identified Thread without rewriting ownership.

Read state is separate from open/settled. Opening a Thread can mark it seen; it cannot settle or clear a pending decision. Tasks offer the Owner **Mark done** / **Reopen**; Spaces have no completion controls. Archive and snooze change visibility while preserving settlement. A completed Agent turn means execution finished; the Thread may still require action.

Generated instruments use the existing constrained JSON component vocabulary. The current instrument is mutable; Thread identity and conversation history are stable. Instrument actions carry the displayed revision, and the Host rejects stale actions. `continue` advances the instrument in the same Thread. An explicitly completing action is valid only on a Task.

Streaming prose, reasoning and tool activity carry Thread and turn IDs. The UI resets a stream for a newly running turn and rejects delayed frames from earlier turns. Durable messages and activity remain available after reconnect.

An ordinary Thread coordinates its children. The parent conversation shows concise progress/results and referenced artifacts, with exact child/turn identity and navigation links. Child execution remains in that child’s own conversation. Ancestor breadcrumbs and a compact child disclosure preserve navigation context without introducing another project panel. Existing process/settings surfaces remain operational controls with concrete Thread origins.

[ADR 0018](docs/adr/0018-spaces-and-tasks.md) refines the common conversation ownership in [ADR 0016](docs/adr/0016-threads-own-conversation.md); these are authoritative where older design notes describe Tasks, Pings, or a single shared conversation history.

## Colors

Tokens in `app/src/styles.css` are the only source of color. `--primary` is green (`oklch(0.48 0.12 158)` light, `oklch(0.79 0.105 158)` dark); neutrals are slate with a teal cast around hue 220; `--background`, `--card`, `--surface`, `--muted`, `--border` and `--ring` each carry a light and a dark value. Status has its own named ramp: `--status-active`, `--status-idle`, `--status-success`, `--status-danger`, `--status-attention`. The cube mark has four facet tokens. New UI reads these variables; a literal color is drift.

## Typography

Type is Inter Variable over a system sans fallback, with a mono stack for IDs, commands and program text. `--text-meta` (0.72rem) is the one named size below `text-xs` and the floor for ids, timings and summaries. Radii derive from `--radius` (0.625rem). Layout keeps one horizontal rhythm — a 42rem reading measure, a 1.5rem gutter and the frame derived from them — and keys width off the named breakpoints `split` (900px), `rail` (1100px) and `workspace` (1280px) rather than literal pixel values.

## Components

Components live in `app/src/components/ui`: button (default, outline, secondary, ghost, destructive, link; xs to lg plus icon sizes), badge, card, input, textarea, dropdown menu, empty state, attachment and pane header, with licensed Lucide icons in `icons.tsx`. Touch targets reach 44px through `pointer-coarse` variants rather than a separate mobile component set.

## Thread workspace surface

The established visual system remains authoritative in `app/src/styles.css`. The Thread workspace carries one set of destinations in two shapes: a 56px icon rail beside the work at `split` and above, and a labelled bottom bar (Threads, New, Artifacts, Processes; Settings from the overview) below it, with no top-left rail on a phone. The inventory beside it is a dense one-line-per-Thread tree: a persistent 288px column at `rail` (1100px) and above, where the Owner's last open/closed choice persists, a 56px icon column between `split` and `rail`, and a modal drawer below `split`. The conversation keeps a 26rem reading minimum; below 1600px the utility region and the showcase are exclusive, and whichever loses the slot collapses to a tab that restores it. Selected rows have a quiet fill; Conversation and Artifacts have explicit selected states. Lifecycle operations are grouped in Thread actions.

Conversation contains messages, artifact reference cards, and explicit owner-facing summaries. An Agent turn is one run card: a header naming what started the run, where it ran, how long it took and how it ended, over the reply and whatever the run published. Reasoning, tool activity and provisional prose follow inline in durable chronological order inside the card's trace; each tool with recorded detail has its own input/result expander. Raw IDs and event data are available through the trace's Technical details. The card is the live view while the turn runs and the same card re-opened afterwards, open while running and closed once finished unless the Owner says otherwise for the session; a settling run hands focus to its own header. Completed, failed and stopped timelines replay after reconnect, and a failure, its recovery line and every artifact the run produced stay readable without opening the trace. The focused frame and context strip keep the addressed Thread clear while another result is open; its composer also names the destination for assistive technology. The conversation viewport follows its latest content when the composer grows, preserving the user's position when they scroll back.

Artifact previews occupy a desktop side pane or a full-screen phone surface below 1024px. Phone previews trap focus; desktop panes allow movement back into the conversation. Close and Escape return focus to the invoking control. Artifact content is a separate isolated document and does not inherit the host's interaction capabilities.

The surface brief is `.impeccable/briefs/thread-workspace.md`.

### Compact rail and focused frame

The approved nested Thread direction retains the established 56px icon rail, framed conversations and Lucide identity. The rail carries Thread overview, Spaces and Tasks, New Space or Task, All artifacts and Processes, with Settings at its foot. Utilities appear once and use licensed Lucide icons with accessible names, tooltips and 44px targets. The Thread inventory is a drawer, closed at rest on narrower screens, retaining creation, filters and attention status.

A focused Thread has an inset fine frame, a line connecting it to the selected branch control, and a compact back/name/view/actions strip. The visible name belongs to this context strip; the composer conveys its destination through its accessible name without a repeated address row. Conversation and artifact controls use icons; actual Thread titles, artifact titles and conversation prose remain text. Owner messages sit right in a filled primary bubble and Agent messages left on the surface; alignment alone identifies the speaker, so no message carries an avatar. Referenced artifacts stay compact rows. Send is visible on desktop and phone, while Enter and touch queue gestures remain available.

At phone widths the same rail and frame distinguish context; the drawer is modal and the artifact preview is full-screen. Keyboard navigation can enter an interactive artifact, return to its controls, and dismiss with Escape without granting host capabilities. The approved hierarchy changes the conversation model; mockup icon replacements are not part of the implementation.

Inventory rows identify Space or Task and show real working duration and queued turns alongside independent attention, unread and visibility. The row's meta column carries the computed state in one short token or two words — needs you, running, queued n, failed, stopped, until <wake>, done — rather than recency alone; recency moves into the row tooltip. Threads waiting on the Owner also head the tree in a “Needs you (n)” band, and the focused Thread's header wears a “Needs you · <waited>” pill; the band, the pill and the overview queue read one selector. Idle Spaces suppress successful terminal-turn completion cues. Tasks offer explicit Mark done / Reopen, independently of the last turn’s outcome. Activity recency excludes read and lifecycle edits. The title, Task-only quick Mark done/Reopen and persistent actions menu are sibling controls; row menus also open with right-click or Shift+F10 inside the native dialog. Opening the drawer preserves the underlying canvas colors.

The Thread drawer uses a compact filter menu beside Search, preserving its selected view when reopened. Creation and browsing have explicit opening intent with one focus owner. A quiet rail marker aggregates visible needs-owner Threads independently of unread state, and updates at snooze expiry. The existing command palette searches Thread titles and exact #references; filtering preserves the addressed conversation until a result is chosen.

Execution details are the run card's own disclosure, beneath its exact final assistant message, never a separate inspector. A running or no-final turn remains beside its addressed Owner request; ownerless background execution follows its real start time. Pagination never pulls unloaded historic execution into the visible page. Markdown artifacts render through the same safe parser as conversation, inside the isolated preview, with original-source downloads retained. An artifact's kind alone decides its surface and the openers its card offers. The selected Thread filter has a quiet caption; Search opens the existing palette with Thread intent. Phone context retains an untruncated numeric Thread reference. Saved drafts from another history are available for manual copying without attaching to reused IDs.

### Thread identity, work steps and processes

Every Thread carries an avatar: a generated letter default, a chosen emoji, or an uploaded PNG, JPEG or WebP center-cropped to a square. Shape carries kind — rounded square for a Space, circle for a Task — and the same icon appears in inventory rows, the Thread header, the Info pane and inline Thread chips. Icon edits are revision-guarded and start no execution.

A turn's work is one flat wrapping row of step pills in chronological order; the Agent's program cell is a peer of the tool calls beside it, not their parent. One step is open at a time and its detail panel opens below the whole row. Reasoning and provisional prose are full-width rows in the same sequence, and raw turn data stays in the run card's Technical details.

Thread Info is a pane inside the same frame, not a separate sheet. It holds the Thread's own facts and the Owner's in-place edits: title, description and **Runs on**, which chooses the default Native route, a Native provider and model, or a CLI agent. Each edit is revision-guarded and settles only when the Host's revision advances.

A process delivery is a structured note in its owning Thread: process name, the trigger that fired, the outcome and the body. A wake that produced nothing to read gets no card at all; consecutive ones fold into a single quiet line. Processes list one entry per process — a dense row at rest, promoted to a card while running or expanded — grouped Running and Finished, scoped to the selected Thread and its descendants, with Cancel process and Disable trigger.

## Hierarchy and pinning

The human inventory derives a tree from required immutable parent IDs. Only top-level Threads can be pinned. Pinned roots appear once at the top of the selected lifecycle filter, ordered by pin timestamp then numeric ID; their descendants remain under that same root. Filtered children retain parent context. Selected ancestry expands on opening; bounded indentation keeps deep rows usable on phone. Arrow keys navigate visible rows and expand/collapse branches without changing the recipient. Row, header and palette share root-only Pin/Unpin and kind-aware creation actions. The top level and Spaces offer New Space / New Task; Tasks offer New Task only. The creation form names its parent and sends both parent and kind explicitly. Owner conversion preserves the conversation and reports invalid parent/child combinations without changing the tree.

Every selected Thread, including retained ID 0, has the same context strip and actions. The overview has no addressed composer. Escape closes the active control without selecting a different conversation. Missing explicit links show an unavailable destination; no pinned, recent or first Thread silently becomes the recipient. Per-history drafts and artifact navigation retain their established isolation.

Child reports are chronological parent activity with exact child and source-turn links, concise Markdown text and normalized artifact references. Parent local turn identity remains null for these reports; the child's inspector is never attached to a parent message. Conversation Canvas instances are filtered to the selected Thread; global human artifact discovery remains available without changing recipient.

The current assignment remains available in a compact Current brief disclosure with its artifact references even when its historical assignment activity is outside the visible page. The original activity keeps its exact chronological position. This is current Thread context; accepted execution turns retain their own assignment snapshots.

Artifact previews stage a single compact About row inside the addressed composer, with its title and a 44px remove control. It belongs to the history/Thread draft, not to the globally open preview. Close, phone return and Conversation/Artifacts toggles preserve it; another preview replaces it. A quiet Use in message action in the preview can re-stage the result for the named selected Thread. Overview browsing has no recipient. Sending snapshots the context as an explicit message reference and clears the submitted context; failed pending rows retain their artifact card for an exact retry. No reference is granted while browsing.

## Related references

Related is the per-Thread companion to Conversation. It groups explicitly saved web links and Thread references with the existing artifact references; the global Artifacts inventory stays available. Recognizing a URL in prose creates no saved record or artifact. Native links preserve authored labels and browser navigation, with local resource icons and adjacent Open, Copy and Add to Related actions. URLs can be added directly, and a title or number search adds Thread references. Removing an item removes only its association with this Thread. Thread references use current titles and do not create hierarchy edges or access grants. The original conversation and artifact references remain unchanged.

Saving or removing a link addresses the captured Thread and history, never whichever Thread happens to be selected later. Related does not send a message, start work, alter a draft or grant resource access. Its list survives reconnect through authoritative snapshots. Artifact previews keep their existing isolation, recipient and draft behavior; they expose no Related action bridge.

Portable Thread references are ordinary Markdown links to `/t/{id}?history={uuid}` on this app’s origin. A matching authoritative hello is required before selecting the recipient or resolving its title. A bare `/t/{id}` with no history resolves against the current history and rewrites the address; a malformed, old-history or missing destination stays unaddressed with a return to the Thread overview. Local `#id` shorthand remains available within the current history.

## Persistent showcase

The selected Thread may keep one artifact in an independently scrolling right-hand pane. It is a durable part of that Thread, separate from a temporary preview or the composer's About context. Artifact reference menus provide **Showcase in this thread**. The pane identifies its artifact and offers replacement and removal without changing the addressed conversation. On phones, a dedicated view keeps the artifact usable without compressing the chat. A showcase follows live artifact updates and never exposes host capabilities to embedded content.
