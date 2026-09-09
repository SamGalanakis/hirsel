# Hirsel interface

The primary screen is the Home conversation or one focused Thread; a compact rail opens the Thread inventory. There is one Agent and one composer; the composer addresses the focused Thread's own conversation.

A Thread row shows its title and useful independent signals: needs your input, unseen activity, execution, snooze and settlement. Informational content is never a reason to hide a Thread. An empty Thread remains real work and shows an empty conversation with its instrument, if any.

The focused surface contains the Thread's messages, current generated instrument, and relevant activity. A message or activity from another Thread cannot appear merely because it arrived while this surface was focused. Cross-thread references navigate to the identified Thread without rewriting ownership.

Read state is separate from open/settled. Opening a Thread can mark it seen; it cannot settle or clear a pending decision. The Owner has explicit settle/reopen controls. Archive and snooze change visibility while preserving settlement. A completed Agent turn means execution finished; the Thread may still require action.

Generated instruments use the existing constrained JSON component vocabulary. The current instrument is mutable; Thread identity and conversation history are stable. Instrument actions carry the displayed revision, and the Host rejects stale actions. `continue` advances the instrument in the same Thread. An explicitly completing action settles it.

Streaming prose, reasoning and tool activity carry Thread and turn IDs. The UI resets a stream for a newly running turn and rejects delayed frames from earlier turns. Durable messages and activity remain available after reconnect.

The coordinator Thread represents global coordination. It is not an overlay that merges all other conversations. Existing process/settings surfaces remain operational controls; they do not define work identity.

[ADR 0016](docs/adr/0016-threads-own-conversation.md) is authoritative where older design notes describe Tasks, Pings, or a single shared conversation history.

## Thread workspace surface

The established visual system remains authoritative in `app/src/styles.css`: Inter, mint primary, slate neutrals, theme-aware surfaces, and the cube mark. The Thread workspace uses a compact icon rail and a named Thread drawer at every width. Selected rows have a quiet fill; Conversation and Artifacts have explicit selected states. Lifecycle operations are grouped in Thread actions.

Conversation contains messages, artifact reference cards, and explicit owner-facing summaries. Inspect execution holds detailed tools, turn states, and diagnostic activity. The focused frame and context strip keep the addressed Thread clear while another result is open; its composer also names the destination for assistive technology. The conversation viewport follows its latest content when the composer grows, preserving the user's position when they scroll back.

Artifact previews occupy a desktop side pane or a full-screen phone surface below 1024px. Phone previews trap focus; desktop panes allow movement back into the conversation. Close and Escape return focus to the invoking control. Artifact content is a separate isolated document and does not inherit the host's interaction capabilities.

The surface brief is `.impeccable/briefs/thread-workspace.md`.

### Compact rail and focused frame

The Owner approved option A on 2026-09-09: a 56px icon rail and titleless Home conversation. The rail carries Home, Threads, New thread, global artifacts and Processes, with Settings at its foot. Utilities appear once and use licensed Lucide icons with accessible names, tooltips and 44px targets. The Thread inventory is a drawer, closed at rest, retaining creation, filters and attention status.

A focused Thread has an inset fine frame, a line connecting it to the selected branch control, and a compact back/name/view/actions strip. The visible name belongs to this context strip; the composer conveys its destination through its accessible name without a repeated address row. Conversation and artifact controls use icons; actual Thread titles, artifact titles and conversation prose remain text. Messages use sender icons and compact referenced-artifact rows. Send is visible on desktop and phone, while Enter and touch queue gestures remain available.

At phone widths the same rail and frame distinguish context; the drawer is modal and the artifact preview is full-screen. Keyboard navigation can enter an interactive artifact, return to its controls, and dismiss with Escape without granting host capabilities. The approved two-panel proposal illustrates two application states and is not itself shipped.

Thread inventory rows show real working duration, queued turns and the latest terminal outcome alongside independent attention, unread, settlement and visibility. Completion reads “Turn finished”; only the Owner settles a Thread. Activity recency excludes read and lifecycle edits. The title, quick settle/reopen and persistent actions menu are sibling controls; row menus also open with right-click or Shift+F10 inside the native dialog. Opening the drawer preserves the underlying canvas colors.
