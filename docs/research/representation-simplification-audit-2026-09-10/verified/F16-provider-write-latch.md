# F16 — failed provider write leaves an acknowledgement latch that closes later drafts

Recommend; high confidence, medium priority. Owner C19, worker C19-02 ../workers/C19-WEB-SETTINGS.md. Independently reopened provider revision effect/write, pending-key begin/timeout/settle, shared protocol-error settlement and edit/add draft owners. Exact latch/revision query repeated:12 matches.

Submitting sets both awaitingFrame=true and a pending key. Rejection or timeout clears pending state but never clears awaitingFrame. An unrelated later providers_changed then closes the still-open editor/add form and destroys the user's ongoing draft as though the old rejected write succeeded. Current host error and roster broadcast paths produce this sequence without malformed input.

Target remove awaitingFrame; use existing pending.any() at roster-revision delivery before settling keys. Both errors/timeouts and successful acknowledgements then consult one lifecycle owner. Keep the revision effect subscribed only to roster revision, so starting/clearing pending work itself cannot close a form. No new map, protocol request IDs or settings-wire redesign for this narrow correction.

Scope ProvidersSection.tsx plus meaningful rejection→continue-editing→unrelated-roster and timeout→roster regressions for edit and Add forms; retain successful pending-write closure. Verify a passive roster refresh does not destroy unsent local input. This does not claim to solve all unrelated-frame correlation while a different local write is still active; that broader behavior is outside the verified stale-latch correction. Audit ran no tests.
