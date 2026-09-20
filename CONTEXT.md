# Hirsel

Hirsel is a personal orchestration system for one Owner. The Owner and Agents work through conversational Spaces and Tasks.

## Language

**Owner:** The person Hirsel serves.

**Agent:** An assistant working within a Space or Task and its descendants. Each conversation retains its own context and execution.

**Space:** An ongoing place for conversation, context and organization. A Space may contain child Spaces or Tasks and has no work-completion state.

**Task:** A finishable outcome with its own conversation. A Task may contain child Tasks only; completing a child or an Agent turn does not automatically complete its parent.

**Thread:** The durable conversational identity shared by a Space or Task, including its Messages, Turns, Activity and generated instrument. The Owner-facing kinds are Space and Task.
_Avoid_: Thread as a third kind alongside Space and Task

**Space chat:** The conversation of any Space. It talks with the Owner and dispatches work to Tasks; it does not perform the work itself. This is guidance and presentation, not an authority or execution profile.

**Worker:** The role of a Task's conversation. It performs the accepted work using the Thread's selected backend and universal tool surface.

**Worker recipient:** The Task worker the Owner has stepped into and is messaging directly. “Talk about this” instead addresses the containing Space chat and drafts an ordinary `#id` reference.

**Conversation:** The exchange of Messages within one Space or Task.

**Message:** Something the Owner or Agent said in exactly one Thread. References to other Threads provide context without changing ownership.

**Turn:** One Agent execution cycle for a Thread. Its execution outcome does not complete the Task.

**Activity:** Something that happened within a Thread, such as progress, an instrument update, or a Task completion action.
_Avoid_: Event as a work object

**Attention:** Whether a Thread currently needs the Owner. Attention can change independently of Task completion, read state, and execution.

**Headline:** A Thread's normalized, nonempty summary of at most 12 words. A Thread stores its own headline plus the displayed rollup, previous displayed headline and seen revisions directly on `threads`.

**Rollup headline:** A Host-derived child-count summary used when a Thread has children. It follows fixed status precedence and numeric-ID tie-breaking, never copies a child's prose and never completes the parent.

**Outside change:** One ordinary per-turn-coalesced activity on the affected Space chat when a Thread in another top-level Space changes it. It injects no context; the Agent rereads state when needed.

**Derived status:** One Host projection from durable Thread facts: needs you, running, queued, hung, sleeping or idle, always with a reason. It is descriptive and never cancels or retries work.

**Thread ref:** A citation written as `#<id>`. Citing a Thread does not move a Message into it.

**Generated instrument:** A constrained semantic interface attached to a Thread. It may change through multiple stages while the Thread's identity and conversation remain stable.

**Process:** An observable Lashlang workflow or Sub-agent run coordinated by the Agent. Its execution lifecycle is separate from Task completion.

**Sub-agent:** An external coding agent driven through a native Sub-agent Driver. It reports to the Agent.

**Host:** The long-running owner of Hirsel's durable state and execution.

**Artifact:** An explicitly agent-published result with global identity and current mutable content. It has no owning Thread and no revision history. Formats are Solid 2 JSX, HTML, or UTF-8 files. Interactive previews are local only, without network or backend access.

**Artifact reference:** A Message-to-Artifact link. Create, edit and show publish a reference card in the current conversation. Many Threads can reference the same Artifact; earlier cards resolve the current content. References do not transfer Message ownership or alter the composer's addressed Thread.
