# C09 — Codex malformed-peer proposals

Coordinator verdict: skip both candidates; no present producer witness or material simplification. Worker ../workers/C09-CODEX-DRIVER.md. Independently reopened codex_io.rs28–60, CodexSession request/PendingRequest cleanup, receive correlation, startup identity extraction, completion and command builders.

F-01 relies on malformed peer frames with null method, string response IDs where Hirsel sent numeric IDs, or a method/result combination. Existing control timeout explicitly fails the session, drains waiters and kills the process group; this is bounded diagnostic latency, not a leaked or silently successful request. Valid server requests intentionally have a separate ID namespace. No current native peer fixture or actual production producer sends the fabricated forms. A new frame enum and request-ID newtype would strengthen malformed-peer validation but does not remove an observed protocol failure or meaningful existing complexity. Reject high priority and broad strict-parser cutover.

F-02 uses empty provider identities, absent from every supplied peer fixture and normal producers. Nonempty opaque identities are the established upstream contract; the current code has no normal transformation making them empty. Distinct Thread/Turn newtypes plus constructors/wire rewrites address hypothetical invalid input, not a demonstrated source bug. Keep this as latent hardening with no schema change or new ticket. Existing #2–6 behavior was independently acknowledged by the worker and is not reopened.

Coordinator reran the exact recorded consumer queries without shell evaluation: matching-line counts [19, 38].
