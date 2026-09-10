# F14 — reconnect replaces view recency with lexical ID order

Recommend; high confidence, medium priority. Owner C18, worker C18-02 ../workers/C18-WEB-VIEWS.md. Independently reopened ViewManager active map/show/update/snapshot, host hello snapshot call, web upsert reducer and Canvas ordering selector. Worker exact order query repeated below.

Show custom IDs z then a in one Thread: live reducer appends and Canvas reverses, showing a then z. Reconnect receives BTreeMap values a then z, reversing Canvas to z then a. Same-ID updates also move the browser item to latest while host key order stays fixed. Generated UUIDs make lexical order unrelated to recency even without custom IDs. The map key order and incremental array order are conflicting authorities.

Narrower target than worker's wire ordinal: use one host ordered collection with explicitly chosen upsert/insertion semantics, and emit its snapshot order. Preserve current visible latest-upsert behavior by moving an existing entry to the end on show/update, matching the web reducer; clear removes it and recreate appends anew. An ordered map provides ID lookup and order together. Do not add a persisted/serialized sequence counter just to reconstruct order when ordered snapshots plus the existing ordered event stream suffice.

Scope ViewManager collection/mutations and matching order tests; keep wire shape unchanged for the narrow target. Preserve publication under the map lock, Thread ownership and F03 clear/recreate dedupe. Validate live and reconnect order for z/a, same-ID updates, clear/recreate and multiple Threads. Audit ran no tests. Root settled latest-upsert order; recommending a new ordinal was rejected as unnecessary state/amplification.

Exact worker consumer query rerun: 42 matching lines.

Worker reported43 order matches; independent exact query returns42. This count correction does not affect the reopened map/reducer/order witness.

Independent materiality qualification: an insertion-ordered map retains efficient ID lookup, but preserving order on removal/reposition can cost O(n) rather than the prior logarithmic operation. Small interactive view sets make this tradeoff credible. The correction is semantic consistency, not a performance claim.
