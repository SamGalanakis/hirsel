# Task settlement is action-authoritative

> **Current product clarification (2026-07-23):** Action-authoritative settlement remains current. References to Side Threads below are superseded by Task Margins. An action can also advance an adaptive generated instrument while the Task remains open; settlement intent is explicit rather than inferred from every structured action.

A Task has exactly two settlement states: open and done. An Owner message may carry both an Anchor `ref` and structured Task `mentions`; both preserve orchestration context and are lifecycle-neutral. Replying, discussing a Task in a side thread, or confirming a side-thread conclusion never implies that the Task was handled.

Settlement is action-authoritative: generated controls send `event_action` (`choose` or `submit`) to move a Task to done, and `reopen` moves it back to open. This keeps conversational dives reversible and prevents an exploratory message from silently removing work from the global orchestrator's Task inventory.
