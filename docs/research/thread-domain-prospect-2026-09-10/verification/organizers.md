# Verified organizer mechanisms

## Verification boundary

This is an independent, read-only check of the selected claims only. Only this
report was written; no source, other report, or commit was changed. The local
clones are unchanged: `/tmp/ref-plane` is at
`2f895b82dad839c730c36a5c0cbc046f1e5d6b56` (`## preview...origin/preview`),
and `/tmp/ref-orgmode` is at
`7b9d6dbe0be2ea50f272e1ec9b61c46bbd100896` (`## main...origin/main`); neither
`git status --short --branch` output contained short changes.

Commands used: `git rev-parse HEAD`, `git status --short --branch`, `rg -n`
for the scoped symbols, and `nl -ba <file> | sed -n '<ranges>p'` for every
retained range. No builds, tests, or backend calls were made.

## Claim verdicts

### Accepted: Plane separates the container, bounded organizer, work-item lifecycle, and type

`Project` is a workspace/container model with feature flags, timezone and
`archived_at`; the complete inspected class has no status or completion field:
`/tmp/ref-plane/apps/api/plane/db/models/project.py:68-120`
([source](https://github.com/makeplane/plane/blob/2f895b82dad839c730c36a5c0cbc046f1e5d6b56/apps/api/plane/db/models/project.py#L68-L120)).
`Module` is project-scoped through `ProjectBaseModel`, has explicit start/target
dates and status choices including `planned`, `completed`, and `cancelled`, and joins work items
through `ModuleIssue`: `/tmp/ref-plane/apps/api/plane/db/models/module.py:67-99,152-168`,
`/tmp/ref-plane/apps/api/plane/db/models/project.py:180-189`
([module](https://github.com/makeplane/plane/blob/2f895b82dad839c730c36a5c0cbc046f1e5d6b56/apps/api/plane/db/models/module.py#L67-L99),
[join](https://github.com/makeplane/plane/blob/2f895b82dad839c730c36a5c0cbc046f1e5d6b56/apps/api/plane/db/models/module.py#L152-L168),
[scope](https://github.com/makeplane/plane/blob/2f895b82dad839c730c36a5c0cbc046f1e5d6b56/apps/api/plane/db/models/project.py#L180-L189)).
The product copy calls this a project milestone/group of work items with its
own periods and deadlines: `/tmp/ref-plane/packages/i18n/src/locales/en/project.json:227-249`
([source](https://github.com/makeplane/plane/blob/2f895b82dad839c730c36a5c0cbc046f1e5d6b56/packages/i18n/src/locales/en/project.json#L227-L249)).
“Finite” is therefore a supported characterization of the explicit bounded
fields, not a literal model term.

An `Issue`/Work Item separately has parent, state, `completed_at`,
`archived_at`, and a type foreign key: `/tmp/ref-plane/apps/api/plane/db/models/issue.py:104-170`
([source](https://github.com/makeplane/plane/blob/2f895b82dad839c730c36a5c0cbc046f1e5d6b56/apps/api/plane/db/models/issue.py#L104-L170)).
Stable state groups include `completed` and `cancelled`, and `_sync_completed_at`
sets `completed_at` only when the selected state’s group is `completed`, and
clears it otherwise: `/tmp/ref-plane/apps/api/plane/db/models/state.py:14-20,79-89`;
`/tmp/ref-plane/apps/api/plane/db/models/issue.py:180-183,240-255`
([groups](https://github.com/makeplane/plane/blob/2f895b82dad839c730c36a5c0cbc046f1e5d6b56/apps/api/plane/db/models/state.py#L14-L20),
[state](https://github.com/makeplane/plane/blob/2f895b82dad839c730c36a5c0cbc046f1e5d6b56/apps/api/plane/db/models/state.py#L79-L89),
[save](https://github.com/makeplane/plane/blob/2f895b82dad839c730c36a5c0cbc046f1e5d6b56/apps/api/plane/db/models/issue.py#L180-L183),
[sync](https://github.com/makeplane/plane/blob/2f895b82dad839c730c36a5c0cbc046f1e5d6b56/apps/api/plane/db/models/issue.py#L240-L255)).
Type is a separate project association/default and serializer field, not the
state: `/tmp/ref-plane/apps/api/plane/db/models/issue_type.py:14-52`;
`/tmp/ref-plane/apps/api/plane/api/serializers/issue.py:66-72,173-180`
([type](https://github.com/makeplane/plane/blob/2f895b82dad839c730c36a5c0cbc046f1e5d6b56/apps/api/plane/db/models/issue_type.py#L14-L52),
[field](https://github.com/makeplane/plane/blob/2f895b82dad839c730c36a5c0cbc046f1e5d6b56/apps/api/plane/api/serializers/issue.py#L66-L72),
[default](https://github.com/makeplane/plane/blob/2f895b82dad839c730c36a5c0cbc046f1e5d6b56/apps/api/plane/api/serializers/issue.py#L173-L180)).

### Accepted, with limits: Org is one hierarchy with optional local finishability

The manual states that TODO items remain integral to the notes tree and that
any headline can become TODO: `/tmp/ref-orgmode/doc/org-manual.org:3944-3950,3963-3967`
([source](https://github.com/bzg/org-mode/blob/7b9d6dbe0be2ea50f272e1ec9b61c46bbd100896/doc/org-manual.org#L3944-L3967)).
`org-todo` supports an empty/unmarked state and removing any TODO keyword, and
the parser/API reads the current heading’s local keyword:
`/tmp/ref-orgmode/doc/org-manual.org:4202-4204`;
`/tmp/ref-orgmode/lisp/org.el:9747-9759,9777-9780,9850-9873,10490-10503`;
`/tmp/ref-orgmode/lisp/org-element.el:1405-1449`
([state API](https://github.com/bzg/org-mode/blob/7b9d6dbe0be2ea50f272e1ec9b61c46bbd100896/lisp/org.el#L9747-L9759),
[state transition](https://github.com/bzg/org-mode/blob/7b9d6dbe0be2ea50f272e1ec9b61c46bbd100896/lisp/org.el#L9850-L9873),
[local query](https://github.com/bzg/org-mode/blob/7b9d6dbe0be2ea50f272e1ec9b61c46bbd100896/lisp/org.el#L10490-L10503),
[parser](https://github.com/bzg/org-mode/blob/7b9d6dbe0be2ea50f272e1ec9b61c46bbd100896/lisp/org-element.el#L1405-L1449)).

Dependency checks are configurable at different scopes: the global
`org-enforce-todo-dependencies` is nil by default and installs an
`org-blocker-hook`; `ORDERED` is a per-entry property for unfinished earlier
sibling blockers, with `NOBLOCKING` as an escape:
`/tmp/ref-orgmode/lisp/org.el:2129-2173,9797-9805,10007-10074`;
`/tmp/ref-orgmode/doc/org-manual.org:4291-4346`
([configuration](https://github.com/bzg/org-mode/blob/7b9d6dbe0be2ea50f272e1ec9b61c46bbd100896/lisp/org.el#L2129-L2173),
[bypass](https://github.com/bzg/org-mode/blob/7b9d6dbe0be2ea50f272e1ec9b61c46bbd100896/lisp/org.el#L9797-L9805),
[blocker](https://github.com/bzg/org-mode/blob/7b9d6dbe0be2ea50f272e1ec9b61c46bbd100896/lisp/org.el#L10007-L10074),
[local ordering](https://github.com/bzg/org-mode/blob/7b9d6dbe0be2ea50f272e1ec9b61c46bbd100896/doc/org-manual.org#L4291-L4346)).
Checkbox blockers are separately gated by `org-enforce-todo-checkbox-dependencies`:
`/tmp/ref-orgmode/lisp/org.el:2175-2191`; `/tmp/ref-orgmode/doc/org-manual.org:4354-4359`
([checkbox configuration](https://github.com/bzg/org-mode/blob/7b9d6dbe0be2ea50f272e1ec9b61c46bbd100896/lisp/org.el#L2175-L2191),
[manual](https://github.com/bzg/org-mode/blob/7b9d6dbe0be2ea50f272e1ec9b61c46bbd100896/doc/org-manual.org#L4354-L4359)).
The hook runs before the local state mutation: `/tmp/ref-orgmode/lisp/org.el:9890-9908`
([hook call](https://github.com/bzg/org-mode/blob/7b9d6dbe0be2ea50f272e1ec9b61c46bbd100896/lisp/org.el#L9890-L9908)).
Separately, parent completion is an opt-in statistics hook: the documented
example changes a parent to `DONE` when all children are done and back to `TODO`
otherwise, and runs only when the headline has a statistics cookie:
`/tmp/ref-orgmode/lisp/org.el:10304-10317`;
`/tmp/ref-orgmode/doc/org-manual.org:4739-4749`
([hook](https://github.com/bzg/org-mode/blob/7b9d6dbe0be2ea50f272e1ec9b61c46bbd100896/lisp/org.el#L10304-L10317),
[example](https://github.com/bzg/org-mode/blob/7b9d6dbe0be2ea50f272e1ec9b61c46bbd100896/doc/org-manual.org#L4739-L4749)).
This proves opt-in local/editor transition checks only; it does not justify
any backend-autonomy inference.

Archiving is a separate mechanism: Org supports moving a subtree or applying
an `ARCHIVE` tag, while `org-archive-mark-done` defaults to nil:
`/tmp/ref-orgmode/doc/org-manual.org:7747-7757,7811-7860`;
`/tmp/ref-orgmode/lisp/org-archive.el:64-72,223-238`
([manual](https://github.com/bzg/org-mode/blob/7b9d6dbe0be2ea50f272e1ec9b61c46bbd100896/doc/org-manual.org#L7747-L7860),
[default](https://github.com/bzg/org-mode/blob/7b9d6dbe0be2ea50f272e1ec9b61c46bbd100896/lisp/org-archive.el#L64-L72),
[command](https://github.com/bzg/org-mode/blob/7b9d6dbe0be2ea50f272e1ec9b61c46bbd100896/lisp/org-archive.el#L223-L238)).
Only the completion/archive separation is retained here; file movement,
storage, filtering, and archive context semantics are not transferred.

## Rejected or unverified claims and exclusions

The richer Plane type-specific-properties/configuration claim is unverified by
the inspected model/serializer; only classification, defaults, activity,
level, and association were established. Broader workflow, task-API,
workflows/Babel, and archive-cascade claims are outside this verification and
remain unverified. No adoption or implementation is proposed.

The clarified root hypothesis is: Conversation/Task is one Thread model with
modes at any depth, not separate tables. Optional tracking can be structurally
equivalent; the genuine question is opt-in UX and semantics. #50 remains
research-only. This pass does not re-audit #13 execution timelines or #49
responsive layout; any later UI delta is limited to semantic labels/actions or
grouping.
