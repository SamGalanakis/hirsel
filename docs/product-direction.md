# Product Direction: effortless orchestration through Task Margins

Status: authoritative current direction, agreed 2026-07 and refined 2026-07-23. It supersedes the visible Chat/Tray/Side Chat and event-queue/scroller concepts recorded in older ADRs. Those documents remain historical evidence; `PRODUCT.md`, `DESIGN.md`, `CONTEXT.md`, and this document define the current product.

## 1. Thesis

Hirsel should make coordinating a fleet of agents feel like working with one excellent, globally aware collaborator.

The scarce resource is Sam's judgment and taste. Conventional multi-agent tools waste it in two opposite ways: operator consoles force him to dispatch sessions and reconstruct context, while hands-off autonomy silently makes the very decisions that need his taste. Hirsel puts autonomy between decisions and routes meaningful boundaries back through one Agent.

The experience must be both minimal and surprising: almost no standing UI, yet the right interface can materialize for the work at hand and transform as that work advances.

## 2. One Agent, two conversational scopes

There is one Agent and one standing composer.

- The resting state is the **global conversation**, aware of all Tasks and processes.
- Opening a **Task** changes the subject and reveals its related conversation in the margin.
- The composer becomes Task-scoped by adding the Task Anchor and mention to an ordinary message.
- Removing the scope speaks globally while the Task stays open.
- The Agent remains clued in to global and Task-scoped exchanges. A dive never creates a separate session, agent persona, or conversation destination.

This is how Hirsel permits deep work without a nested labyrinth: location is flat, scope is explicit, and escape to the global Agent is one reversible gesture.

## 3. Tasks are the only durable visible objects

A Task is a stable place for one piece of work. The flat Task inventory answers what exists, what is selected, and which items need the Owner. The selected row carries identity and status; the main field is reserved for the work itself.

A Task owns:

- stable identity and Anchor;
- a related conversation slice with hard boundaries against other Tasks;
- an explicit open/done lifecycle;
- a constrained generated instrument;
- optional accompanying Views.

The UI does not add a second overview, feed, evidence ledger, nested task tree, or per-Task chat destination. Tasks may be reordered by urgency, but “queue” is an implementation policy, not the product metaphor.

## 4. The generated instrument is the interface

Each Task may carry a validated semantic JSON tree. A small renderer maps that tree onto the design system. The catalog includes headings, prose, key/value data, status, choices, fields, submit actions, insets, and accompanying View slots.

The constraint is the source of power:

- generated content cannot choose raw color, arbitrary layout, glow, or type scales;
- the renderer owns hierarchy, accessibility, responsive behavior, motion, and safe fallback;
- the same semantic tree can be rendered by web and native clients;
- unknown nodes remain visible as a quiet fallback instead of breaking the Task;
- text is rendered as text, never injected HTML.

This is not a card generator or a collection of mini-apps. It is a small instrument language that lets Hirsel produce the exact control surface a moment needs while the surrounding product stays calm.

## 5. Instruments can recompose in place

An action is not synonymous with settlement. A structured Task action may:

1. settle the Task, or
2. transition the same Task to another generated stage.

The deploy reference flow demonstrates the model:

```text
Ship build 4821?
        │ Ship now
        ▼
5% canary healthy — promote?
        │ Promote to 100%
        ▼
Build 4821 live · done
```

Across the transition, Task id, Anchor, selected row, composer scope, and related conversation remain stable. Reopen restores the last actionable stage rather than resetting to an unrelated beginning. The mock expresses this through declarative stages/transitions; production contracts should preserve that generality instead of hard-coding deployment semantics.

This is the mind-bending part delivered quietly: the interface understands the phase of work and becomes the next instrument without navigation.

## 6. Conversation lives in the margin

Conversation supports the Task rather than dominating the screen. The generated question or status leads; related Owner/Agent exchanges sit in an organic margin beside or below it. Durable Anchor and mention boundaries prevent one Task's messages from bleeding into another even when reply chains interleave.

There is no drill-in Side Chat. If the Owner needs a global comparison during a Task, he removes scope and asks the globally aware Agent in place. If he needs local depth, he leaves scope active. The open Task never disappears merely because conversational scope changes.

## 7. Utilities are summoned, never visited

Processes, Settings, and Canvas are temporary utilities:

- **Processes** makes background work observable and routes steering back through Hirsel (for example, “Ask Hirsel to stop”).
- **Settings** changes local/client configuration without becoming a product destination.
- **Canvas** hosts a larger generated View when one exists; absence is honest, not an empty permanent tab.

Presentation follows the visit. **Processes** is a glance kept beside the work, so on desktop it docks as an inspector at the field's right edge and on phone it is a modal sheet; **Canvas** docks the same way. **Settings** is an infrequent, deep, form-shaped visit — grouped rows, identity, prompt editors — with nothing to compare against the field behind it, so it is a full-viewport modal overlay at every width, reading in the same measure the Task world holds rather than a narrow rail. Every one of them is focus-trapped while it is up, and its Escape dismisses it alone. Closing restores the exact Task, scope, draft, scroll position, and usable focus. No utility contains another Task inventory or conversation.

## 8. Visual and interaction character

Task Margins is deliberately soft, spare, and non-rectilinear:

- warm blue-charcoal and mint interaction, with literal status text rather than color alone;
- plain rows and hairlines instead of card grids and aggressive boxes;
- one organic conversation aperture and one organic composer capsule;
- generated questions lead with generous type and space;
- Task identity/status stay in the selected row and scope control rather than repeating as desktop banners;
- motion explains recomposition and focus, never withholds content;
- 44px phone targets, roving task keys, honest shortcuts, and no document-level overflow down to 320px.

Minimal does not mean inert. It means every visible thing either establishes the current subject, enables a decision, or carries the conversation forward.

## 9. Runtime mapping and compatibility

| Product concept | Current implementation |
| --- | --- |
| Task | typed `EventItem` replayed by `event_upsert` |
| Generated instrument | `ui` tree rendered by `EventCardRenderer` |
| Stage or settlement action | `event_action { event_id, action, data }`; node settlement intent is explicit |
| Task conversation | ordinary messages selected by Anchor/ref and Task mentions |
| Task placement for accompanying View | legacy `ping:<id>` wire spelling |
| Global conversation | ordinary `send_message` with no Task Anchor/mention |
| Processes | replayed/streamed `ProcessInfo` plus temporary inspector |

`ping`, `Chat`, `Side Chat`, and queue/scroller language survives in historical docs, protocol fields, debug routes, and compatibility tests. It does not license those concepts as visible destinations.

## 10. Product invariants

1. One Agent, one composer, one flat Task inventory.
2. Opening a Task changes subject, not interlocutor.
3. Ambient is the absence of Task focus, not a named mode or destination.
4. Hirsel knows about work across every Task margin.
5. Generated UI is constrained, semantic, responsive, and safe by construction.
6. A Task can adapt through multiple stages without losing identity or context.
7. Only explicit settling actions close a Task; conversation alone preserves lifecycle.
8. Utilities always return to the same world.
9. Status is readable without color; shortcuts are shown only when implemented.
10. No nested destinations, duplicate identity, dashboard noise, or arbitrary generated chrome.

## 11. Deferred compounding layers

The eventual taste store can turn repeated decisions into standing guidance, citations, and proposed amendments. That is the leverage endgame, but it should grow from real Task decisions rather than precede them. General memory, voice, push, arbitrary UI, and built-in workflows remain deferred until the core orchestration loop proves a specific need.

## Provenance

This direction synthesizes the “effortless orchestration” exploration, experimental interface research, the selected Task Margins world, and implementation evidence from the current SolidJS reference client. The archived directions remain in `docs/effortless-orchestration/index.html`; Task Margins and the adaptive instrument are the selected present, not one more option in a gallery.
