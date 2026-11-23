# S3 object storage

[API overview](../API.md). [Declarations](../object.hirsel).

A `Bucket` names an endpoint, AWS region, bucket name, key prefix and
addressing style. `Path` puts the bucket in the request path. `Virtual`
prepends it to the endpoint's DNS name. Keys are relative to the configured
prefix. Use an HTTPS endpoint and a Net grant covering its DNS name, connect
addresses and TLS verification name. The access key secret stays in a
`Secret` with the `kdf` use. PUTs and upload parts carry CRC32C. The client
signs with `UNSIGNED-PAYLOAD` and leaves TLS GET integrity to the record layer.

`start` takes a SigV4 signer, the public access key ID and session token,
a trust policy, a clock, Random and Tasks capabilities, a lane count and
the admission budget per lane. Each lane owns a connection
and all of its HTTP buffers. The resolver refreshes DNS at 300-second
intervals, favors addresses with throughput at or above the median, and
breaks ties by the fewest open lanes. Failed connects cool down for 100 ms.
A lane closes a connection after four seconds idle and limits reuse to
one second idle for conditional PUT and multipart complete. Dropping the
client cancels its tasks and closes their connections.

`Meta` carries the object size, an inline ETag, optional CRC64/NVME and an
optional 16-byte put-id. An ETag holds up to 64 octets and its length. A
longer ETag is malformed. Writes return `Created(meta)` or `Exists` where
the operation uses a no-clobber precondition.

| Operation | Result |
| --- | --- |
| `fetch(key, take into, deadline_ns)` | Appends the object and returns metadata plus the original buffer. A known body that exceeds spare capacity gives `TooLarge` before reading it. |
| `fetch_range(key, lo, hi, take into, deadline_ns)` | Appends the requested half-open interval. Metadata includes the full object's size. |
| `fetch_parts(key, take buffers, deadline_ns)` | Reads the object as 8 MiB ranges with an ETag pinned across the lanes. The last range can be shorter. Returns the original list and keeps the ranges in separate buffers. Any unused buffers are left as supplied. |
| `store(key, take body, deadline_ns)` | Overwrites a key and returns metadata and the caller's buffer. |
| `store_no_clobber(key, take body, deadline_ns)` | Conditionally creates the key and returns `Created` or `Exists` with the caller's buffer. |
| `list(prefix, after, mut into, deadline_ns)` | Appends one lexical page to a bounded list. `Page.after` is the last returned key. Pass `key_text(Page.after)` to get the next page if `Page.truncated` is true. |
| `erase(key, deadline_ns)` | Deletes a key. An absent key is success. |
| `upload(key, mode, deadline_ns)` | Begins a multipart upload and assigns the put-id. |
| `parts(mut upload, take bodies, deadline_ns)` | Uploads a batch concurrently, numbers parts in input order across calls, and returns the buffers. If a batch fails, its part numbers can be reused by retrying it. |
| `complete(take upload, deadline_ns)` | Publishes the multipart object. In `NoClobber` mode it sends `If-None-Match: *`. |
| `abort(take upload, deadline_ns)` | Stops a multipart upload. An absent upload is success. |

A retried multipart initiation can leave a pending upload if the first
reply was lost. It publishes no object. The owner of the prefix can
abort those pending uploads during recovery.

## Deadlines and retries

Deadlines are absolute monotonic nanoseconds. Each operation can make five
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
| HTTP 403 `ExpiredToken` | Report a fatal credentials error. |
| HTTP 304 or 412 on a no-clobber request | Return `Exists`. Use HEAD when an earlier attempt was uncertain. |
| HTTP 409 on a pinned range | Retry. |
| HTTP 404 | Return `NotFound`. Erase and abort succeed. |
| Other 4xx | Return `Fatal`. |
| HTTP 200 with an S3 XML error body, except GET object data | Classify by its error code. |
| GET interrupted after bytes arrive | Preserve the bytes and resume with `Range` and `If-Match` using the original ETag. |

PUT and multipart initiation send the operation's 128-bit random put-id
as `x-amz-meta-hirsel-put-id`. When resolving a write, HEAD returns
`Created` for the same put-id and `Exists` for a different or missing
put-id on the identified object. A missing object permits another attempt.
