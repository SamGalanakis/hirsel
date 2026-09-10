# C23 plugin proposals — defer

Coordinator independently reopened loader.ts, App connected effect, Settings toggle, registry teardown, generated registration, Plugin::id contract and ADR 0014. Re-ran the worker loader consumer query: 30 matching lines. Both mechanisms are real framework limitations, but the fixed snapshot contains no installed plugin implementation or UI (plugins/.gitkeep only).

C23-01 would retain a mounted plugin after disable and permanently latch a failed initial roster fetch once a plugin UI is installed. That requires adding an extension absent from this audited product. Defer a desired-roster reconciler/generation mechanism until a concrete plugin integration exercises this supported extension boundary; do not label current user-visible failure high priority. Existing cleanup primitives should be used then. The worker target sentence about retaining a latch after an unsuccessful fetch is mistaken: failed attempts must be retryable.

C23-02 folder versus implementation ID mismatch requires a newly authored plugin violating the documented equality contract. No current generated registration or shipped implementation produces it. A generated-ID/API cutover is not justified by this latent mismatch. Revisit a narrow boot equality check with the first actual integration, rather than adding another ID authority now.

Both remain explicit coverage, not accepted findings. No source edits/tests were performed.
