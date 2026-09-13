# Process wakes

Read [the runbook rules](../RULES.md). Run `just product-runbook process-wakes`.
Use a fresh store, unused loopback port, matching production frontend and real
configured provider. Budget: one Owner registration turn plus one automatic
normal delivery turn. Never retry a model call to pass.

The Owner asks for one three-second timer process returning the run's unique
marker. After it fires, judge `01-process-delivered.png`: one muted process
note shows the monospace name, clock, timer interval, completed state and time,
with the exact bare result below, without an Agent avatar or bubble. The
normal turn may remain quiet or respond.

The live message, open_thread message, and receipt JSON must agree on process
identity, typed trigger, completed outcome and string result. SQLite contains
one delivery message and exactly two turns: registration and normal delivery.
There is no triage fork. `03-process-reloaded.png` and captured snapshots must
preserve the same message, turn identities and note. No duplicate is accepted.

Open Processes and judge `02-folded-finished-process.png`: exactly one
`wakeAfterThreeSeconds` row under Finished (1), state done, trigger in 3s,
last-fired time and exact last result. There is no waiting duplicate and no
Cancel or Disable trigger action. Prefire and finished live upserts share one
row ID. The reloaded hello_ok contains exactly that same finished row.

Record the model from hello_ok, evidence path, message/turn IDs and a judged
verdict after viewing all three screenshots. Any missing provider, failed predicate,
missing UI element or cross-surface disagreement is a blocker, not a pass.
