# Product scenario: Spaces and Tasks

## Owner-visible outcome

The Owner can create a top-level Space or Task. A Space can contain Spaces and
Tasks; a Task offers only child Tasks. Tasks can be marked done and reopened.
Valid kind changes preserve the Thread identity and conversation, while a Space
with a Space child cannot become a Task and a child of a Task cannot become a
Space. The rejected action remains visibly explained and leaves no partial
change.

Spaces remain ongoing coordination surfaces. They expose neither Owner nor
generated completion actions and do not show a stale successful-turn badge.
Their live queued, needs-input, and request-error states remain visible. Space
and Task identity, nesting, root pinning, and the selected conversation remain
legible at desktop and phone widths.

Archiving the selected Thread returns to the unaddressed overview only after the
Host accepts the action. Its conversation and unsent draft remain intact; the
Thread is discoverable under Archived and can be explicitly reopened. Browsing
Archived and unarchiving do not silently choose a different recipient. The
drawer header and New Space/New Task controls remain distinct, unclipped, and
at least 44px tall at 320px, 390px, and desktop widths.

## Isolated no-model check

This scenario creates and changes the core hierarchy through the production web
controls against a fresh isolated Host store on an unused non-3076 port. It
reconciles each checkpoint with authenticated WebSocket snapshots and SQLite,
including IDs, kinds, parentage, revisions, completion, and pinning. It reloads
after Task completion, conversion, and pinning. The scripted/fake service makes
no provider call.

Before the fixture-only layer, the runner also sends a unique conversation
marker through the real composer, leaves a second unique draft unsent, archives
that selected Thread through its action menu, and reconciles the overview route,
remembered selection, browser draft, authenticated history, Archived inventory,
explicit archived selection, unarchive behavior, and SQLite row. Empty and
populated drawer states are measured and captured together at 320px, 390px, and
desktop widths; the measurements cover header overlap, utility and creation
target size, label clipping, and icon-label gap.

After those UI claims pass, the runner clearly enters a disposable fixture-only
layer. It first writes a completed turn, `needs_owner`, and an instrument with
one continuing and one completing action into the isolated store. The real Host
must project Needs you while suppressing both the completed-turn success badge
and the completing action on the Space. A second checkpoint adds one inert
queued turn and requires Queue to remain visible. This fixture is presentation
evidence; it is not a model-generated instrument proof.

Build the exact integrated checkout first, then run:

```bash
cd app && npm run build
cd .. && cargo build --workspace --all-targets
node e2e/spaces-tasks-runbook.mjs
```

`HIRSEL_SPACES_HOST_BIN` may point at the exact integrated Host binary.
`HIRSEL_SPACES_EVIDENCE` may name the evidence directory. The runner records the
source identity, browser frames without sent authentication, DOM extracts,
authenticated `threads_list` and `open_thread` snapshots, SQLite extracts, and
screenshots. It exits on the first failure, preserves the failure evidence, and
tears down only the isolated Host process group it started.

## Agent judgment

Inspect the desktop, 390, and 320 CSS-pixel screenshots in one bounded pass. Confirm
that Space avatars read as softly square and Task avatars as round; the kind
labels and selected context are easy to scan; hierarchy remains readable; the
pinned Task appears once; Task completion and conversion controls use clear
language; both invalid conversions show an understandable error without losing
the selected conversation; the archive returns to the overview while preserving
history and draft and remains explicitly reopenable; and queued plus needs-input
state remains visible on the Space while completion controls are absent. Confirm
from the empty and populated layout JSON that the header groups do not overlap,
all measured controls are at least 44px, both creation labels remain visible and
unclipped, and each icon-label gap is at least 8px. Record the exact screenshot
or DOM/store row for every decision and replace `NOT_JUDGED` with a separate
judged verdict in the handoff.
