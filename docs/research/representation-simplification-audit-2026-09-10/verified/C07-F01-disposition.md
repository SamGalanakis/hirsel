# C07-F01 — asymmetric malformed Related snapshot handling

Coordinator verdict: skip, insufficient materiality. Worker ../workers/C07-RELATED.md. Independently reopened storage list/snapshot, source-guarded related publication, web receive and remove action, native replace_related_items guard. Exact worker query repeated: 51 matches.

Web accepts a fabricated envelope/item source mismatch that native rejects. Current host producers derive both source identities from the same source-filtered database snapshot, and publication holds the history guard. No normal producer, race or one-copy update creates the contradictory frame. The worker explicitly relies on a future conversion bug/malformed server output. That is latent defensive parity, not a demonstrated lost association or scope violation in shipped behavior. The actual item owner is useful in native flat state; do not delete it just because the grouped web cache also supplies an owner.

No recommendation to add generalized protocol validation or a one-off guard purely for parity. Preserve existing history/revision/target authorization and this skip for fresh materiality review; C17 may independently find a real web request race but should not promote the same hypothetical mismatch without new evidence.
