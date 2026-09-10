# Ongoing conversations and finishable work

> **Adoption note (2026-09-10):** [#51](https://github.com/SamGalanakis/hirsel/issues/51) adopts **Spaces and Tasks** sharing one Thread identity, with Spaces containing either kind and Tasks containing Tasks only. [ADR 0018](../../adr/0018-spaces-and-tasks.md) records the decision. The unrestricted optional-tracking recommendation below remains historical and was not adopted unchanged.

Research for [#50](https://github.com/SamGalanakis/hirsel/issues/50), 2026-09-10. **Recommendation, not an adopted decision or implementation approval.** Hirsel source baseline: `bbca7831bf6224535aba58b5bd3b4c927a608c1b`.

Keep one Thread identity and make task tracking optional. A conversation can organize other conversations, carry a bounded outcome, or do both. The useful distinction is whether someone has committed to an outcome—not whether the Thread is a root, has children, or has just finished an agent turn.

This refines the proposed Conversation/Task modes rather than disproving their architecture: that proposal already used one Thread model at any depth. A binary mode and optional tracking can encode the same capability. The meaningful product choices are how tracking begins, what Done means, and what happens when tracking ends.

## The actual mismatch

Current doctrine describes a Thread as an ongoing subject or unit of work, gives every Thread explicit settlement, and keeps execution, attention, reading and visibility independent. Projects are ordinary root Threads. ADR0016 superseded the earlier global-conversation/Task-margin proposal; that older report is not current policy. [Current doctrine](https://github.com/SamGalanakis/hirsel/blob/bbca7831bf6224535aba58b5bd3b4c927a608c1b/PRODUCT.md#L7-L28), [ADR0016](https://github.com/SamGalanakis/hirsel/blob/bbca7831bf6224535aba58b5bd3b4c927a608c1b/docs/adr/0016-threads-own-conversation.md#L3-L11).

The code does not confuse turn completion with settlement. Its inventory nevertheless continues showing the last terminal turn, with a checkmark for “Turn finished,” whenever nothing is running or queued; every Thread also receives Settle/Reopen. That gives an ongoing subject completion-shaped emphasis without recording whether it has a finishable commitment. [Status projection](https://github.com/SamGalanakis/hirsel/blob/bbca7831bf6224535aba58b5bd3b4c927a608c1b/app/src/threads/status.ts#L11-L25), [status rendering](https://github.com/SamGalanakis/hirsel/blob/bbca7831bf6224535aba58b5bd3b4c927a608c1b/app/src/threads/ThreadStatus.tsx#L9-L20), [actions](https://github.com/SamGalanakis/hirsel/blob/bbca7831bf6224535aba58b5bd3b4c927a608c1b/app/src/threads/actions.ts#L12-L24).

## Three coherent choices

| Choice | User experience | Gain and cost |
| --- | --- | --- |
| Universal Thread closure | Create any Thread; leave it open indefinitely or explicitly close it. Improve resting status and action wording. | Smallest change and preserves current doctrine. Cannot distinguish “nothing committed” from “unfinished commitment.” |
| **Optional task tracking on a Thread** | Create a conversation freely; **Track as task** when an outcome matters. Show completion controls only while tracked. | Preserves identity, conversation and hierarchy while making outstanding work meaningful. Adds an explicit transition and requires intelligible withdrawal/history semantics. Equivalent to two modes if their entry and conversion behavior match. |
| Separate organizers and work records | Conversations/organizers link to independently managed work items. | Useful if one conversation needs many independently managed outcomes or work items need multiple conversations. Introduces ownership, navigation and conversion decisions not justified by this narrow problem. |

Choose optional tracking if the absent-versus-unfinished distinction matters. Otherwise, universal closure with better presentation is sufficient. Peer source establishes workable mechanisms; it does not prove which wording users will understand best.

## The smallest coherent tracking experience

The following is proposed behavior, not the current contract:

- New Threads start as conversations without a mandatory mode picker. **Track as task** asks “What outcome do you want?” The existing title/description can hold that answer; no separate outcome field is justified yet. A deliberate New task shortcut could enter the same flow.
- Tracked, open work offers **Mark done**; completed work offers **Reopen**. A conversation without tracking has no completion checkbox or action. Both remain fully conversational and can have children.
- **Stop tracking** withdraws an open commitment; it does not declare success. Explain that consequence beside the action. Returning completed work to an ongoing conversation must preserve its recorded completion. Reopen, withdrawal and later tracking should leave durable history, not silently erase earlier decisions. This adds implementation work and user concepts beyond renaming a kind flag; exact history representation remains undecided.
- Resting ongoing rows emphasize conversation recency. Preserve current live work/queue and attention signals; retain failure and terminal-turn details with explicit turn scope. An outstanding-work filter includes tracked open work, keeping contextual ancestors without implying that those ancestors are tasks.

| Example | Meaning |
| --- | --- |
| Lash | Ongoing conversation; can organize children without being unfinished work. |
| Fix artifact layout | Tracked outcome; root or child. Finishing it does not finish Lash. |
| Trip planning | May remain exploratory, or track an accepted itinerary while organizing children. |
| Book flights | Tracked outcome under Trip planning, or a standalone Thread. Its completion need not complete the itinerary. |

Containment continues to express context and delegation. It does not automatically create acceptance dependencies, completion rollups, or permission to move conversations across scopes. Archive remains visibility/retention rather than accepted completion; attention asks for input; a turn records an execution attempt. Those distinctions already exist and are not new prospect findings.

## Who may say Done

Current PRODUCT assigns settlement to the Owner. Ordinary agent update schema exposes instrument and attention, not done/review state. Owner-invoked generated controls can settle when their validated action says `settles`; this happens after submitting the turn and does not wait for its success. Thus the current mechanism represents explicit invocation, not an outcome-verification gate. [Agent schema](https://github.com/SamGalanakis/hirsel/blob/bbca7831bf6224535aba58b5bd3b4c927a608c1b/crates/hirsel-host/src/lash_runtime/thread_schemas.rs#L5-L15), [Owner and instrument actions](https://github.com/SamGalanakis/hirsel/blob/bbca7831bf6224535aba58b5bd3b4c927a608c1b/crates/hirsel-host/src/thread_commands.rs#L170-L258).

Recommend retaining Owner acceptance by default. An agent can report its result or request review through existing messages/reports and attention. A new Ready for review state is optional, not required for this design. Allowing agents to mark work done is a separate authority decision; a finished run or child handoff should not imply it. Completion-bearing generated controls would also need tracking eligibility and clear acceptance wording.

One bounded implementation caveat: the plugin context has a scoped settlement capability, but the installed plugin registry is empty. This is a latent authority surface to account for if the contract changes, not evidence of a shipped agent auto-completion tool or a new runtime-audit finding. [Scoped capability](https://github.com/SamGalanakis/hirsel/blob/bbca7831bf6224535aba58b5bd3b4c927a608c1b/crates/hirsel-host/src/plugins/scoped_ctx.rs#L112-L122), [registry](https://github.com/SamGalanakis/hirsel/blob/bbca7831bf6224535aba58b5bd3b4c927a608c1b/crates/hirsel-plugins/src/registry.rs#L9-L12).

## Verified peer mechanisms, ranked for this decision

| Rank | Reference and actual mechanism | Transfer and limit |
| --- | --- | --- |
| 1 | **Org-mode:** any heading can acquire or lose TODO status while remaining in the notes tree; dependency blocking is configurable. [Manual](https://github.com/bzg/org-mode/blob/7b9d6dbe0be2ea50f272e1ec9b61c46bbd100896/doc/org-manual.org#L3944-L3967), [state operations](https://github.com/bzg/org-mode/blob/7b9d6dbe0be2ea50f272e1ec9b61c46bbd100896/lisp/org.el#L9747-L9759), [blocking configuration](https://github.com/bzg/org-mode/blob/7b9d6dbe0be2ea50f272e1ec9b61c46bbd100896/lisp/org.el#L2129-L2173). | Direct evidence that unified hierarchy need not make every item finishable. Does not establish Hirsel acceptance authority or the proposed withdrawal-history contract. |
| 2 | **Plane:** inspected Project model has archive but no completion/status field; Module has dates/status and links to work items. Work-item type and lifecycle state are separate. [Project](https://github.com/makeplane/plane/blob/2f895b82dad839c730c36a5c0cbc046f1e5d6b56/apps/api/plane/db/models/project.py#L68-L120), [Module](https://github.com/makeplane/plane/blob/2f895b82dad839c730c36a5c0cbc046f1e5d6b56/apps/api/plane/db/models/module.py#L67-L99), [work item](https://github.com/makeplane/plane/blob/2f895b82dad839c730c36a5c0cbc046f1e5d6b56/apps/api/plane/db/models/issue.py#L104-L170). | A bounded organizer can itself have an outcome. Supports the heavier separate-record option, not a rule that all containers are ongoing. |
| 3 | **Vikunja:** project parentage, task completion fields and task-relation records are separate structures. [Project](https://github.com/go-vikunja/vikunja/blob/a59dbc2842ca7ee014e36e894a77349540fa475d/pkg/models/project.go#L39-L54), [task](https://github.com/go-vikunja/vikunja/blob/a59dbc2842ca7ee014e36e894a77349540fa475d/pkg/models/tasks.go#L75-L122), [relations](https://github.com/go-vikunja/vikunja/blob/a59dbc2842ca7ee014e36e894a77349540fa475d/pkg/models/task_relation.go#L49-L63). | Distinguish organizational containment from work decomposition. Does not justify replacing Hirsel's scoped immutable parentage or adding another hierarchy now. |
| 4 | **Focalboard:** boards define card properties; dragging a card writes an option into the view's grouping property. The guide's Completed column is a property convention in the inspected model/UI. [Guide](https://github.com/mattermost-community/focalboard/blob/a84bbb65e32edf972856b329417096ac413518e9/website/site/content/guide/user/_index.md#L41-L52), [drop handling](https://github.com/mattermost-community/focalboard/blob/a84bbb65e32edf972856b329417096ac413518e9/webapp/src/components/kanban/kanban.tsx#L111-L138). | Filtering/grouping can present the same identity differently. A label or checkbox alone does not define authoritative completion; no general property system is recommended. |
| 5 | **OpenHands:** this checkout is Agent Canvas, a frontend. It distinguishes execution state, goal verdicts and automation-run contracts. [Architecture](https://github.com/OpenHands/OpenHands/blob/0a5a65c5a18a5054cdfaeed1d87c5d6924b44db1/docs/architecture.md#L5-L31), [goal contract](https://github.com/OpenHands/OpenHands/blob/0a5a65c5a18a5054cdfaeed1d87c5d6924b44db1/src/types/agent-server/core/events/conversation-state-event.ts#L66-L97), [run contract](https://github.com/OpenHands/OpenHands/blob/0a5a65c5a18a5054cdfaeed1d87c5d6924b44db1/src/types/automation.ts#L79-L160). | Compare outcome evidence with execution status. Frontend types do not prove backend enforcement or human acceptance; copying an automation engine is excluded. |

## Considered, not adopted

- A mandatory Conversation/Task creation picker: structurally valid, but makes users classify before they may know the outcome. Opt-in tracking is the recommended entry point, not a different identity model.
- Separate Project/Task entities, configurable status workflows, a general property system, and multiple outcome records per conversation: peer mechanisms with additional complexity, not needed to answer this question.
- Root means ongoing / child means task; automatic completion cascades; all children as mandatory dependencies: hierarchy alone does not establish these meanings.
- Agent turn finished means task done; Ready for review as a mandatory new enum; delegated acceptance inferred from child ownership: no such authority decision is adopted.
- Copying peer archive cascades, file moves, automation engines or execution-status labels: outside scope or inconsistent with the problem being solved.

The only proposed deltas against existing work are semantic actions/filtering alongside #49 and inventory emphasis alongside #13. No responsive-layout, timeline, nesting, pinning, artifact or runtime implementation is proposed. [Full exclusion map](exclusions.md).

## Method, convergence and verification

Five independent reference readers worked in parallel from local clones, followed by two independent Hirsel doctrine reviews: one read Org's manual and Plane's written product doctrine; the other read OpenHands architecture and Org's completion doctrine, then checked organizer UX and completion authority. Both converged on optional finishability at any depth. A fresh semantic verifier independently agreed, correcting the misleading suggestion that this is architecturally different from one-model Conversation/Task modes.

That is reviewer convergence, not unanimous peer endorsement. Plane and Vikunja readers favored separate work records; Focalboard supplied flexible properties/views; Org supplied optional TODO semantics. The recommendation weighs these alternatives against Hirsel's current conversation identity rather than counting references as votes.

Separate mechanical verifiers re-read selected claims from all five clones. Verification narrowed Focalboard's absence claim to inspected model/API paths, corrected an OpenHands evidence path, and rejected backend-authority inference and richer unverified Plane workflow claims. Only retained, verified mechanisms ship here; broader provisional reader suggestions remain outside the report. [Domain verification](verification/domain.md), [Plane and Org verification](verification/organizers.md), [other peer verification](verification/peers.md).

All source links pin the inspected commits. This was static source research: no application changes, builds, product calls, UX experiments or new design tickets. The recommendation needs a product decision before implementation; peer evidence does not validate the proposed wording empirically.
