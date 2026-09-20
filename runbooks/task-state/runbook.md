# Task headlines

Follow [`../RULES.md`](../RULES.md). Run this real-provider check only with explicit approval and a fresh store.

Create a Task, have its worker set a unique headline through `threads.state`, and verify normalization, the 12-word bound, `previous_headline`, and one revision increment. A parent with children must show the deterministic count/status rollup rather than child prose and must not be completed.

Verify the Task view orders headline/status, instrument or showcase, children/headlines, then the existing timeline. In its Space board verify Needs you, Changed since you looked (`previous → current`), and the remaining rows; marking displayed revisions seen must not change `read`.

From a different top-level Space, change the Task once and verify one coalesced `Changed by #<id>: <what>` activity appears on the affected Space chat with no automatic turn or context injection. Retain sanitized screenshots, authenticated frames, `open_thread` snapshots and the relevant `threads` and `thread_turns.last_event_at` rows.
