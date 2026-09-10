# F04 — Lifetime authentication latch masks rejected reconnect credentials

Recommend; high confidence. Owner C14-PROTOCOL-CONNECTION. Worker evidence: ../workers/C14-PROTOCOL-CONNECTION.md C14-02. Independently reopened ws/client.ts current authenticated/everAuthed fields, socket lifecycle, hello handling and error routing; host run_protocol authentication rejection and App onAuthReject consumer. Exact auth consumer query reproduced17 matches.

After one successful socket, everAuthed stays true forever. A later socket awaiting hello_ok may receive a plain uncorrelated auth error (for example after a host token rotation). The browser classifies it as an operational error because everAuthed is true, leaves the rejected token stored and repeatedly reconnects. The current authenticated flag already captures the required per-socket phase and is reset on new socket/close.

Target: remove everAuthed and route pre-hello uncorrelated errors using the current socket authentication phase. Preserve post-hello operational errors, correlated request errors and ordinary close-only reconnects. Existing handleAuthReject already stops reconnection, clears token and returns App to its authentication gate. No server/wire/schema change or new state abstraction needed.

Scope: ws/client.ts plus its focused test file. Regression: hello succeeds on socket1, drop, socket2 opens and receives uncorrelated rejection before hello_ok; assert one auth callback with reason, token cleared and no socket3. Preserve first-socket auth rejection, authenticated operational error and close-only reconnect fixtures. No tests/live auth read by audit; source3ee/treea4aac830 unchanged.
