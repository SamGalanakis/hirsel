# Hirsel

Hirsel is a personal orchestration system for one Owner. A globally aware Agent coordinates work across durable Threads.

## Language

**Owner:** The person Hirsel serves.

**Agent:** The globally aware orchestrator. It can inspect and coordinate every Thread while keeping each Thread's conversation distinct.

**Thread:** A durable subject of work with a stable identity, its own Messages, Turns, Activity, and generated instrument. A Thread may be active or settled; reading it does not settle it.
_Avoid_: Task, Ping, notification, conversation slice

**Message:** Something the Owner or Agent said in exactly one Thread. References to other Threads provide context without changing ownership.

**Turn:** One Agent execution cycle for a Thread. Its execution outcome does not settle the Thread.

**Activity:** Something that happened within a Thread, such as progress, an instrument update, or a settlement action.
_Avoid_: Event as a work object

**Attention:** Whether a Thread currently needs the Owner. Attention can change independently of settlement, read state, and execution.

**Orchestrator conversation:** The standing Thread for cross-Thread coordination and requests that do not yet belong to another Thread.

**Thread ref:** A citation written as `#<id>`. Citing a Thread does not move a Message into it.

**Generated instrument:** A constrained semantic interface attached to a Thread. It may change through multiple stages while the Thread's identity and conversation remain stable.

**Process:** An observable Sub-agent or monitor run coordinated by the Agent. Its execution lifecycle is separate from Thread settlement.

**Sub-agent:** An external coding agent driven through a native Sub-agent Driver. It reports to the Agent.

**Host:** The long-running owner of Hirsel's durable state and execution.

**Artifact:** An explicitly agent-published result with global identity and current mutable content. It has no owning Thread and no revision history. Formats are Solid 2 JSX, HTML, or UTF-8 files. Interactive previews are local only, without network or backend access.

**Artifact reference:** A Message-to-Artifact link. Create, edit and show publish a reference card in the current conversation. Many Threads can reference the same Artifact; earlier cards resolve the current content. References do not transfer Message ownership or alter the composer's addressed Thread.
