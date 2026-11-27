# page_inline

The page editor uses `leaf_fit` to check an inline row against a canonical leaf and calculate its slot, encoded size and row charge. A same-width replacement records the value offset. Other changes record the space that the caller must reserve before applying the edit.

`write` applies a fit to the original leaf bytes with the same key and value. `canonical_space` shifts the entries and slot directory in a reserved buffer and returns the new entry offset. The caller supplies the checked slot, old entry size, new entry size and final image size.
