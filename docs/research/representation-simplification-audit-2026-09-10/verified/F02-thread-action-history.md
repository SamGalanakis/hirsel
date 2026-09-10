# F02 — Generic Thread actions omit the history component of identity

Recommend; high confidence. Authoritative finding owner C21-CLIENT-CORE; cross-layer consumers C14/C15/C22/C01. Full worker evidence: ../workers/C21-CLIENT-CORE.md, F1. Fixed source3ee0621/treea4aac830.

Independently verified by reopening core client.rs:317–329; proto/client.rs:95–101; protocol.rs:155–170 and456–464; thread_commands.rs:115–199; web threads/store.ts:118–121 and actions.ts:12–24; Android Connection.kt:85–88 and ChatScreen.kt lifecycle actions; FFI thread_action adapter. Exact worker consumer grep rerun:39 matches.

The command accepts numeric thread_id but no captured history. After history replacement reuses an ID, a delayed prior-history action can archive/settle/pin/snooze the new Thread. The host handler reads current history itself, so it cannot distinguish the intended prior history. The authenticated connection loop forwards frames without an epoch check. Clearing queued frames when clients observe hello only covers already-buffered local work; it cannot guard old callbacks or an already-sent frame arriving after a host reset. This violates PRODUCT/ADR0016 history-scoped identity.

Target: require a captured history+Thread identity for generic ThreadAction across core/wire/web/FFI/Android, reject stale history locally and atomically with the host mutation, and remove history-less accepting overloads. Keep generated-action revision checks. Existing set_showcase history in its data should become the shared addressing contract, not a separate per-action scheme; icon revisions alone cannot distinguish reused IDs across histories. No database schema migration required.

Narrow affected interface set and proposed validation are in worker F1. Add old-history action/reused-ID regressions through actual native/wire/host paths and a race where the host resets after the client's local validation. Prove no new-history state mutates. Preserve valid current-history actions and existing queue-reset tests.

C14 completed its adjacent command review. Root independently accepted the four-command scope below under GitHub #19 and owns the shared wire/FFI/native implementation; F05 action-result correlation follows in that same lane. No audit source edits or tests.

## Independently verified shared mutation-addressing scope

The same missing captured history is present at other production mutation boundaries; use one canonical fix instead of repeated wire changes:

| Command | Current wire/host behavior | Concrete stale-ID consequence |
|---|---|---|
| ThreadAction | proto/client.rs95–101; protocol.rs456–464; thread_commands.rs122 fetches current history | Old action settles/archives/pins a reused Thread |
| SendThreadMessage | proto/client.rs83–94 has no history; protocol forwards to thread_commands.rs28–53; line40 fetches current history | Already-sent old message is accepted into a new-history reused Thread and starts work |
| CancelTurn | proto/client.rs104–106 has only thread_id; thread_lanes.rs306–317 reads current epoch then writes cancellation | Old stop request cancels new-history work under reused Thread ID |
| CreateThread (with parent) | proto/client.rs CreateThread has parent_thread_id but no history; protocol.rs351–366 calls Storage::create_thread directly | Old child-creation request attaches work to a new-history reused parent |

I reopened the protocol connection loop at155–170: it passes authenticated frames directly with no connection-history check. A local client guard cannot protect a frame already on the wire when the host resets. The existing history-aware storage operations used by Related and accepted-message recording provide the pattern, but expected history must come from the caller and be checked inside each actual transaction/admission lock, not fetched from current storage at receipt.

Keep settings and authentication independent of history. Read-only ID commands may warrant the same address contract during the final boundary review; this addendum promotes only the four verified mutation paths. Root owns the shared API decision. The report's authoritative cross-layer issue remains one F02, not multiple findings/tickets.
