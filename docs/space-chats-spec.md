# Spec: Space chats, essentials only

Accepted 2026-09-21.

Every Space owns a durable Space chat. It talks with the Owner, coordinates its Space and dispatches real work to Tasks. Every Task is a worker conversation. These roles are kind-derived guidance; every Thread keeps the same tools and backend choices. Reach, grants and root are the whole authority model.

The Owner lands in the last active top-level Space used for this history, then the first active top-level Space by ID, then an ordinary newly created Home Space. Explicit bad links never redirect. “Talk about this” opens the containing Space and drafts `#<task-id>`; “Step in” opens the worker and labels the recipient plainly.

Each Thread carries a short own headline. Parents show a deterministic count/status rollup, never child prose. The Host projects a reasoned status from durable facts: needs you, running, queued, hung, sleeping or idle. A configurable hung threshold defaults to ten minutes and causes no automatic action.

Space view is chat-left/board-right on desktop and Chat/Board tabs on phone. The board filters the already-loaded inventory by top-level Space and groups Needs you, Changed since you looked (`previous_headline → headline`), then everything else. Marking displayed revisions seen is independent from conversation read state. Task view orders headline/status, instrument or showcase, children/headlines, then the existing timeline.

Reply pills are derived only from complete, parseable tool timeline payloads and refusal activities. Kinds are created, sent to, delegated, read, edited and refused. Open is the only effect action. A refused Thread can open Reach for today's subtree grant/revoke; `owner_fence` explains why no grant is offered, artifact refusals never guess a Space, and granting never retries.

Questions and orchestration use code mode rather than new Host records. A worker reports a question with options and a recommendation, continues independent safe work, and its requester answers from the Owner's established direction or asks the Owner through an instrument. Space chats never decide spending, external commitments or irreversible actions. Programs and processes on typed `thread.*` triggers handle fan-out, fan-in and batching.

Cross-Space writes add one coalesced ordinary activity to the affected Space chat. There is no context injection: reread the Thread when it matters.
