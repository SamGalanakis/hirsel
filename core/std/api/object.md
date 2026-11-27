# S3 object storage

[API overview](../API.md). [Declarations](../object.hirsel).

A `Bucket` names an endpoint, AWS region, bucket name, key prefix and
addressing style. `Path` puts the bucket in the request path. `Virtual`
prepends it to the endpoint's DNS name. Keys are relative to the configured
prefix. Use an HTTPS endpoint and a Net grant covering its DNS name, connect
addresses and TLS verification name. The access key secret stays in a
`Secret` with the `kdf` use. PUTs and upload parts carry CRC32C. The client
signs with `UNSIGNED-PAYLOAD` and leaves TLS GET integrity to the record layer.

`start` takes `cred.Credentials`, an optional credential grant for refresh,
a trust policy, a clock, Random and Tasks capabilities, a lane count and
the admission budget per lane. Pass zero for the default of eight lanes.
Eight lanes reached 96% of the peak in the 512 MiB Tigris sweep.
Each lane owns a connection
and all of its HTTP buffers. The resolver refreshes DNS at 300-second
intervals, favors addresses with throughput at or above the median, and
breaks ties by the fewest open lanes. Failed connects cool down for 100 ms.
A lane closes a connection after four seconds idle and limits reuse to
one second idle for conditional PUT and multipart complete. Dropping the
client cancels its tasks and closes their connections.

`start_simulated` takes an endpoint created by `std.sim.s3.start_service`.
`simulated` reports whether the client was started with that endpoint.
`bucket_config` returns the configured bucket and prefix along with its
endpoint settings. `endpoint_secure` reports whether the configured scheme
is HTTPS.
`set_prefix` points the client's later requests at another key prefix of
the same bucket and returns the previous one. The prefix only names keys:
the credentials, endpoint and lanes stay the client's, and openers made
earlier keep their prefix.

`isolated_request` pre-encodes an object path for the supplied prefix and key. Use it with the client that prepared it. `fetch_isolated` and `store_if_isolated` send each request with its own reply channel, including during cancellation cleanup when the ordinary client receiver may still hold another reply. The supplied deadline covers queue admission and the reply. If it passes after the lane accepted the request, the lane owns the request storage and the prepared request expires. An isolated conditional PUT reports that outcome as uncertain. `ISOLATED_REPLY_BYTES` is the charge for one reply channel.

`split` moves the request state and the worker lifetime out of a client.
`Requests` can move to another task, where `attach(requests, None)` makes it a client again. On the opening task, pass the original Workers to `attach` to restore the split client.
The original task keeps `Workers` until the request task has stopped.
`matches_lanes` checks the effective lane count and per-lane grant. It treats
a requested count of zero as the default.

`Meta` carries the object size, an inline ETag, optional CRC64/NVME and an
optional 16-byte put-id. An ETag holds up to 64 octets and its length. A
longer ETag is malformed. Writes return `Created(meta)` or `Exists` where
the operation uses a no-clobber precondition.

| Operation | Result |
| --- | --- |
| `fetch(key, take into, deadline_ns)` | Appends the object and returns metadata plus the original buffer. A known body that exceeds spare capacity gives `TooLarge` before reading it. |
| `fetch_if_changed(key, unchanged, take into, deadline_ns)` | Uses `If-None-Match`. Returns `None` on HTTP 304, or the new metadata and body on success. |
| `bucket_location(deadline_ns)` | Reads the location constraint of the configured bucket. |
| `fetch_range(key, lo, hi, take into, deadline_ns)` | Appends the requested half-open interval. Metadata includes the full object's size. |
| `pause_retry(attempt, deadline_ns)` | Waits with bounded transport backoff and jitter between attempts of a multi-request operation. Attempt 0 is the first retry. |
| `fetch_parts(key, take buffers, deadline_ns)` | Reads contiguous ranges sized by each buffer’s spare capacity, with a single ETag pinned across the lanes. Appends bytes to the original buffers in order and leaves unused buffers as supplied. |
| `store(key, take body, deadline_ns)` | Overwrites a key and returns metadata and the caller's buffer. |
| `store_no_clobber(key, take body, deadline_ns)` | Conditionally creates the key and returns `Created` or `Exists` with the caller's buffer. |
| `store_if(key, pre, take body, deadline_ns)` | Sends `If-Match` for `Match(etag)` or `If-None-Match: *` for `Absent`. Returns `Applied(meta)` or `Mismatch` with the original body. |
| `store_if_retry(key, pre, take body, deadline_ns)` | Also retries uncertain responses within the same attempt budget. An exhausted budget after possible delivery returns `Uncertain`. Resolve `Mismatch` by reading the object. |
| `store_if_issue(key, pre, take body, deadline_ns)` | Returns `PendingStore` as soon as the lane accepts the retrying PUT. A precondition refusal or closed lane returns the original body and error. |
| `store_if_finish(take pending)` | On the issuing client, waits for that PUT's result and returns the original body. The lane runs the retries even if the caller drops the pending result. |
| `list(prefix, after, mut into, deadline_ns)` | Appends one lexical page to a bounded list. `Page.after` is the last returned key. Pass `key_text(Page.after)` to get the next page if `Page.truncated` is true. |
| `erase(key, deadline_ns)` | Deletes a key. An absent key is success. |
| `upload(key, mode, deadline_ns)` | Begins a multipart upload and assigns the put-id. |
| `parts(mut upload, take bodies, deadline_ns)` | Uploads a batch concurrently, numbers parts in input order across calls, and returns the buffers. If a batch fails, its part numbers can be reused by retrying it. |
| `complete(take upload, deadline_ns)` | Publishes the multipart object. In `NoClobber` mode it sends `If-None-Match: *`. |
| `complete_unresolved(upload, deadline_ns)` | Sends completion and returns any uncertain outcome to the caller. The caller retains the upload handle. |
| `abort(take upload, deadline_ns)` | Stops a multipart upload. An absent upload is success. |
| `pending_uploads(prefix, mut into, deadline_ns)` | Appends handles for up to 16 multipart uploads still pending under the prefix and returns true when more remain. Abort them and list again. |
| `pending_uploads_page(prefix, after, mut into, deadline_ns)` | Lists pending upload handles with their relative object keys. Pass the returned cursor to continue. `None` marks the final page. |

## Request counters

Use `request_monitor(client)` to hold a read handle while the client runs in
another task. `request_counts(monitor, kind)` sums the per-lane totals published
when jobs finish, plus any completed background credential checks. The counters
start at zero for each client and follow it through `split` and `attach`.

`ObjectRequest(op)` includes the startup location GET as `S3GetBucketLocation`.
`ReconcileWrite` counts HEAD probes separately from the job that issued them.
`ValidateCredentials` counts the signed bucket HEAD for a credential check.
Jobs count operations handed to a lane, or credential checks. Attempts count
each exchange, including failures before sending. Retries count additional
attempts for the same job and kind. Starts count requests that may have sent
bytes. Responses count accepted response heads, including error statuses.
Successes count usable exchange results. For a streamed object, that means
the head and handoff succeeded. The counters measure client activity and
cannot establish what the provider received or billed.

## Opening an object for a relay

See [HTTP response outcomes](../../../docs/spec/http.md#http-10-outcomes) for the object
route rules.

`opener(client)` returns an `Opener`, a handle any task can use while the
client lives; `share_opener` copies it, for instance into each connection's
state of an HTTP server. Each call is answered on its own channel, and a
caller that stops waiting leaves the lane serving others.
`opener_deadline_ms(opener, ms)` is the deadline `ms` from now.

`open(opener, key, want, deadline_ns)` sends a signed GET and returns at the
store's head, with the body unread on the connection it hands back:
`Ready(part, connection)` or `Unsatisfiable` for the store's 416. `Want` is
RFC 9110 §14.1.2's byte range: `Whole`, `From(first)`, `Span(first, last)`
with `last` inclusive, or the suffix `Last(n)`; its Range field is signed
with the request. `Part` holds the status, the half-open span
[first, end), the whole object's size and its ETag. A 206 whose
Content-Range is not the span `range_span(want, size)` gives, or a 200 without
Content-Length, is `Fatal(Malformed)`. `stat(opener, key, deadline_ns)`
returns the object's `Meta` from a HEAD. `range_span(want, size)` is the span a
range selects, or `None` when it is unsatisfiable.
[`http_object`](http_object.md) serves an object by range with these.

`tag_value(etag)` is an ETag's field value, quotes included.
`strong_match(etag, value)` is RFC 9110 §8.8.3.2's strong comparison.

A retried multipart initiation can leave a pending upload if the first
reply was lost. It publishes no object. The owner of the prefix can
abort those pending uploads during recovery: `pending_uploads` lists them and
`abort` stops each one.

## Deadlines and retries

Deadlines are absolute monotonic nanoseconds on the Clock the client was
started with. `deadline_after_ms(client, ms)` returns the deadline `ms`
milliseconds from now on that clock. Each operation can make five
attempts before its deadline. A conditional write can issue a HEAD within
an attempt to determine whether it committed. Retries use exponential
backoff and draw jitter from the supplied Random capability.

`Fatal` covers terminal server statuses or malformed protocol data.
`Retriable` reports the cause when the attempt or time budget runs out.
`Uncertain` means a conditional write or completion may have committed.
`TooLarge` covers caller capacity and response bounds. `NotFound` is a
missing object and `Precondition` means a pinned version changed.

| Phase and response | Action |
| --- | --- |
| DNS, connect, TLS or a reset before any request byte committed | Retry. |
| Request bytes committed, then reset, timeout or EOF before a response head | Retry reads and idempotent writes. Resolve conditional PUT and complete by HEAD. |
| HTTP 500, 502 or 504, or XML `InternalError` | Retry or resolve as above. |
| HTTP 429, or HTTP 503 / XML `SlowDown` | Use throttle backoff for idempotent operations. Resolve conditional PUT and complete by HEAD. |
| HTTP 400 `RequestTimeout` | Retry. |
| HTTP 400 `RequestTimeTooSkewed` | Correct the signing time and retry once. |
| HTTP 403 `ExpiredToken` | Refresh and validate new credentials, then retry once. |
| HTTP 304 or 412 on a no-clobber request | Return `Exists`. Use HEAD when an earlier attempt was uncertain. |
| HTTP 409 on a pinned range | Retry. |
| HTTP 404 | Return `NotFound`. Erase and abort succeed. |
| Other 4xx | Return `Fatal`. |
| HTTP 200 with an S3 XML error body, except GET object data | Classify by its error code. |
| GET interrupted after bytes arrive | Preserve the bytes and resume with `Range` and `If-Match` using the original ETag. |

`store_if` retries before request delivery, on HTTP 409 and on HTTP 400
`RequestTimeout`. HTTP 412 returns `Mismatch`. A lost reply after delivery
may have started, HTTP 429 and HTTP 5xx return `Uncertain` for the caller to
resolve. `Match` requires a strong ETag.

Plain PUT, no-clobber PUT and multipart initiation send the operation's 128-bit random put-id
as `x-amz-meta-hirsel-put-id`. When resolving a write, HEAD returns
`Created` for the same put-id and `Exists` for a different or missing
put-id on the identified object. A missing object permits another attempt.

On plain HTTP, a connection reset while sending a conditional PUT's fixed
body leaves the message incomplete. The client retries it as a request that
could not apply. A reset while reading the reply retains uncertainty.

Dynamic credentials refresh 300 seconds before expiry. A replacement must
pass a signed HEAD of the bucket. HTTP 403 leaves the active credentials
in use up to their expiry. Refresh failures wait 100 ms at first and double
the delay up to expiry. Static credentials enter through `cred.from_secret`.

`request_lanes(requests)` returns the active lane count and each lane's byte grant. A caller can size a consumer of a transferred `Requests` endpoint before attaching it.
