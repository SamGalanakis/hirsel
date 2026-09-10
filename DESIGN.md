# Hirsel interface

The primary screen is one selected Thread conversation, or an unaddressed overview for choosing and creating a Thread. A compact rail opens the nested Thread inventory. The composer exists only for an actual selected Thread and names its conversation.

A Thread row shows its title and useful independent signals: needs your input, unseen activity, execution, snooze and settlement. Informational content is never a reason to hide a Thread. An empty Thread remains real work and shows an empty conversation with its instrument, if any.

The focused surface contains the Thread's messages, current generated instrument, and relevant activity. A message or activity from another Thread cannot appear merely because it arrived while this surface was focused. Cross-thread references navigate to the identified Thread without rewriting ownership.

Read state is separate from open/settled. Opening a Thread can mark it seen; it cannot settle or clear a pending decision. The Owner has explicit settle/reopen controls. Archive and snooze change visibility while preserving settlement. A completed Agent turn means execution finished; the Thread may still require action.

Generated instruments use the existing constrained JSON component vocabulary. The current instrument is mutable; Thread identity and conversation history are stable. Instrument actions carry the displayed revision, and the Host rejects stale actions. `continue` advances the instrument in the same Thread. An explicitly completing action settles it.

Streaming prose, reasoning and tool activity carry Thread and turn IDs. The UI resets a stream for a newly running turn and rejects delayed frames from earlier turns. Durable messages and activity remain available after reconnect.

An ordinary Thread coordinates its children. The parent conversation shows concise progress/results and referenced artifacts, with exact child/turn identity and navigation links. Child execution remains in that child’s own conversation. Ancestor breadcrumbs and a compact child disclosure preserve navigation context without introducing another project panel. Existing process/settings surfaces remain operational controls with concrete Thread origins.

[ADR 0016](docs/adr/0016-threads-own-conversation.md) is authoritative where older design notes describe Tasks, Pings, or a single shared conversation history.

## Thread workspace surface

The established visual system remains authoritative in `app/src/styles.css`: Inter, mint primary, slate neutrals, theme-aware surfaces, and the cube mark. The Thread workspace uses a compact icon rail and a named Thread drawer at every width. Selected rows have a quiet fill; Conversation and Artifacts have explicit selected states. Lifecycle operations are grouped in Thread actions.

Conversation contains messages, artifact reference cards, and explicit owner-facing summaries. A concise work summary names the current action or completed outcome. Failed and stopped work stays visible at rest; expanding once reveals ordered steps, with raw IDs and event data under Diagnostics. Plain replies carry no empty work disclosure. The focused frame and context strip keep the addressed Thread clear while another result is open; its composer also names the destination for assistive technology. The conversation viewport follows its latest content when the composer grows, preserving the user's position when they scroll back.

Artifact previews occupy a desktop side pane or a full-screen phone surface below 1024px. Phone previews trap focus; desktop panes allow movement back into the conversation. Close and Escape return focus to the invoking control. Artifact content is a separate isolated document and does not inherit the host's interaction capabilities.

The surface brief is `.impeccable/briefs/thread-workspace.md`.

### Compact rail and focused frame

The approved nested Thread direction retains the established 56px icon rail, framed conversations and Lucide identity. The rail carries Thread overview, Threads, New thread, global artifacts and Processes, with Settings at its foot. Utilities appear once and use licensed Lucide icons with accessible names, tooltips and 44px targets. The Thread inventory is a drawer, closed at rest, retaining creation, filters and attention status.

A focused Thread has an inset fine frame, a line connecting it to the selected branch control, and a compact back/name/view/actions strip. The visible name belongs to this context strip; the composer conveys its destination through its accessible name without a repeated address row. Conversation and artifact controls use icons; actual Thread titles, artifact titles and conversation prose remain text. Messages use sender icons and compact referenced-artifact rows. Send is visible on desktop and phone, while Enter and touch queue gestures remain available.

At phone widths the same rail and frame distinguish context; the drawer is modal and the artifact preview is full-screen. Keyboard navigation can enter an interactive artifact, return to its controls, and dismiss with Escape without granting host capabilities. The approved hierarchy changes the conversation model; mockup icon replacements are not part of the implementation.

Thread inventory rows show real working duration, queued turns and the latest terminal outcome alongside independent attention, unread, settlement and visibility. Completion reads “Turn finished”; only the Owner settles a Thread. Activity recency excludes read and lifecycle edits. The title, quick settle/reopen and persistent actions menu are sibling controls; row menus also open with right-click or Shift+F10 inside the native dialog. Opening the drawer preserves the underlying canvas colors.

The Thread drawer uses a compact filter menu beside Search, preserving its selected view when reopened. Creation and browsing have explicit opening intent with one focus owner. A quiet rail marker aggregates visible needs-owner Threads independently of unread state, and updates at snooze expiry. The existing command palette searches Thread titles and exact #references; filtering preserves the addressed conversation until a result is chosen.

Execution details are compact disclosures beneath their exact final assistant message. A running or no-final turn remains beside its addressed Owner request; ownerless background execution follows its real start time. Pagination never pulls unloaded historic execution into the visible page. Explicit Markdown file artifacts render through the same safe parser as conversation, inside the isolated preview, with original-source downloads retained. The selected Thread filter has a quiet caption; Search opens the existing palette with Thread intent. Phone context retains an untruncated numeric Thread reference. Saved drafts from another history are available for manual copying without attaching to reused IDs.

## Hierarchy and pinning

The human inventory derives a tree from required immutable parent IDs. Only top-level Threads can be pinned. Pinned roots appear once at the top of the selected lifecycle filter, ordered by pin timestamp then numeric ID; their descendants remain under that same root. Filtered children retain parent context. Selected ancestry expands on opening; bounded indentation keeps deep rows usable on phone. Arrow keys navigate visible rows and expand/collapse branches without changing the recipient. Row, header and palette share root-only Pin/Unpin and New child thread actions. The creation form names its chosen parent and sends that ID explicitly.

Every selected Thread, including retained ID 0, has the same context strip and actions. The overview has no addressed composer. Escape closes the active control without selecting a different conversation. Missing explicit links show an unavailable destination; no pinned, recent or first Thread silently becomes the recipient. Per-history drafts and artifact navigation retain their established isolation.

Child reports are chronological parent activity with exact child and source-turn links, concise Markdown text and normalized artifact references. Parent local turn identity remains null for these reports; the child's inspector is never attached to a parent message. Conversation Canvas instances are filtered to the selected Thread; global human artifact discovery remains available without changing recipient.

The current assignment remains available in a compact Current brief disclosure with its artifact references even when its historical assignment activity is outside the visible page. The original activity keeps its exact chronological position. This is current Thread context; accepted execution turns retain their own assignment snapshots.

Artifact previews stage a single compact About row inside the addressed composer, with its title and a 44px remove control. It belongs to the history/Thread draft, not to the globally open preview. Close, phone return and Conversation/Artifacts toggles preserve it; another preview replaces it. A quiet Use in message action in the preview can re-stage the result for the named selected Thread. Overview browsing has no recipient. Sending snapshots the context as an explicit message reference and clears the submitted context; failed pending rows retain their artifact card for an exact retry. No reference is granted while browsing.

## Related references

Related is the per-Thread companion to Conversation. It groups explicitly saved web links and Thread references with the existing artifact references; the global Artifacts inventory stays available. Recognizing a URL in prose creates no saved record or artifact. Native links preserve authored labels and browser navigation, with local resource icons and adjacent Open, Copy and Add to Related actions. URLs can be added directly, and a title or number search adds Thread references. Removing an item removes only its association with this Thread. Thread references use current titles and do not create hierarchy edges or access grants. The original conversation and artifact references remain unchanged.

Saving or removing a link addresses the captured Thread and history, never whichever Thread happens to be selected later. Related does not send a message, start work, alter a draft or grant resource access. Its list survives reconnect through authoritative snapshots. Artifact previews keep their existing isolation, recipient and draft behavior; they expose no Related action bridge.

Portable Thread references are ordinary Markdown links to `/t/{id}?history={uuid}` on this app’s origin. A matching authoritative hello is required before selecting the recipient or resolving its title. An incomplete, malformed, old-history or missing destination stays unaddressed with a return to the Thread overview. Local `#id` shorthand remains available within the current history.

## Persistent showcase

The selected Thread may keep one artifact in an independently scrolling right-hand pane. It is a durable part of that Thread, separate from a temporary preview or the composer's About context. Artifact reference menus provide **Showcase in this thread**. The pane identifies its artifact and offers replacement and removal without changing the addressed conversation. On phones, a dedicated view keeps the artifact usable without compressing the chat. A showcase follows live artifact updates and never exposes host capabilities to embedded content.
