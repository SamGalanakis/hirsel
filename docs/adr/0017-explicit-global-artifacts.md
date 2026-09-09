# 0017: Explicit global artifacts with conversation references

Status: Accepted (2026-09-09)

The Owner wants reusable results alongside the conversation, created deliberately by the Agent. Executor's explicit create/edit/list/show workflow is the reference; its React runtime and backend tool bridge are excluded.

An Artifact has a stable global numeric ID, title, format, MIME type, optional filename and current UTF-8 content. It has no Thread owner and no versions. Message references provide many-to-many Thread association and backlinks. Editing overwrites current content; earlier references open that current content. Durable mutation receipts contain input hashes, not historical copies of source.

`artifacts.create`, `artifacts.edit` and `artifacts.show` atomically commit the artifact mutation (if any), an Agent message and the artifact reference. The actual durable Lash tool execution identity scopes replay receipts. Replaying a publication returns the existing card and the current content; it never duplicates cards or rewinds later edits. Edits use unique exact-match replacements and check the read content under the write transaction to avoid losing a concurrent edit.

Solid 2 JSX and self-contained HTML render in an isolated client preview with local state only. UTF-8 files can be previewed and downloaded. Content is limited to 1 MiB. The host stores content as inert text; it never executes JavaScript. There is no artifact backend bridge, tool invocation, network permission, filesystem read, implicit attachment import, or automatic conversion from runtime output.

The resting surface is Conversation. Per-Thread Artifacts lists references; the global inventory shows results across Threads. Expansion does not change conversation addressing. Activity remains execution evidence. Existing Thread-generated instruments, including their action revision guards, are unaffected.
