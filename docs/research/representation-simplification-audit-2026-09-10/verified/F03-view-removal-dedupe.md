# F03 — Removed snapshot view suppresses a later recreation

Recommend; high confidence. Owner C14-PROTOCOL-CONNECTION. Worker evidence: ../workers/C14-PROTOCOL-CONNECTION.md C14-01. Independently reopened hello_dedupe.rs in full, ViewManager::show/clear, tools/views.rs wrappers, host protocol broadcast loop and web reducers. Consumer query reproduced71 matching lines.

Reachable sequence: connect with existing view v=A in hello; clear v; show v=A using the supported explicit instance_id. ViewRemoved reaches the client but does not evict the snapshot's cached v=A; the next identical ViewUpsert calls remove(v), sees equal content and suppresses it. Host has v again, client does not until reconnect.

NARROW worker recommendation: preserve the intentional possibility of one-shot hello snapshot deduplication. Repeated identical upserts after the snapshot has drained are not independently a material defect. A permanent last-sent cache is not required. The minimal representation fix is that ViewRemoved must evict that identity from HelloBroadcastDedupe.views before allowing the removal frame. No schema/wire/client changes.

Scope: protocol/hello_dedupe.rs plus focused protocol regression. Test snapshot(v=A) -> remove(v) -> recreate(v=A) is sent; preserve equal snapshot broadcast suppression and changed-upsert delivery. A real ViewManager show/clear/show sequence should exercise the supported same-ID producer. No tests run by audit. Source3ee/treea4aac830 unchanged.
