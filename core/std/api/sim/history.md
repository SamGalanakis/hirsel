# std.sim.history

`History` checks the version-stamped observations from one Place. A law replays
its own committed operations through its model and supplies the expected
projection at each version it will read. Pass the observed stamp and bytes
from the client separately. Keep session IDs across reconnects and follower
changes. Use one canonical byte representation per projection.

Capture `request` before issuing the operation. `Fresh` captures the floors
from earlier committed replies, including replies delivered to other sessions.
`AtLeast` uses the requested version. `Follower` uses the declared bound from
catch-up or at-least admission. A typed refusal leaves the session's read
floor unchanged. Use `recovered` after reopening the same Place to compare its
durable position with the versions clients saw before the crash.

Record `committed` from a commit witness and `reply` from the client, using
the complete idempotency key. A second commit for the same key is a violation.
A retry can return the saved result more than once, in a round at or above
the original commit. A retry that reports the
committed result missing supplies `None`.

`check` fails the law with the seed, anomaly class and version stamps. VOPR
keeps the corresponding SEM-8 recording with the failing seed.
