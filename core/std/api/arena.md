# Arena

[API overview](../API.md). [Operations and contracts](../arena.hirsel).

Explicit region construction has no ambient region or hidden sizing pass.
`batch` runs its producer once in an admitted reservation and publishes with
`finish_trim`. Failure publishes no batch and preserves the admitted region
prefix. `copy_span` and `collect_span` construct spans in the supplied region.
