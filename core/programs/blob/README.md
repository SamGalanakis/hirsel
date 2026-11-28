# Store Blob HTTP routes

The `hirsel.ai/blob@1` package exposes the `http` module over Store Format 15
`store.Blobs[K]` collections.

## Uploads

Mount `upload_route(handler, body_max)` in Runner's raw router. This sets
the decoded body limit even when the listener's default differs. Call
`upload` with the responder, collection, key and a scoped writer callback.
The fill loop uses the collection's configured chunk size and makes one
writer call per chunk. A successful upload commits one final attachment.
A read, allocation, quota or writer failure after begin calls abort.

Use `apply` inside the Store transaction for each submitted operation. A
writer must return settled results. Authorize begin and recheck authorization
in the final attach transaction. An `auth.transact` handler can call `apply`
with its scoped proof. Let the upload's trusted cleanup path abort its
own staging ID after an authorization failure or expiry. A failed abort
is reported to the caller. Store aging and sweep handle reclamation of
abandoned data, including cleanup after process loss.

## Downloads

Authorize the key before serving it. Supply `Bound` with the request's
`runner.LiveBinding` when an authenticated download needs expiry and
revocation checks. Pass a reader and snapshot from the same Store. Give
the route a stream allowance and a send endpoint for a bounded read queue.
The listener's owner scope runs `serve_reads` with that queue, its
worker count and the per-worker byte allowance. The worker allowance
covers one Store read and one sliced chunk. Choose stream event space for
one output item and the pull request. Credit bounds the number of queued
output items.

The response retains its snapshot while pulling chunks. HEAD and
conditional refusals release the reader without sending a read job. GET
reuses the standard library's conditional, single-range and multipart
response logic, including its attachment, nosniff and sandbox headers.
