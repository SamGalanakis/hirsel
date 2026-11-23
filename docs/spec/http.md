# HTTP

This specification covers std `http`, the runner's HTTP bindings and keyed operations. It is a design: nothing in it is built yet, and each rule's slice in §7 says what builds it.

- **Part A** is §0-§3 and §5-§8: std `http`, streams, runner bindings, outside inputs and cutover.
- **Part B** is §4 and the slices H9 and H12: keyed operations. §4 and every rule, row, law and decision marked "Part B" are unbuilt and may still change, and nothing in Part A depends on them.

**Settled ground it builds on:**
- WIRE-SPEC W-D1..12, including §2.1's `upgrade_ms = 5000`, `hello_ms = 10000`, `pending_max = 1024` and its release points;
- [net.md](net.md) NET-1, NET-6, NET-7 and NET-11;
- RUNNER-SPEC v2.12.1, including PubIn and the session task group (§3), and v2.13's `Drained` wake;
- ownership arrangement (a): the reader allocates each decoded frame;
- MEM-19..30, CONC-3..13, CAP-3, CAP-8 and CAP-9.

**Repository citations** are to commit fbdb7d69. RUNNER-SPEC (core/programs/store/RUNNER-SPEC.md) is cited by rule (V1-V5, H1-H4, §3).

**Layers.**
- **Layer 1, std `http` (§1-§2):** a library over `Net`. It needs no runner.
- **Layer 2, runner bindings (§3):** library values over Layer 1, plus the runner's HTTP relay.
- **Keyed operations (§4, Part B):** a store and runner extension. Its slices are separate, and no Layer 1 or Layer 2 slice waits on them.

**Rule numbers.** HTTP-n are std rules, B-n binding rules, K-n keyed-operation rules, and L-n acceptance laws.

## 0. Prior art

There is one line per mechanism, saying what Hirsel does with it and why.

### Goose and goose-http

**Goose (the language)**
- **ADOPTED.** Networking is a library over explicit costs, not a language form. Goose's stdlib excludes networking (goose docs/stdlib.md:1253) and asks for explicit allocation and copy costs (goose docs/design/stdlib_design.md:86).
- **NOT TAKEN.** Its coroutine-owned protocols are unimplemented design (goose docs/design/coroutines.md:52). Hirsel already has tasks, channels and NET, and builds on them.

**goose-http: parsing and the codec**
- **ADOPTED.** A bytes-only codec that makes no system calls (message.goose:1).
  - Hirsel's std `http1` already works this way. std `http` keeps the parser and framing pure, and keeps I/O in the connection task.
- **ADAPTED.** One head pass interprets Content-Length, Transfer-Encoding, Connection, Host and Expect while it scans (message.goose:70, :141).
  - Hirsel keeps httparse-strict field syntax in `http1` and runs framing (HTTP-4) once over the parsed `Head`. The fields are not scanned again.
- **REPLACED.** Each call parses again from zero (message.goose:70), and `field_at` scans the colon and line end again (message.goose:222).
  - Hirsel's parse is resumable across fragments, with a retained cursor (HTTP-3). Fields are found once.

**goose-http: views and output**
- **ADOPTED.** Request views live only for the handler (message.goose:222), and retained bytes are copied (epoll.goose:357).
  - These are HTTP-6's rules. The difference is that Hirsel's checker enforces them through an anchored `Request` (OWN-19, OWN-20); goose-http relies on a comment.
- **ADOPTED.** Small replies use static prefixes and direct appends (message.goose:817, :872, :1068).
  - This is HTTP-9's response builder.
- **REPLACED.** The write buffer "grows to hold any response whole" (http.goose:109).
  - Hirsel's output buffer is a fixed charged size. A larger body is written from the handler's own bytes through the committed-prefix cursor, so it never grows.

**goose-http: deadlines and connection limits**
- **REPLACED.** goose-http's `body_ms` runs between body pieces and `write_ms` runs "once the client stops taking it" (http.goose:110-119). Both deadlines reset on progress, so a client that drips one byte per interval holds a connection open forever.
  - In Hirsel every phase and every output unit has one absolute deadline, set before it starts (HTTP-5).
- **ADOPTED.** Bounded drain: unread body bytes up to `max_drain`, then close (http.goose:101).
  - This is HTTP-7.
- **ADAPTED.** `max_conns` is per worker (http.goose:88), and `max_queued` stops reading while output is queued (http.goose:163).
  - Hirsel counts every socket for its whole life in one `sockets_max` per `serve` (HTTP-1), and a connection never reads the next request while its handler is active (HTTP-8).

**goose-http: routing, streaming and transport**
- **ADOPTED.** 405 with `Allow` for a path that matches other methods (router.goose:136).
  - This is HTTP-11.
- **ADOPTED.** Response, stream and upgrade are outcomes of an ordinary handler (message.goose:898; docs/api.md:325).
  - These are HTTP-10's three outcomes.
- **ADAPTED.** io_uring (http.goose:126-160), through the pure-Rust `io-uring` crate instead of liburing, so it needs no C headers or library. Provided buffer rings are not taken, and neither are HTTP/2 and HTTP/3 in rung 1 (http.goose:41).
  - NET's transport has two reactors behind one boundary: io_uring when the running kernel allows it, epoll where it does not (an older kernel, a container that refuses `io_uring_setup`).
  - The runtime chooses once, at its first socket poller. An unset `HIRSEL_NET_REACTOR`, or `auto`, takes io_uring when the kernel allows it and its release is at least 6.13, `HIRSEL_NET_REACTOR=epoll` forces epoll and `HIRSEL_NET_REACTOR=io_uring` forces io_uring (refused if the kernel does not allow it). Any other value is refused. Through 6.12.y, killing a process with multishot polls armed can oops the kernel in `io_put_task_remote` (seen on 6.8); 6.13 removed that path.
  - The choice changes no program-visible result and is not a replay input, so a run recorded under one reactor replays under the other.
  - Under io_uring, TLS ciphertext is sealed into a runtime-owned transmit slot and sent zero-copy (`SEND_ZC`); the slot is reused only after the kernel's last notification for it. `HIRSEL_NET_SEND_ZC` chooses where: unset or `auto` sends zero-copy to every TCP peer that is not loopback, `always` to every TCP peer, `never` to none; any other value is refused. Loopback is skipped because the kernel copies every loopback send anyway (64 of 64 notifications reported a copy) and the extra notification made TLS PUT 17% slower in 60 interleaved pairs (2.86 against 3.37 GB/s). Plaintext bodies are sent from the caller's bytes as before, never zero-copy: an aborted send delivers at once, so the kernel may not keep reading bytes the caller owns. Like the reactor, this choice changes no program-visible result and is not a replay input.

### Others

**Go net/http**
- **ADOPTED.** A connection is refused for reuse after an output error, a short Content-Length, an unread body or a body limit hit (server.go:1830).
  - This is HTTP-7.
- **ADOPTED.** The handler is called directly from the connection loop, one request at a time (server.go:2228).
  - This is HTTP-8.
- **ADOPTED.** "More specific wins", and two patterns that overlap with neither more specific are refused (pattern.go:216-231).
  - This is HTTP-11's conflict check.
- **NOT TAKEN.** Go keeps repeated identical Content-Length fields (transfer.go:653, :1162). Hirsel refuses them, so that each request has one lexical form (HTTP-4).
- **NOT TAKEN.** A Go handler (`Handler`, server.go:89) receives a `*Request` whose context carries no entry attenuation, so what it may call is whatever the program closes over.
  - Runner-managed handlers get a sealed context instead (B-1).

**hyper**
- **ADOPTED.** At most 16 ready iterations, then a flush before yielding (proto/h1/dispatch.rs:185, :226).
  - This is HTTP-9's bounded ready batch. In Hirsel the yield is `task.check_cancel()`, a scheduling point (CONC-10).

**Bandit and Cowboy**
- **ADOPTED.** The pipeline calls the handler, then commits a response or hands over an upgrade (bandit/pipeline.ex:35).
- **ADOPTED.** SSE is encoded into an ordinary stream body (cowboy_req.erl:995).

**WHATWG HTML, server-sent events** (HTML Living Standard, sections `#event-stream-interpretation` and `#sse-processing-model`)
- **ADOPTED.** Line splitting on CR, LF and CRLF; `data` lines joined; an event without its terminating blank line discarded at end of stream; `id` with NUL ignored (section "Interpreting an event stream").
  - The encoder (HTTP-13) emits only complete events, and the laws' parser is a transcription of that section.
- **NOT TAKEN.** `Last-Event-ID` resume (the reconnect request sets it, :135840).
  - A version alone cannot resume W-D3's state, so a reconnect gets a fresh subscription.

**google.api.http, Effect HttpApi and Smithy**
- **ADOPTED.** Path, query and body mapping are declared beside the operation and drive the router, codecs, client and OpenAPI (google/api/http.proto:54; HttpApiBuilder.ts:945; OpenApi.ts:395; Smithy HttpTrait.java:35).
  - `HttpBinding` is that declaration, written as library data (B-3).
- **REPLACED.** These tools allow lossy or ambiguous JSON forms, such as protobuf's int64 accepted as a number or a string.
  - Each Hirsel shape has exactly one accepted spelling (B-4), because the spelling feeds `request_digest`.

**Phoenix digester and Go embed**
- **ADOPTED.** Fingerprinted immutable asset names, and bytes built into the image (digester.ex:20).
  - This is HTTP-14's manifest.
- **REPLACED.** Validators computed at run time.
  - Here each representation's digest is computed at build time.

**Convex**
- **NOT TAKEN.** An unbounded channel feeds the streaming HTTP action (local_backend/src/http_actions.rs:218).
  - It is the counterexample for §2's credited producer.
- **NOT TAKEN.** `None` as the mutation identity at HTTP ingress (mod.rs:2249).
  - In Hirsel every durable binding has an identity (B-6, §4).

**IETF Idempotency-Key draft** (draft-ietf-httpapi-idempotency-key-header; cited as a document, no line numbers)
- **ADOPTED.** Statuses: 400 for a missing key, 409 for a request still in progress, 422 for a key reused with another payload.
- **ADAPTED.** Under a Stream binding the key's value is the (stream, position) pair, not an opaque string (B-6).

Keyed-operation prior art (Stripe, Restate, Temporal, AWS Lambda Powertools, Standard Webhooks, GitHub) is in §4.0.

## 1. std `http` (Layer 1)

Module `std.http` of package `std`. It replaces every other HTTP/1.1 server path (§6). `std.http1` stays as the head parser it uses, and gains resumption (HTTP-3).

### HTTP-1: The server, its task group and its limits

`http.serve(take ls: List[Listener], c: Clock, cfg: Config, router: Router[S], state: S) -> Result[(), ServeError] waits`. The `Listener`s come from `Net`; `S: shareable + send` is the handlers' shared state. One call serves all of a process's listeners. Hirsel's runner makes exactly one call, so its caps are process-wide; a program that calls `serve` twice gets two independent sets of caps.

**The task group.** `serve` follows RUNNER-SPEC §3's session pattern.
- **Supervisor**: the `serve` task itself. It waits only on its inbox (capacity `inbox_cap`). Its events are `Accepted(socket, peer)` from the acceptors, `Promote` requests from connections (HTTP-10), `ReadSlot` and `ReadDone` from connections serving a one-shot read (B-11), `Established` from upgraded protocols, and `Ended(slot)` from every task it spawned.
- **Acceptor** children, one per listener. Each calls `accept` and sends `Accepted` to the inbox. When the inbox is full it waits, so it stops accepting and the kernel backlog holds the rest. The backlog therefore takes part in overload; among accepted sockets, fairness is accept order.
- **Socket tasks**, all spawned by the supervisor, because a child's limit comes from its spawner and no task outlives its spawner (CONC-3, CONC-8): connections (reservation C), stream groups (S, §2), upgraded protocol tasks (U, HTTP-10) and refusal tasks (R).
- **Ending.** Cancelling `serve` cancels every child, then joins it (CONC-8).
- **Stopping (v2.15, RUNNER-SPEC §12.1).** A `Stop(D)` event on the supervisor's inbox, with D an absolute deadline on `serve`'s `Clock`, stops the server in order. The supervisor cancels and joins every acceptor, which drops the listeners, so the host refuses new connections. It admits nothing more: a `Promote` is answered as a refusal, and an `Established` protocol task is sent the same stop. A connection closes after its current exchange, as if the request had said `close` (HTTP-7), and a connection between requests closes at once. Every unit deadline of a running exchange becomes min(its own, D). A one-shot read waiting for its initial result (B-11) is such an exchange. A stream group (§2) ends its stream at the stop: its writer finishes the event it is writing and closes, as at `lease_end`, and the client reconnects elsewhere. So a live stream never holds `serve` until D. When the supervisor has no children, `serve` returns `Ok(())`. At D the caller cancels `serve` (Ending). This is not HTTP-7's drain: HTTP-7 discards an unread body to keep one connection open, and the stop never keeps one open. Its laws are the HTTP halves of RUNNER-SPEC O28 and O34, built in slice O3.
- **Stop commits.** `serve_with_stop` takes a receiver for one D. Its watcher sends `Stop(D)` and waits for D. A connection commits active after the first byte of the head arrives and commits idle before it waits for another request. These transitions and Stop use the same supervisor inbox. Stop cancels the connections that are idle at that commit. The supervisor publishes D for active exchanges and wakes stream owners. An already-issued wait whose original deadline is later is interrupted by cancellation at D.

`Config` is a plain record. All of its fields are required, and the runner's generator fills them in.

| Field | Meaning | Value set by Hirsel's runner (§3) |
|---|---|---|
| `sockets_max` | Admitted sockets this `serve` owns, counted from the supervisor's admission at `Accepted` until the socket's last task ends: connection, stream and upgraded alike. A refused socket is never admitted, so it is not counted here; refusal tasks are their own population, at most `refuse_max` | 4096 |
| `http_max` | Connection tasks, each holding C for its whole life, first request and keep-alive alike | 1024 |
| `upgrades_max` | Upgraded protocol tasks | 3072 |
| `conns_max` | Sockets in their first phase: from accept until the first response completes, or for an upgrade until the protocol reports `Established` (WIRE `pending_max`, released at Hello accepted) | 1024 |
| `ip_sockets_max` | Sockets per source IP (W-D5 source key) | 32 |
| `refuse_max`, `refuse_ms` | Pre-admitted refusal tasks, and each one's total deadline | 16, 1000 |
| `inbox_cap` | The supervisor's inbox | 64 |
| `head_max`, `fields_max` | One head, and its fields (`http1`'s fixed bounds) | 8192 B, 64 |
| `pipeline_max` | Unread bytes kept after the current request | 8192 B |
| `body_inline` | Largest body held in the read buffer | 16384 B |
| `body_max` | Largest body for any route; a route may set less | 1 MiB |
| `out_cap` | Writer buffer | 8192 B |
| `handler_bytes` | Each request's `bound bytes` | 256 KiB |
| `first_head_ms` | Accept to complete first head (WIRE `upgrade_ms`, unchanged) | 5000 |
| `idle_ms`, `head_ms`, `body_ms`, `handler_ms`, `write_ms`, `drain_ms`, `linger_ms` | Absolute deadlines (HTTP-5) | 60000, 10000, 30000, 30000, 30000, 2000, 2000 |
| `drain_max` | Unread body bytes the connection discards to stay open | 65536 B |
| `streams_max` | Live stream groups (§2) | 256 |
| `state_max` | Largest encoded producer state a `Stream` may carry (§2) | 16384 B |
| `reads_max` | One-shot reads waiting for their initial result (B-11) | 64 |
| `heartbeat_ms`, `lease_ms` | Stream heartbeat period and lifetime (§2) | 15000, 300000 |

**Reservations.** Each is a logical charge under MEM-24 (`charge-v1`), not RSS: host memory behind provider operations, the call stack and kernel socket buffers are outside it (MEM-19). T(r) = 256 + r + 32 is a task record whose result is r bytes, without its `n` (MEM-24); ch(cap, item) = cap × item + 64 is a channel. h is the size of a `ReadHalf` or `WriteHalf` value.
- **Supervisor**, admitted at `serve`: its record, its inbox ch(`inbox_cap`, event), the acceptors (T(0) + 64 each), the router and `Config`, and `refuse_max` × R, where **R** = T(0) + 64 (the static response) + 8192 (`net.finish`'s scratch, net.hirsel:12).
- **C** (a connection) = T(0) + H + 8192 + X + `handler_bytes` + J + ch(1, Promote reply), where
  - H = `head_max + pipeline_max + body_inline` + `out_cap` (the read buffer and the writer);
  - 8192 is the one scratch, used for body reads, then drain, then `net.finish`'s own 8192 after the scratch is dropped, so the peak is one 8192;
  - X is the fixed `Head` scratch (`Inline[Header, 64]`) and cursor;
  - J = 1024, the JSON transcoder's depth stack (B-5).
- **S** (a stream group) is §2's equation. **U** (an upgraded socket) is T(0) plus the protocol's declared `bytes n` (HTTP-10).
- **Hand-over reserves.** A connection that hands its socket on ends while blocks it paid for are still alive. MEM-20 keeps them on the connection's record, which joins the supervisor's when the connection ends, so the supervisor's limit pays them until they are freed. So nothing the handler allocated crosses a hand-over except **one encoded hand-over block** per transfer, built by std in the handler's scope; the recipient rebuilds what it needs from it under its own reservation and then drops it (HTTP-10):
  - a stream's block W_s ≤ `out_cap` + `state_max`: its encoded response head (status and fields, the bytes the writer will send) and its encoded P; freed when the writer has copied the head into its own buffer and the producer has decoded P (§2);
  - an upgrade's blocks W_u ≤ B_r + `out_cap` + `state_max`: its moved read buffer (B_r = `head_max + pipeline_max + body_inline`, freed when the protocol drops it, row 15), its encoded 101 head (freed when `upgrade.start` has written it) and its encoded argument (freed when the protocol task has decoded it into its own `bytes n`).

**Populations.** Each kind of socket task has its own cap, checked by the supervisor where that population grows:

| Population | Cap | Checked |
|---|---|---|
| Accepted, not yet received by the supervisor | `inbox_cap` + one per listener (a blocked acceptor holds one) | by construction: an acceptor waits on the full inbox |
| Connection tasks | `http_max` | at `Accepted` |
| First-phase sockets (WIRE `pending_max`) | `conns_max` | at `Accepted`; released at the first response's end, at the hand-over to a stream group, or at `Established` |
| Stream groups | `streams_max` | at `Promote(Stream)` |
| Upgraded protocol tasks | `upgrades_max` | at `Promote(Upgrade)` |
| Refusal tasks | `refuse_max` | at refusal |
| Admitted sockets (connection, stream, upgraded) | `sockets_max` | at `Accepted` |

A socket being handed over is briefly in two populations: its connection task lives until the `Promote` reply, beside the group or protocol task the supervisor just spawned. The bound below sums each population at its own cap, so the overlap is inside it.

- **The bound.** A live `serve` holds at most `fixed + http_max × C + streams_max × (S + W_s) + upgrades_max × (U_max + W_u)`, all admitted at spawn, where U_max is the largest `bytes n` any `Upgrade` route declares. No admission happens after a response head is committed.
- **Sockets not yet received.** An accepted socket in the inbox, or held by a blocked acceptor, costs one inbox item (inside `fixed`) and a kernel descriptor, which is outside logical charges (MEM-19). Refused sockets are at most `refuse_max` (their tasks) and are paid by `refuse_max` × R inside `fixed`; a refused socket with no free slot is closed at once. So the process holds at most `sockets_max + refuse_max + inbox_cap + listeners` server sockets.

**Accepting.** On `Accepted`, the supervisor takes the source key (W-D5): the TCP peer at accept. For a peer in the trusted `ingress` prefixes, the sanitized `X-Forwarded-For` source is known only when the first head completes, so the connection reports it to the supervisor, which re-checks `ip_sockets_max` against it and moves the count. The supervisor refuses when `sockets_max`, `http_max`, `conns_max` or `ip_sockets_max` would be passed, or when spawning C with `task.try_spawn` returns `Capacity`.

**Refusing.** A refused socket goes to a refusal task if one of the `refuse_max` slots is free. It writes a static `503` with `Connection: close` and `Retry-After: 1`, then calls `net.finish`, all under one absolute deadline, accept + `refuse_ms`. With no free slot, the supervisor drops both halves at once: a mechanical close, with no response. Neither path waits on the client inside the supervisor.

### HTTP-2: The connection task

Each connection has exactly one task. It holds:
- the `ReadHalf` and `WriteHalf` (until HTTP-10 hands them on);
- the read buffer, the `Head` scratch, the scratch and the writer;
- the parse cursor.

There is one parser (`http1` plus HTTP-4), one framing implementation and one writer per socket. No other code on the server side reads a request head.

The loop:
1. Wait for the head (HTTP-3) under the first-head, idle or head deadline (HTTP-5).
2. Frame it (HTTP-4) and route it (HTTP-11).
3. Run the handler inside `bound bytes handler_bytes` (MEM-25), with the anchored `Request` and the request's scratch region (HTTP-6). The body is read lazily through `Body`.
4. Carry out the outcome (HTTP-10).
5. Settle the body (HTTP-7), then keep the connection alive or close it (HTTP-7).

### HTTP-3: Head parsing, once and resumable

- `http1.request` gains a resumable form: `http1.resume(b, mut h, mut cur: Cursor) -> Progress`.
  - It keeps the scan state (line start, field count, offset of the last byte examined) across fragments, so each byte of a head is examined once. Law L1 counts examined bytes.
  - Its results are those of the restart form on the same bytes (law L1).
  - The restart form stays in place for clients and tests.
- After `Complete`, ingress, framing, routing and upgrade all read the same `Head`. Nothing parses that head again (§6 deletes the paths that did).
- `TooLarge` answers 431, and `Bad` answers 400, each followed by close.
- A request line longer than `head_max` is also `TooLarge`, answered 414 when the line alone overflows.

### HTTP-4: Framing (RFC 9112 §6, §7)

Framing runs once per request over the parsed `Head`. Each failure below closes the connection after a static response.

**Body framing**
1. `Transfer-Encoding` and `Content-Length` both present gives 400. The request is not interpreted as chunked.
2. More than one `Content-Length` field, or one value that is a list (even of equal values), gives 400.
   - The value is `1*DIGIT`, at most 19 digits, and no larger than the route's body limit.
   - An oversize value gives 413.
3. Read `Transfer-Encoding` codings across fields in wire order. A non-final `chunked` gives 400 (RFC 9112 §6.3). Empty or malformed coding fields, parameters on `chunked`, and `Transfer-Encoding` on HTTP/1.0 also give 400.
   - A syntactically valid unknown coding gives 501 when no earlier framing error applies (RFC 9112 §6.1).
   - A single `chunked` coding selects chunked framing.
4. A request with neither field has an empty body.

**Chunked bodies** (RFC 9112 §7.1). Chunked framing requires CRLF, whatever `http1`'s head rule accepts.
5. **Chunk lines.**
   - The chunk size is 1-16 hex digits, and must not overflow or exceed the route's limit in total.
   - Chunk extensions are syntax-checked and discarded. A chunk line is at most 1024 B.
   - A chunk line ends in CRLF. A lone LF or a lone CR gives 400.
   - The chunk data is followed by exactly CRLF; anything else gives 400.
6. **Trailers.**
   - Each trailer line ends in CRLF, and the section ends with an empty CRLF line.
   - They are parsed with the head's field syntax and count toward `fields_max` and `head_max` with the head.
   - They are syntax-checked and discarded. They are never merged into the head, so the handler's `Request` is unchanged by them (law L2).
7. **Sticky failure.** A chunk error or a body-limit hit found during a body read is returned to the handler as `BodyError`. It also latches the connection as not reusable, even if the handler catches it and returns a `Response`.

**Host, target and Expect**
8. **Host.**
   - An HTTP/1.1 request needs exactly one `Host` field (RFC 9112 §3.2); otherwise 400.
   - Its value must be `uri-host [ ":" port ]` (RFC 3986 §3.2.2): a reg-name, an IPv4 literal or a bracketed IP literal, with a port of 1-5 digits at most 65535. Otherwise 400.
   - An absolute-form target's authority is validated the same way and is used in place of the `Host` value. `Host` must still be present and valid (RFC 9112 §3.2.2).
   - The CONNECT method and authority-form targets give 501. The asterisk form is allowed only with OPTIONS.
9. **Target normalization**, once, before routing.
   - The path is split on `/`. Then each segment is percent-decoded and must be UTF-8. A decoded `/` stays inside its segment.
   - A segment that is empty (`//`), `.` or `..`, before or after decoding, gives 400. A trailing `/` is an empty last segment, allowed only there.
   - The query stays raw for B-4; routing never reads it.
10. **Expect.**
    - `Expect: 100-continue` makes the connection write `100 Continue` the first time the handler reads the body, and never otherwise.
    - Any other expectation gives 417.

**Response framing**
11. The writer owns `Content-Length`, `Transfer-Encoding` and `Connection`.
    - A handler that sets one gets a typed `HeaderReserved` error. A field name or value outside RFC 9110 §5.1 and §5.5 syntax gets `HeaderInvalid`. Both become 500 before anything is committed.
    - A handler's final status is 200-599. A `Response` with a 1xx status is `StatusInvalid`, and becomes 500. Only the writer sends 100 and 101.
    - 1xx, 204 and 304 carry no body and no framing field.
    - A HEAD response has the framing fields GET's response would have, and no body (HTTP-11 says which handler runs).
    - A known-length body gets `Content-Length`.
    - A `Stream` gets `chunked` on HTTP/1.1, and is close-delimited on HTTP/1.0. Both carry `Connection: close` (HTTP-10).
12. **Interoperability.** Slice H2 sends the framing corpus through nginx and HAProxy in front of `serve` with stock, documented framing settings (law L2p). A vector passes if the proxy refuses it or the client receives the answer direct `serve` gives to the request the proxy actually forwarded, captured on the wire. In either case no second smuggled request may reach `serve`.

### HTTP-5: Absolute deadlines

Each phase, and each output unit, has one absolute deadline on the monotonic `Clock` (`host.mono_ns`). It is computed once, before the phase or unit starts, and never moved by progress.

| Phase | Starts at | Deadline |
|---|---|---|
| First head | accept | `first_head_ms` (WIRE `upgrade_ms`, 5000) |
| Idle | the previous response finishes | `idle_ms`, until the first byte of the next head |
| Head | the first byte of a later request | `head_ms` |
| Body | the head completes | `body_ms`, for every body read, drain included |
| Handler | the head completes | `handler_ms`, for every std wait the handler makes, B-1's calls included |
| Output unit | before the unit's first send | `write_ms`. A `Response` (head and body) is one unit; in a stream, each event and each control item is one unit |
| Drain | the handler returns with body unread | min(the body deadline, return + `drain_ms`) |
| Linger | close begins | `linger_ms` (`net.finish`) |

**How deadlines are checked.**
- Every read and write carries the current deadline as its `until`.
- **A set read** is the one `host.mono_ns(c)` read that computes a deadline, before its phase or unit starts.
- **A check read** is a fresh `host.mono_ns(c)` read between two successful operations under one deadline. The task stops with `TimedOut` once the deadline has passed, even when the next operation would be ready, so readiness-first operations cannot extend a deadline. Every task that runs I/O under a deadline makes them: the connection (reads, writes, body reads, drain), the refusal task (its send and `net.finish`), the stream writer, and `net.finish` itself (net.hirsel:10).
- Every set read and check read is a `Clock` input at a named point (§5).

**The handler.** A handler is supervised only at its waits. `Request.deadline` is the handler deadline. Every std wait the handler makes (`Body.read`, `ctx.call`, a runner read) uses min(its own deadline, `Request.deadline`). Pure computation is bounded only by a step bound (MEM-30), which a route may set (`steps`); CONC-7 does not preempt it.

**When a deadline passes.**
- Before anything is committed, the connection writes `408` (first head, idle, head or body deadline) or `503` (handler deadline), and closes.
- After a commit, it closes without a new status (HTTP-12).

### HTTP-6: The anchored request and the read buffer

**The request**
- `Request` is a scoped type (OWN-19) whose values are anchored to the connection's read buffer (OWN-20). The handler receives it as `req: Request in buf`, so it cannot outlive the handler call.
- Its accessors (`method`, `target`, `path`, `segments`, `query`, `field`) return `String` and `Bytes` views anchored to `req`. `http1.Span` offsets stay inside std; no handler sees an offset.
- `Outcome`, `StreamSpec`, `UpgradeSpec` and a producer's state are `send` types that contain no scoped type (OWN-22). A view cannot be returned in them or captured into them; the checker refuses it.
- A handler that keeps a field copies it with `http.copy`, charged to its `bound bytes` scope (MEM-25).

**The request scratch region**
- The connection opens one region per request, inside the handler's `bound bytes` scope, and passes it to the handler as `scratch: Region` (MEM-1). Request-lifetime documents, such as a parsed JSON body, are built there.
- It closes when the handler returns, on every exit edge (MEM-8): everything in it is freed and refunded at once, with no walk. Its allocations are charged to `handler_bytes` like any other, so no accounting row changes.
- Nothing in it outlives the request: `Outcome` holds no region data (MEM-7), so a response that keeps request data copies it into own values first.
- Closing returns the region's chunks to the connection task's allocator, and the next request's region reuses them, so request scratch is a rewind ([MEM-32](memory.md#mem-32-engine-obligations)).

**Compaction**
- While `req` lives, the read buffer is frozen (OWN-10), and nothing writes it. The connection moves the pipelined tail to the front only between requests.

**Body**
- A body of at most `body_inline` bytes that has fully arrived is a view anchored to `req`.
- Otherwise the handler reads through `Body`: `body.read(mut b: Body, mut into: Bytes) -> Result[u64, BodyError] waits`.
  - It first copies into `into` the body bytes already in the read buffer, which it only reads.
  - Then it receives into the connection's 8192-byte scratch and copies the body's bytes into `into`. Bytes past the body's end stay in the scratch, and join the read buffer when the handler returns.
  - The frozen read buffer is never written during the handler.

### HTTP-7: Drain, close and keep-alive

After the outcome, the connection closes when any of these hold:
- the request or response says `close`;
- the request was HTTP/1.0 without `keep-alive`, or the response was close-delimited;
- the outcome was a `Stream` (HTTP-10);
- the response is partial (a write failed, or its unit deadline passed);
- a framing error occurred, or a body failure latched (HTTP-4 rule 7);
- the unread body is larger than `drain_max`;
- draining does not finish by the drain deadline.

An `Upgrade` does not close. HTTP hands the socket to its protocol and does nothing more with it (HTTP-10), and never calls `net.finish` on it.

**Draining.** An unread body that may be kept is drained into the 8192 B scratch. Draining never grows the buffer, and counts the bytes it discards (law L5).

**Closing.** Close is `net.finish` under `linger_ms`. If `finish` cannot allocate its scratch, it still releases both halves (net.hirsel:5-8).

### HTTP-8: One handler at a time, and the pipelining allowance

**One handler.**
- A connection runs at most one handler at a time.
- While a handler runs, the connection issues no read except the handler's own body reads (HTTP-6).

**Pipelined bytes.**
- Bytes after the current request stay in the buffer, up to `pipeline_max`.
- The connection parses them only after the response completes.
- Reaching `pipeline_max` stops reading; that is the backpressure.
- A head that cannot fit in the remaining buffer after compaction is `TooLarge`.

**Where `pipeline_max` comes from.** It is part of H inside C, so it needs no other account.

### HTTP-9: The writer and the committed-prefix cursor

The writer owns `out_cap` bytes and a cursor `committed`: the count of response bytes the socket has accepted.

**Writing.**
- Static status lines and field prefixes are appended from constants.
- A body larger than the free buffer is written from the handler's bytes directly after the buffered prefix, with no copy into `out_cap`.
- A send that accepts some bytes (`Ok(n)`) advances `committed`, and the next send resumes there. A partial send is progress, not failure.
- A send error, or the unit's deadline, is terminal: the connection closes and never starts another response.
- Bytes before `committed` are never sent twice, and bytes after it are never skipped (law L4).

**Flushing.** The writer flushes before every wait: a body read, a handler wait and the idle wait. A response is never held behind a read that might not come.

**Ready batches.** The connection serves up to 16 ready pipelined requests, then calls `task.check_cancel()`. That is a scheduling point (CONC-10), so a connection that pipelines heavily lets the others run. This is the event-order form of hyper's 16 iterations. It does not bound one handler's own computation (HTTP-5).

**Date.** `Date` is formatted from one `host.wall_ms` read per response head. RFC 9110 §6.6.1 Date is wall time, which a monotonic reading cannot give. The read is an outside input (§5). The writer uses `std.time.from_unix_ms` for the calendar range. At Date-field encoding it returns `DateInvalid` before appending any part of that field if the timestamp is outside that range. IMF-fixdate requires a four-digit year (RFC 9110 §5.6.7).

### HTTP-10: Outcomes

A handler returns `Outcome = Response(Response) | Stream(StreamSpec) | Upgrade(UpgradeSpec)`. All three are `send`, and own everything in them (HTTP-6).

**`Response`**
- Status, fields and a body of known length. Writing it is HTTP-9.

**`Stream`, an ownership transfer**
- Status, fields, a source (§2) and its declared bounds, and `Request.deadline` as the group's pre-head deadline (§2), carried unchanged: no clock is read at the hand-over, so it can neither reset nor extend the deadline.
- std encodes the head (status and fields) and P into the one hand-over block W_s (HTTP-1), inside the handler's scope; the group's writer copies the head from it into its own `out_cap` buffer.
- A raw source's state P is `codec + send`. The handler builds the spec, and std encodes P into at most `state_max` bytes, inside the handler's scope; a P that does not fit is a handler error before anything is committed. The producer child decodes it into its own limit (§2), so every block P ever has is born and grows there.
- The connection sends `Promote(Stream(spec), halves)` to the supervisor and waits on its 1-slot reply.
- The supervisor admits S by spawning the stream group (§2) **before any byte of the response head is written**. The spawn is `task.try_spawn`, so on a refusal CONC-3 returns the halves inside the error. The supervisor passes them back, and the connection answers 503 and closes.
- On success the stream group owns the socket. The connection task ends, and its read buffer, with any pipelined bytes, is dropped. A stream always ends its connection, so its head carries `Connection: close`.

**`Upgrade`, an ownership transfer**
- Valid only as the answer to a request whose `Head` asked for that upgrade.
- An `Upgrade` request with a body (a `Content-Length` other than 0, or `Transfer-Encoding`) is refused with 400 before the handler runs, as WIRE-SPEC §2.1 already requires. So the tail is never a body.
- The spec carries the protocol's task function, its `bytes n` and its argument.
- The connection sends `Promote(Upgrade(spec), taken)` to the supervisor. `Taken` holds:
  - the `ReadHalf` and `WriteHalf`;
  - the read buffer, moved and not copied, with the **exact tail**: the bytes after `head.end`, including any already pipelined;
  - the encoded 101 head and the encoded argument (HTTP-1's W_u); the protocol task decodes the argument into its own `bytes n`, so nothing the handler allocated outlives the hand-over except these blocks.
- The supervisor spawns the protocol task with `task.try_spawn` under `bytes n` before the 101 is written. On a refusal, the connection gets `Taken` back and answers 503.
- The protocol task's first act is std's `upgrade.start(taken)`. It writes the full 101 under one output-unit deadline, then gives the protocol the halves and the tail. A failure before the 101 is complete closes the socket.
- The socket keeps its `sockets_max` slot until the protocol task ends. Its `conns_max` slot is released when the protocol sends `Established`; the runner's WebSocket sends it at Hello accepted.
- After the handoff, HTTP does nothing more with the socket: no drain and no `net.finish`.

### HTTP-11: Router and raw handlers

Handler state can hold immutable shareable data and granted capabilities such as
`Dir`. Move-only resources such as `Listener` and `Latest` stay with the task that
owns them. Channel `Sender` is sendable, and a shared state field requires shareability.

**Routes**
- `Router[S]` is an immutable value built by `router.add(mut b, method, pattern, route)` and closed by `router.build(b) -> Result[Router[S], RouteConflict]`.
- A pattern is literal segments, `{name}` captures of one whole segment, and an optional final `{name...}` that captures the rest. Patterns match HTTP-4 rule 9's decoded segments.
- A raw handler is `fn(req: Request in buf, scratch: Region, mut out: Responder, s: S) -> Result[Outcome, HandlerError] waits`; `scratch` is the request's scratch region (HTTP-6). It holds only the capabilities in `S` and its arguments (CAP-3).

**Conflicts**
- Precedence is Go's: the more specific pattern wins.
- Two patterns for one method that some path matches, with neither more specific, give `RouteConflict` at `build`. They are never resolved by order.
- Raw routes and generated routes (§3) are added to one builder, so they get one conflict check.

**Unrouted requests**
- No route for the path gives 404.
- A path routed for other methods gives 405, with `Allow`.
- `HEAD` runs the path's HEAD route if it has one, and otherwise `GET`'s handler with the body suppressed.
- `OPTIONS *` gives 204 with `Allow`.

### HTTP-12: Errors are values

- Before the final response's first byte is committed:
  - a handler's `Error(e)` writes 500, with no body detail;
  - a passed handler deadline (HTTP-5) writes 503.
- A failure in the handler, a refused admission's `Capacity` ([MEM-21](budgets.md#mem-21-admission)) included, closes the connection without writing a new status, before that commit or after it. A handler that answers a refusal with 503 runs its allocating work under `isolate` ([FAIL-16](failure.md#fail-16-isolation)) and returns an error.
- A cancellation (server shutdown) closes.
- None of these is a run stop: the connection's cleanup ends it (CONC-8).

### HTTP-13: The SSE encoder (WHATWG event stream)

`sse.event(mut out: EventBuf, name: Option[String], id: Option[String], data: Bytes) -> Result[(), SseError]`, with `sse.comment` and `sse.retry`.

**Field rules**
- `name` and `id` must contain no CR or LF, and `id` no NUL. Otherwise the error is `SseError.Field`.
- `data` must be UTF-8. It is split on CRLF, CR and LF, and each line is written as `data: <line>\n`.
- The event ends with `\n`.

**Atomicity**
- An event is appended whole or not at all. On any error, `out` is byte-identical to what it was before the call (law L7).
- An event longer than the `EventBuf`'s bound is `SseError.TooLarge`.
- The writer starts a unit only at an event boundary, and writes control items (§2) only between events.

So a stream cut at any byte delivers to a WHATWG parser exactly the complete events before the cut. An incomplete event is never applied.

### HTTP-14: Static asset manifest

At build time, a declared asset directory becomes a manifest, built into the image.
- **Entry:** a path, a media type, and one or more representations.
- **Representation:** a content coding (identity, gzip or br, precomputed), its bytes, and its digest (SHA-256).
- **Fingerprinted names:** `name-<first 8 hex of the identity digest>.ext`, served with `Cache-Control: public, max-age=31536000, immutable`.
- **Unfingerprinted names**, such as `index.html`, are served with `Cache-Control: no-cache`.

**Validators**
- Each representation's validator is the strong ETag `"<64 hex of its digest>"`.
- `If-None-Match` is compared per representation, and a match gives 304.
- The coding is chosen from `Accept-Encoding` among the precomputed ones, with `Vary: Accept-Encoding`.
- `Range` is ignored, so the answer is 200 with the full body (RFC 9110 permits this).

**Runtime**
- The manifest is immutable. Assets are never read from a `Dir` at run time, which replaces WIRE-SPEC §5's run-time read. So serving needs no file capability and reads no file.

### HTTP-15: Ownership inventory

Every byte the server keeps appears here, with its payer, admission and refund. HTTP-1's equations are sums of these rows, and no two rows overlap.

**Rules applied throughout:**
- A sent block stays charged to the sender's limit (MEM-20).
- Growth is charged to the owner's existing limit, not to the current scope (MEM-26).
- A channel's whole capacity is admitted when the channel is created (CONC-4).
- When a limit ends, its surviving blocks are handed to its parent (MEM-20). A row whose blocks outlive their task says where they go.

**Server**

| # | Retained thing | Bound | Paid by | Admitted | Refunded |
|---|---|---|---|---|---|
| 1 | Supervisor record, inbox, acceptors, router and `Config` | fixed | `serve`'s limit | at `serve` | at stop |
| 7 | Refusal tasks | `refuse_max` × R | the supervisor, pre-admitted | at `serve` | at stop; a slot is reused when its task ends |
| 7a | Accepted sockets not yet received | one inbox item each, inside row 1; the descriptor is outside logical charges (MEM-19) | row 1 | — | when received |

**Connection** (all inside C: admitted at spawn, refunded when the task ends, except rows 3 and 8's hand-overs)

| # | Retained thing | Bound | Notes |
|---|---|---|---|
| 2 | Task record | T(0) | |
| 3 | Read buffer: head, pipelined tail and inline body | B_r = `head_max + pipeline_max + body_inline` | never grows; moved with an `Upgrade` (row 15) |
| 4 | `Head` scratch and parse cursor | X, fixed | |
| 5 | Writer buffer and cursor | `out_cap` | never grows |
| 6 | Scratch: body reads, then drain, then `net.finish` | 8192 | one use at a time |
| 8 | The handler's scope: copied fields, body storage, response body, a `Stream`'s encoded state | `handler_bytes` | per request; a refusal gives a static 503 and the handler never runs; a response body is written before the scope exits; the encoded state survives the connection (row 13c) |
| 9 | JSON decoding into argument bytes (§3) | `arg_max`, inside row 8 | the JSON reply transcoder needs no scope (B-5) |
| 9a | JSON transcoder depth stack | J = 1024 | |
| 9b | Promote and read-slot reply channel | ch(1, reply) | |

**Streams (§2)** (inside S unless marked: admitted by the supervisor before the head is committed, refunded when the group ends)

| # | Retained thing | Bound | Notes |
|---|---|---|---|
| 10 | Owner record, inbox, outgoing queue, control slot, `net.finish` scratch, and (subscriptions) the retirement mark | T(0) + ch(cap, event) + (k + 1) × u + 8192 + ch(1, ()) | |
| 11 | Data items: being built, queued or held by the writer | (k + 1) × `event_max` | paid by the producer's `bytes`, because a sent block keeps its record (MEM-20) |
| 12 | Writer child, its channel, and for a subscription the depth stack | T(h) + `out_cap` + ch(1, unit) (+ J) | |
| 13 | Producer child, decoded P included | T(0) + `producer_bytes` | raw streams only; P's growth past it is the producer's `OutOfBytes` |
| 13a | Reader monitor and timer children | T(h) + 64 + T(0) + 4096 | |
| 13b | Credits channel | ch(k, unit) | raw streams only |
| 13c | The stream's hand-over block: encoded head and encoded P | W_s = `out_cap` + `state_max` | **not in S**: the supervisor's hand-over reserve (HTTP-1), from the connection's end until the writer has copied the head and the producer has decoded P |
| 14 | Subscription state and held content (B-9) | the session `budget` per group | **not in S**: on the publisher's ledger for the group's session, as for a session; held until the publisher's sweep finds the group's retirement mark (§2 Publisher retirement), which happens at the latest after the next message the publisher takes, so the sessions it holds, live or ended but not yet swept, number at most `streams_max` + `reads_max` + 1 (§2 Publisher retirement; reads, row 14a) |
| 14a | A one-shot read's subscription (B-11) | one frame's rows allowance | on the publisher's ledger; at most `reads_max` live or unswept (the read slot is held until its mark is set), retired as row 14 |
| 14b | Release fences (RUNNER-SPEC §3 "The release check", v2.19.1): each group and one-shot read's fence root, its first `Arc[Fence]` and two handles; one share of the place's round register at its writer | one root and one `Arc[Fence]` per group; two more `Arc[Fence]` and one `Arc[u64]` reserved at `Joined` | the root, its first `Arc` and the handles: the group's scope (row 14's session); the reserved `Arc`s: the publisher's ledger for the group's session, as row 14 |

**Upgrade**

| # | Retained thing | Bound | Paid by | Admitted | Refunded |
|---|---|---|---|---|---|
| 15 | The upgrade's hand-over blocks: the read buffer with the exact tail, the encoded 101 head, the encoded argument | W_u = B_r + `out_cap` + `state_max` | the connection's limit; the supervisor's hand-over reserve once the connection ends (MEM-20) | no new admission | the 101 head when written, the argument when decoded, the buffer when the protocol drops it |

**Runner (§3)**

| # | Retained thing | Bound | Paid by | Admitted | Refunded |
|---|---|---|---|---|---|
| 16 | An HTTP send's reply channel | ch(1, `DONE_MAX` + 16) | the connection (row 8's scope) | before the send goes to the relay | when the reply is received, or the connection ends |
| 16a | Relay send records (B-8): record, reply sender, deadline timer child | `relay_sends_max` × (64 + T(0) + 4096) | the relay's own limit | when the relay accepts the send | when the send's Outcome is handed over and its timer is joined, and, if that timer had queued an `Expired`, when the relay has taken it (B-8) |
| 16c | Delivered Outcomes (B-8): each a delivery block the relay copies the Outcome into | (`http_max` + 1) × `DONE_MAX` | the relay's own limit (MEM-20: the charge stays with the relay while the block lives) | when the relay copies an Outcome for hand-over | when the connection drops the block after writing its response, or at once when the reply channel is closed |
| 16b | Relay positions (B-8): argument copy, stream key, digest, and the outcome reserve for the head's answer | `relay_positions_max` × (`arg_max` + 128 + `DONE_MAX`) | the relay's own limit; the head's `Told` payload is allocated by the publisher inside this reserve (RUNNER-SPEC F12) | when the relay forwards the position | when the head's `Told` for it reaches the relay, never at a send's `Abandoned` |
| 17 | Keyed-operation claims (§4) | K-7 | **Part B**: specified in §4, not built with Part A | | |

**Bounds.** HTTP-1's sum bounds the server, row 14 and 14a bound what the publisher holds for HTTP, and the relay's limit (B-8) bounds every request that outlives its connection. No row's total depends on client behaviour beyond these bounds.

### HTTP-16: Client response body storage

`std.http_client.read(mut connection, mut into)` appends decoded response
payload to the caller's existing `Bytes` capacity. A full buffer returns
`NoSpace` before consuming more of the response. The caller can resume with
spare capacity. A zero-byte result marks the end of the response.

The connection copies any payload that arrived during its head read once.
After that it receives payload directly into the caller's allocation. Each
receive is bounded by the available capacity and the current Content-Length
or chunk remainder. Close-delimited bodies use the remaining decoded-body
limit and require transport EOF. Chunk framing uses the fixed wire buffer
and never enters the caller's payload. Any bytes buffered beyond the body
stay available for the next response.

The caller's storage retains its existing payer and charge (MEM-19 and
MEM-20). Body reads require no new admission or growth. The connection
keeps its fixed 8192-byte wire buffer and 8192-byte final-head storage.

### HTTP-17: Client request bodies

The std HTTP client (`http_client`, [api](../../core/std/api/http_client.md)) sends a
request body in one of three modes:

```hirsel
pub type Coding: key + send = Raw | Form
pub type Segment: send = Plain(Bytes) | Sealed(Secret, Coding)
pub type RequestBody: send = Fixed(u64) | Chunked | Segments(List[Segment])
```

- **`Fixed(n)`** declares `Content-Length: n`, and `write` sends exactly `n` plain bytes.
- **`Chunked`** declares `Transfer-Encoding: chunked`. `write` sends one chunk of plain
  bytes, and `write_segments` sends one chunk made of a sequence of segments.
- **`Segments(list)`** is a whole body given with the request. `start` sends the head and
  the body together, and `finish_request` follows with no `write`.

```hirsel
fn write_chunk(mut c: std.http_client.Connection, segments: List[std.http_client.Segment]) -> Result[(), std.http_client.Error] waits {
    std.http_client.write_segments(mut c, segments)
}
```

A body is a sequence of **plain** and **protected** segments. `Plain(bytes)` is sent as
it is. `Sealed(s, coding)` is the bytes of the `Secret` `s`, written by the runtime in
`coding`: `Raw` sends them unchanged, and `Form` form-urlencodes them (TLS-5's serializer,
WHATWG URL §5.2: `*-._`, digits and letters unchanged, a space as `+`, every other byte as
`%` and two uppercase hex digits). `Form` is for a value inside an
`application/x-www-form-urlencoded` body, so an OAuth refresh is

```text
Segments([Plain("grant_type=refresh_token&refresh_token="), Sealed(token, Form)])
```

with the caller's `Content-Type: application/x-www-form-urlencoded` header. The program
writes the names, `=` and `&` itself, and encodes plain values itself.

**Content-Length.** For `Segments`, `Content-Length` is the sum of the plain segments'
lengths and the encoded lengths of the protected ones. The program cannot compute it,
because no operation tells it a secret's length (TLS-5). When the body holds no protected
segment, the client writes the sum itself and sends the request over any stream. When it
holds one, the head, the body and every protected header go out in one `tls.send_secret`
call. The `Content-Length` value is a kind 2 mark whose range is the whole body, so the
runtime writes the digits, and a protected segment that ends the body counts. The number
never reaches program memory, a render or an error.

**Chunked bodies.** `write_segments` on a `Chunked` upload sends one chunk: the chunk size
is a kind 3 mark over the chunk's data, so the runtime writes it in hex. A chunk whose
segments hold no plain byte is refused with `InvalidRequest` before any byte is sent,
because its encoded length could be zero, and a zero size line would end the body. Each
call sends one chunk, so a stream of protected values is several calls.

**Authority.** Every protected segment, in a header or in the body, passes TLS-5's sink
checks exactly as a protected header does: all of them are checked before the first byte
of the request is sent, a `Secret` whose `tls_send` patterns do not cover the stream's
verified name gives `Write(Denied)` with nothing sent, and on plain TCP any protected
segment gives `Write(Denied)` before any byte. A refused `start` or `write_segments`
leaves the connection unusable, as any write failure does.

**Bounds.** The head, any `Segments` body's plain bytes and a chunk's plain bytes share the
8192-byte encoding buffer when the request carries a secret, so a larger plain part gives
`TooLarge` before any byte is sent. A protected segment's bytes are not counted: they never
enter the buffer, and a bound on them would reveal their length. A `Segments` body with no
protected segment is sent from the caller's bytes and has no such bound.

**Renders.** `render_request` and `render_error` print the method, target and fields, with
`[secret]` for each protected value. They do not print a body.

**Fixed uploads.** `write_segments` on a `Fixed(n)` upload gives `InvalidRequest`, because
`n` cannot count a protected segment. `write` on a `Segments` upload gives `State`.

## 2. Stream ownership

A `Stream` (HTTP-10) runs as one task group, spawned by the supervisor. It follows RUNNER-SPEC §3's session group: an owner that waits only on its inbox, and children that report to it.

**Two phases.**
- **Pre-head.** The group starts here. Its deadline is the spec's pre-head deadline, the handler's `Request.deadline` (HTTP-5). Nothing but a response head may be written. There is no heartbeat, and the lease has not started. A raw stream leaves pre-head at once: its first unit is the head. A subscription leaves it at its completing initial Batch (B-9).
  - If the reader monitor reports `ReadEnd` first, the group ends without writing.
  - If the pre-head deadline passes first, the writer writes HTTP-5's handler-deadline `503`, as one output unit, and the group ends.
  - If an `Ended` comes first (B-9), the writer writes B-7's response for its Outcome, and the group ends.
- **Committed.** From the head's first byte. The owner makes a set read (HTTP-5) then: `lease_end` = that reading + `lease_ms`.

**The owner**
- Its one wait is its event inbox. The events are:
  - `Item(event)` from the producer;
  - `Sent` and `WriteFailed` from the writer;
  - `ReadEnd` from the reader monitor;
  - `Tick` from the timer;
  - `SourceEnded(result)` from the producer;
  - for a subscription, the publisher's content (`Batch`, `Ended`, `Reset`), charged and coalesced exactly as for a session (RUNNER-SPEC §3).
- Capacity: k + 4 for a raw stream; the session's inbox capacity for a subscription (RUNNER-SPEC §3).
- **At every event** the owner makes one check read and tests the pre-head deadline (pre-head) or `lease_end` (committed). So the deadline and the lease are honoured at the first event after they pass, and the timer guarantees an event at least every `heartbeat_ms` in both phases, and one at the pre-head deadline: a `Tick` is dropped only when the inbox is full, and then an event is already waiting.
- It never waits on a child or on the publisher while handling an event. It hands the writer one unit by `try_send`, and only when the writer is idle (after its `Sent`), so that `try_send` always succeeds.
- It holds the outgoing queue (at most k data units), one control slot, the credits sender and the children's handles.

**The writer child**
- It holds the **only** `WriteHalf`, the writer buffer and the committed-prefix cursor.
- It takes one unit from its capacity-1 channel and writes it whole under that unit's own deadline (HTTP-5: a set read before its first send, then `write_ms`), with check reads between sends. Then it reports `Sent`, or `WriteFailed`.
- For a subscription it encodes each SSE event from the frame as it writes (B-9), with the transcoder's depth stack J.
- Nothing else writes to the socket. When it returns, its result is its `WriteHalf`.

**The reader monitor child**
- It holds the `ReadHalf` and a 64-byte scratch, and reads until end of stream or an error, discarding what it reads.
- EOF, an error, or more than `pipeline_max` bytes in total report `ReadEnd`.
- When it returns, its result is its `ReadHalf`.

**The timer child**
- One periodic child ([`stream.hirsel:209`](../../core/std/stream.hirsel#L209)), with one schedule for the group's whole life, so commit needs no transition: it wakes at start + n × `heartbeat_ms` for n = 1, 2, …, and additionally once at the pre-head deadline if that is not already a wake. At each wake it calls `host.sleep_until(c, t)` and then `try_send(Tick)`. A Tick in the committed phase may come sooner than `heartbeat_ms` after the commit; the owner's heartbeat rule (idle writer, nothing queued) makes that harmless.
- A full inbox drops the tick. That is harmless: the owner checks at every event, and a full inbox has events waiting.

**The source**
- **Raw: a producer child.** Its state P is `codec + send` (HTTP-10), so it holds no resource (OWN-4) and its drop is mechanical release (FAIL-13), which waits for nothing. Its function is per item: `fn(mut st: P) -> Result[Option[Event], E] waits`. std runs:
  0. Decode P from the spec's encoded state into the producer's own limit, then drop the encoded bytes. A decode failure ends the stream with `SourceEnded(failed)`.
  1. Take a credit. This is a wait, so it is a cancellation point.
  2. Run `next` inside `bound bytes event_max` and the route's step bound `producer_steps` (MEM-30).
  3. Send the item.
  - `None` ends the stream normally. `Error`, `OutOfBytes` or `OutOfTime` ends it with `SourceEnded(failed)`. P's own growth beyond `producer_bytes` is `OutOfBytes` in the producer, not a server `Capacity`.
- **Subscription: the publisher**, exactly as for a session (B-9). The group joins the publisher as its own session (`Joined`, then `Subscribed`). Its `Joined` carries the receiving end of the session's **retirement mark**, a ch(1, ()) the session creates and pays for (row 10). The publisher `try_send`s to the owner's inbox, and holds content on the group's `budget` ledger while the inbox is full.

**Credits (raw sources).** There are k = 2 data credits. At every event this invariant holds, and law L8 asserts it:

`available credits + items being built + items queued at the owner + items held by the writer = k`

- A credit returns to the producer only when a data item it paid for is `Sent`.
- Control items (a heartbeat comment, the terminating chunk, SSE control events) use the one control slot and never touch a credit.

**Heartbeat and lease.**
- A committed `text/event-stream` stream gets a heartbeat. On a `Tick`, when the writer is idle and nothing is queued, the owner hands the writer `:\n\n`. Other streams have no heartbeat.
- At `lease_end` the stream ends normally, and the client reconnects. For a subscription that is a fresh subscription (B-9).

**Ending.** The owner decides the end on `SourceEnded`, `ReadEnd`, `WriteFailed`, the pre-head deadline, `lease_end`, or (B-9) a Reset or an Ended. Then, in this order:
1. Cancel the producer child, if there is one.
2. If the writer is healthy and the head is committed, queue the terminating control item (the `0\r\n\r\n` chunk, or B-9's final event). It has one deadline, end decision + `write_ms`, shared with the unit in flight.
3. On `Sent` of the terminal item, or at that deadline, cancel the writer, the reader and the timer.
4. Join every child. Each cancel comes before its join.
5. If both halves came back, call `net.finish` under `linger_ms`, from the owner's own 8192-byte scratch. Otherwise the halves were dropped with their children, which is a mechanical close.
6. For a subscription: drop the owner's inbox `Receiver` and the mark's `Sender`, which closes the mark, then `try_send` one `Drained` (RUNNER-SPEC v2.13) on PubIn. Nothing here waits. Dropping is mechanical release, so an owner that fails or is cancelled closes its mark too, as its blocks are released. Such an owner sends no `Drained`, so the supervisor `try_send`s one after it joins the group's task, whatever the outcome (RUNNER-SPEC v2.14, CHANGES-v2.14 item 3).

**Publisher retirement** (RUNNER-SPEC v2.13's pattern: a mark the session owns, swept by the publisher after every message, and a coalesced wake that is safe to drop).
- **The sweep.** After every message it takes, the publisher `try_receive`s the retirement mark of each joined session that has one. A closed mark means the session ended: the publisher drops its subscriptions (their Cover, read set and held content) and the session, as for `Left`. A live session's mark is empty, so the test changes nothing.
- **The wake.** The ending session's `Drained` makes sure such a message comes. If PubIn is full the `Drained` is dropped, which is safe: a full PubIn holds a message the publisher will take, and the sweep follows it. No step depends on a blocked sender winning a slot (CONC-10 promises no such fairness).
- **So every ended group is retired** without any later traffic and without the group waiting. A group's slot is freed when its task ends, which may come before the sweep. But a session gets state on the publisher only when the publisher takes its `Joined`, and a sweep follows every message. So right after each sweep every session the publisher holds had an open mark, which means it was a live group or read holding a `streams_max` or `reads_max` slot. A call response does not join (RUNNER-SPEC v2.19.1). Before the next sweep one more message can add one session. The publisher therefore holds state for at most `streams_max` + `reads_max` + 1 sessions.
- **What this adds to RUNNER-SPEC** (§7 prerequisites): `Joined` may carry a retirement mark, and the publisher sweeps marks after every message. PubIn's message variants are unchanged, and no frame changes. A WebSocket session needs no mark; its end stays `Left`.

**The release bound.** From the end decision, the group's own work and waits take at most:
- `write_ms` (the terminal item), then
- the producer's run to its next cancellation point: at most one item's `producer_steps`, because pure computation is not preempted (CONC-7) and MEM-30 bounds it, and P's drop is mechanical;
- `linger_ms` (`net.finish`);
- nothing for the publisher: retirement does not wait (Publisher retirement).

So a group's whole life is at most `handler_ms` (pre-head) + `lease_ms` + `heartbeat_ms` + that release bound. These are bounds on the group's own progress. Scheduling delay is outside them: CONC-10 orders events but promises no latency.

**The reservation S** is the sum of:
- the owner: T(0) + its inbox ch(cap, event) + the outgoing queue and control slot (k + 1) × u, where u is the size of one queued unit's descriptor, + 8192 (`net.finish`) + ch(1, ()) (the retirement mark, subscriptions);
- the writer: T(h) + `out_cap` + ch(1, unit), plus J for a subscription;
- the reader monitor: T(h) + 64; the timer: T(0) + 4096;
- the source's part:
  - for a raw source, T(0) + `producer_bytes` + ch(k, unit), where `producer_bytes ≥ (k + 1) × event_max + state_max`: k items alive, one being built, and room to decode P. This is checked when the `StreamSpec` is built, so a spec that cannot fit is a handler error before commit;
  - for a subscription, nothing in S: its content is on the publisher's ledger under the session `budget` (HTTP-15 row 14).
- The encoded producer state is not in S: it is the supervisor's hand-over reserve (HTTP-1), freed at step 0.
## 3. Runner bindings (Layer 2)

### B-1: The sealed handler context

A runner-managed handler, generated or raw, receives `runner.Ctx` and never a `Db`, `Tx`, `Head` or the place's grants.

`Ctx` is opaque. It holds three things, and only these:
- **The admitted principal**, bound by B-2.
- **An attenuated call client**, which can call only the entries named in the handler's route. `ctx.call(entry, args)` for any other entry is `Refused(NotAllowed)` before forwarding.
- **Read authority**, for the query entries named in the route: a one-shot read (B-11) or a subscription (B-9).

The composition root (`place`) keeps managed-store write authority inside the runner. A handler writes only by calling a transact entry. This is CAP-3 applied: authority is exactly what `Ctx` carries.

### B-2: One transport-neutral ingress service

Every request from any transport passes the same four steps, in this order. WebSocket sessions and HTTP handlers both use them.

**1. Source.** The transport classifies the source with WIRE-SPEC §2.1's sanitized rule (W-D5): the TCP peer, or the one `X-Forwarded-For` literal from a trusted `ingress` peer, normalized once. HTTP and WebSocket share that code.

**2. `Ingress`, the limits task.** It owns the state that crosses connections and sessions: the per-IP limits. It authenticates nothing and authorizes nothing (RUNNER-SPEC v2.24 rule 13, ruling R6).
- **The Caller.** The application's binding (`runner.wire`, RUNNER-SPEC §3) maps the credential to a Caller, or refuses it. With no binding, each session, and each HTTP request, gets a fresh anonymous Caller of 16 bytes from `host.random`. Ingress mints no principal. A keyed binding (Part B) binds its Caller with its own claim function in place of `Wire.bind` (K-2), the same mechanism with the request's named headers and body as input.
- **No per-Caller rate.** Ingress keeps no state keyed by Caller and allows or refuses nothing by Caller (rule 13). The configured transport and session bounds limit load instead (RUNNER-SPEC §3's threat table): `max_sessions` and `socket.pending_max` bound transport admission (HTTP 503), and each session's `1+q` unit limit bounds its sends (`Refused(Overloaded)`). An anonymous Caller is one session, so a per-Caller rate would only repeat the session bound.
- **State bound.** Per-IP state lives in a fixed table of `buckets_max` entries (8192). A bucket is dropped only when it has refilled to full, because a full bucket carries no information; dropping never resets an active limit. When the table is full of non-full buckets, a request with a new key is refused as overload.
- **Inbox.** Capacity `ingress_inbox` (256). A caller sends `Ask(source, principal, kind, reply)`, where `reply` is a fresh 1-slot channel, so Ingress answers every ask by `try_send` and never waits. A caller that finds the inbox full is refused as overload without asking.
- **What it decides, and nothing more.** It does no entry lookup, decoding or validation, so its work per ask is constant.
- **Its answer** is an opaque `Admitted`: the authenticated principal, the transport's entry attenuation (the route's entries, or the session's Hello entries), and the arrival time, a `host.mono_ns` read (§5).

**3. `runner.prepare(reg, a: Admitted, want: Want) -> Result[Send, Refusal]`.** It is pure and runs in the caller's task, outside Ingress's loop. It:
- resolves the entry key against the registry and the gone-key set (RUNNER-SPEC §6):
  - **Ready** (a current key): continue;
  - **Gone** (a key in `previous ∪ current` minus this registry's keys): continue as a gone call, which the head answers `Failed(Gone)` under V5, durably;
  - **Unknown**: `NoEntry`, with nothing recorded;
- checks the entry against `a`'s attenuation (`NotAllowed`);
- decodes the argument bytes with the entry's real decoder, inside `bound bytes arg_max` and a step bound, so a `via` validator's work is bounded (`Malformed(Bad)`);
- computes `request_digest(entry key, argument bytes)` (store.hirsel:1303);
- forms the stream key `caller_key ‖ u32-BE(stream)` from the Caller ingress computed (RUNNER-SPEC v2.15 item 2: the bearer token is never stored);
- checks the byte budget and the deadline against `a`'s arrival time (`Budget`, `Deadline`).

`Send` holds the entry key, stream key, position, argument bytes, digest, budget and deadline. It is opaque, and made only by `prepare` (and, in Part B, by K-9's `prepare_retired` for a `Retired` binding). So the store never sees a digest the runner did not compute.

**4. Reserve, then forward.** The send takes its unit before it is forwarded: a session's slot table (RUNNER-SPEC §3) or the HTTP relay (B-8). Only then does it reach the head.

**Refusals are phase-specific** (RUNNER-SPEC §3's threat table, WIRE W-D2):
- limits known before upgrade or before a request is read (per-IP sockets, pending sockets): HTTP 503 at `serve` while a refusal slot is free, and otherwise a bare close with no response (HTTP-1);
- a full per-connection frame pool after upgrade: WebSocket Close 1013;
- a credential the application's binding refuses: WebSocket Close 1008, HTTP 401;
- a full Ingress inbox or bucket table, a full relay or slot table: `Refused(Overloaded)` from capacity, which HTTP answers 503.

**What moves out of the WebSocket session.** The session (runner.hirsel:1241) currently does all of this inline: Hello token binding and principal minting, entry lookup, `check_arg` (runner.hirsel:372), building `Req` with its digest (runner.hirsel:742), and the budget and deadline refusals. These move into `Ingress` and `prepare`. The gone-key handling of RUNNER-SPEC §6 moves with them, so the current `NoEntry` refusal at runner.hirsel:1220 does not survive the move for a gone key.

**What the session keeps:** frames, the slot table (1+q units) and V2 local joins, Batch building, the outcome reserve, subscriptions and Reset.

### B-3: `HttpBinding` values

There is no new syntax. A binding is immutable library data, registered in `place` after the entries:

```hirsel unbuilt
pub type Mode: send = Read | Subscribe | Stream   // Part B adds Keyed(KeyedRule[G]) and Retired(Retirement[G]), and so the registry's G to Mode and HttpBinding (K-1)
pub type Source: send = Path | Query | Body
pub type FieldSource: send { field: String, source: Source }
pub type HttpBinding: send {
  op: String, method: Method, path: String, entry: Bytes, fields: List[FieldSource], mode: Mode,
  ok_status: u16, error_status: u16, body_max: u64, arg_max: u64, deadline_ms: u64, steps: u64
}
pub fn bind[G, H](mut r: Registry[G, H], take b: HttpBinding) -> Result[(), BindError]
```

**Finalization.**
- `describe` finalizes entries and bindings in the same pass.
- Its output appends the bindings after the entries. This is a change to the describe format, and the generator reads both. The activation table (RUNNER-SPEC §6) therefore keeps the bindings of `previous` and `current`, so a removed binding's `op` stays known (B-12).
- A `BindError` fails `place`, so describe fails (exit 2) and the runner does not start. A bad binding is never found at request time.

**Checks at `bind`:**
1. **The entry exists.**
2. **The mode fits the entry's kind.**
   - Query entries take `Read` (GET, B-11) or `Subscribe` (GET, `text/event-stream`, B-9).
   - Transact entries take `Stream` or `Keyed` (POST, PUT, PATCH or DELETE). `Keyed`, `Retired` and their rules are §4's, Part B: until Part B lands, those variants do not exist in std. A `Retired` binding has no entry (K-9): of these checks only 3 and 7 apply to it, with K-1's own list, because its argument schema, field mapping and statuses are the op record's, compared with the store at open (K-1), not at `bind`.
   - Compute entries take `Read` (GET or POST; no identity, because they are pure).
   - Granted entries are refused. An HTTP retry of an outside effect without a durable identity is a duplicate effect.
3. **Each durable endpoint binds exactly one entry,** and `op` is unique in the registry and matches `[a-z0-9][a-z0-9_.-]{0,63}` (B-12).
4. **Fields.**
   - Every field of the argument record has exactly one `FieldSource`. No field has two, and no path capture, query name or body member is shared by two fields.
   - Every path capture names a field whose source is `Path`, and every `Path` field is captured. There is no reserved capture.
   - `Path` and `Query` fields have B-4's path and query forms.
   - GET has no `Body` fields.
5. **Shapes.** The argument and result schemas have B-4 forms and pass B-5's analysis; otherwise the refusal names the opcode or the cycle.
6. **Statuses.** `ok_status` is 200 or 201. `error_status` is in 400-499; 422 is what the generator writes.
7. **Routes.** Raw and generated routes go into one router build (HTTP-11), with one conflict check.

### B-4: Lexical forms (JSON, path and query)

**What the bytes depend on.** The argument bytes are a function of the parsed value, not of its spelling.
- `request_digest` is computed over those bytes, so equal values give equal digests over HTTP and WebSocket.
- Each shape has one canonical spelling, which the encoder writes. Another accepted spelling must denote the same value.
- JSON is RFC 8259 in UTF-8. Nesting deeper than 64 gives 400.
- **Duplicates.** A duplicate member name in any JSON object (a record, a `Result`, a sum variant object) gives 400, and so does a duplicate key in a Map's pairs. No first-wins or last-wins.

| Opcode | Type | JSON | Path / query |
|---|---|---|---|
| 0 | `()` | `null` | refused at bind |
| 1 | bool | `true`, `false` | `true`, `false` |
| 2 | char | a string of exactly one scalar value | one scalar value |
| 3-5 | u8, u16, u32 | a number matching `0\|[1-9][0-9]*`, in range | the same digits |
| 6 | u64 | a **string** of canonical decimal, because JSON numbers above 2^53 are not portable | canonical decimal |
| 7-9 | i8, i16, i32 | a number matching `-?(0\|[1-9][0-9]*)`, in range, with no `-0` | the same |
| 10 | i64 | a **string** of canonical decimal | canonical decimal |
| 11, 12 | f32, f64 | below | refused at bind |
| 13 | String | a string. `\u` escapes decode; a lone surrogate gives 400. The output escapes `"`, `\` and U+0000-U+001F as `\u00XX`, and nothing else | percent-decoded; it must be UTF-8 |
| 14 | `List[u8]` (Bytes) | a string in RFC 4648 §4 base64 with padding; non-canonical padding bits give 400 | refused |
| 14 | `List[E]` | an array, in order. An E with LEAST 0 is refused at bind | query only, below |
| 15 | `Map[K, V]`, any K | an array of `[k, v]` pairs in insertion order. A pair is a 2-element array; a duplicate key gives 400. An entry (key plus value) with LEAST 0 is refused at bind, the codec's rule (codec.hirsel:463) | refused |
| 16 | `Option[T]` | `null` or T's form. As a record field, an absent member is also None. Refused at bind when T is nullable (below) | query: below. Path: refused |
| 17 | `Result[T, E]` | `{"ok": t}` or `{"error": e}`, exactly one member | refused |
| 18 | tuple | refused at bind | refused |
| 19 | record | an object with its fields by name, in any member order. An unknown member gives 400. A missing member gives 400 unless the field is an `Option` (None) or has a declared default (the default, which is the HTTP form of RUNNER-SPEC §6's `Resolve`) | only its scalar fields, through `FieldSource` |
| 19 | sum | a variant with 0 fields is `"Name"`; with 1 field `{"Name": v}`; with n ≥ 2 `{"Name": [v1, …, vn]}` | refused |
| 19 | via | the form of the type it is written as | that type's form |
| 20 | Never | no JSON value is accepted | refused |

**Nullability.** A type is nullable when its JSON form can be `null`: `()`, `Option[_]`, and a `via` or nominal type whose underlying type is nullable, computed through every `via` and nominal definition. `Option[T]` with a nullable T is refused at bind, because `null` would have two meanings.

**Floats.**
- **Input:** any JSON number, or the strings `"NaN"`, `"Infinity"` and `"-Infinity"`. A number is rounded once, directly to the target width (f32 or f64), to nearest, ties to even. A finite number that rounds past the target's largest finite value gives 400; it never becomes infinity.
- **Output:** a finite value is written as ECMA-262's `Number::toString` writes it (the shortest round-trip digits, with that function's choice of fixed or exponent form); for f32 the digits are the shortest that round-trip at f32. `-0` is written `-0`. Infinities are the strings above. NaN is written `"NaN"`, and `"NaN"` reads as the canonical quiet NaN, which is the only NaN the codec accepts (codec.hirsel:242).

**Map order is semantic.** The codec writes a Map in insertion order, so a reordered Map is another value with other bytes and another digest. The pair array keeps that order in every JSON library, including JavaScript, whose objects reorder integer-like keys. The generated TypeScript client represents a Map as an array of pairs, or an ES `Map`, which keeps insertion order.

**Query strings** use RFC 3986 percent-decoding only (`+` is a literal plus).
- An unknown parameter gives 400.
- A repeated parameter whose field is not a List gives 400.
- `List[E]` (E a path/query scalar): the parameter repeated, in order. An absent parameter is the empty list. `name=` is one element, the empty string for a String E, and 400 for any other E.
- `Option[T]` (T a scalar): absent is None; present is Some, with T's form.

**Body.**
- With `Body` fields, `Content-Type` must be `application/json` (an optional `charset=utf-8`); otherwise 415.
- With no `Body` fields, the body must be empty; otherwise 400.

**Assembly.** The record is assembled from the path, the query and the body.
- It is encoded in declared field order inside `bound bytes arg_max`.
- Then `prepare` runs the entry's real decoder over those bytes, so a `via` type's own validation applies.
- A refusal at this step is 400, with `Bad.kind`.

### B-5: Reply encoding and its bound

**The reply** decodes as the entry's result type: `Result[R, E]` for transact, R for compute, and B-11's envelope for a query read.

**Encoding is a streaming transcoder.** It reads the reply's binary codec bytes and writes JSON through the writer, a piece at a time.
- A first pass computes the exact JSON length for `Content-Length`, allocating nothing. A second pass writes.
- Its only storage is a depth stack of J = 1024 bytes (16 per level, 64 levels), inside C. So encoding allocates nothing per reply, and cannot fail on `Capacity`.
- A reply nested deeper than 64 levels has no JSON form under B-4's depth rule. It is answered 500 `{"fault": "depth"}`. The durable outcome is unchanged, and a WebSocket client still receives it.

**The length bound.** B-3 rule 5 runs a schema analysis that gives `JSON length ≤ a·n + b` for every reply of n binary bytes. It is used for the write deadline's sizing and for B-9's events, not for memory.
- **Per node.** Each schema node gets (a, b). Leaves: bool 5 B per 1 B; u8-u64 and i8-i64 their longest decimal (with quotes for 64-bit) per their width; f32/f64 24 B per 4 or 8 B; char 12 B per scalar; String 6 B per byte plus 2 charged to its count; Bytes 4/3 per byte plus 2. Records, sums, Options, Results, Lists and Maps add their punctuation and names.
- **Charging fixed text.** A node's fixed text (braces, names, a variant name) is charged to the binary bytes the node itself consumes: a sum's tag, an Option's tag, a List's or Map's count. A node that consumes no byte of its own charges its fixed text to its parent.
- **Recursion.** Every cycle in the schema graph must pass through a node that consumes at least one binary byte of its own (a tag or a count). A cycle that does not is refused at bind, because its fixed text could repeat without consuming input.
- **Zero-byte values.** `()` and empty records consume nothing; their fixed text goes to b, or to the nearest consuming ancestor. B-4 already refuses them as List elements and Map entries.
- **Arithmetic.** a and b are computed with checked u64 arithmetic; an overflow is a bind refusal.
- **Proof obligation.** The bound is checked against the binary bytes the codec decoder consumes (law L12).

### B-6: Retry identity under `Stream`

**The header.** A `Stream` binding requires `Idempotency-Key: "<stream>.<position>"`, an RFC 8941 sf-string.
- `stream` is canonical decimal and at most 2^32−1. `position` is canonical decimal and at most 2^64−1.
- A missing header gives 400. Any other spelling gives 400.

**The Caller.** The runner authenticates nothing (RUNNER-SPEC v2.24 rule 13). The request's `Authorization` value goes, unread, to the application's binding.
- A binding's `Error(())` gives 401. With no binding, the request is its own anonymous session with a fresh Caller, so a `Stream` binding's dedup across requests needs an application binding. There is no `POST /.hirsel/principal`.
- The stream key is the WebSocket one: `caller_key ‖ u32-BE(stream)` (RUNNER-SPEC v2.15). A credential the binding maps to one Caller gives the same streams over both transports (RUNNER-SPEC law L29).

**Behaviour.**
- The send is one send under RUNNER-SPEC V1-V5, held by the relay (B-8): one unit, its own deadline (`deadline_ms` from arrival), and exactly one Outcome.
- The store's admission is the only memory of the request. There is no HTTP dedup cache and no mapping from opaque keys.
- An HTTP request and a WebSocket `Call` with equal (principal, stream, position, entry, argument bytes) are the same send at the store.

### B-7: Statuses

**Transport refusals (before `prepare`)**

| Answer | HTTP |
|---|---|
| Framing and route errors | HTTP-4 and HTTP-11 |
| Missing or invalid `Idempotency-Key` | 400 |
| A credential the application's binding refuses (RUNNER-SPEC v2.24) | 401 |

**Refusals from `Ingress`, `prepare` and the relay (nothing recorded)**

| Answer | HTTP |
|---|---|
| `Refused(NoEntry)` | 404 |
| `Refused(Malformed(Bad))` | 400 `{"refused": "malformed", "kind": <Bad.kind>}` |
| `Refused(NotAllowed)` | 403, with no body |
| `Refused(Overloaded)`, from capacity (the Ingress inbox or bucket table, the relay, the head's store refusal under V3) | 503, `Retry-After: 1` |
| `Refused(Deadline)` | 504 |
| `Refused(Budget(_))` | 503, `Retry-After: 1` |

The relay knows which `Overloaded` it has, because it made the decision or received it from the head, and `Refusal` carries no reason. The wire's `Refusal` tags are unchanged.

**Answers about the request's identity**

| Answer | HTTP |
|---|---|
| `Refused(RequestMismatch)` | 422 `{"refused": "request_mismatch"}` |
| `Refused(HistoryUnavailable)` | 410 |
| `Abandoned` (this send's deadline passed while it was owed) | 409, `Retry-After: 1`: in progress, retry with the same key |
| A send the head holds in its FIFO behind `Rejected(latest)` (an earlier position not yet re-sent, H2) | no answer until the send's deadline, then `Abandoned` (409); the connection waits at most `deadline_ms`, inside the handler deadline |
| store `Error(Full)` | 503, `Retry-After: 1` |

**Durable outcomes**

| Answer | HTTP |
|---|---|
| `Returned(Ok(r))`, or R for query and compute | `ok_status`, with r in JSON |
| `Returned(Error(e))` | `error_status`, with e in JSON |
| `Failed(Raised(k))`, `Failed(OutOfBytes(_))`, `Failed(OutOfTime(_))`, `Failed(Gone)` | 500 `{"fault": "<variant>", ...}` |
| `InDoubt` | 502 `{"outcome": "in_doubt"}`. It never means a fresh key is safe; the client retries with the same key |

**Duplicates.** When the head's answer came from a `Duplicate` admission, the relay sets `Idempotent-Replayed: true` beside the original outcome's status and body (B-8 says how it knows). The saved reply is delivered as it was committed (RUNNER-SPEC v2.19, ruling 73 as amended). So equal identities give equal statuses and bodies over any transport.

`Retry-After` is the constant 1. It is not computed from load, so it needs no outside input.

### B-8: The HTTP relay, and delivery failure

**The relay** is a runner task, spawned by `place_head`'s caller beside the head and the publisher. It does for HTTP sends what a session's owner does for its sends (RUNNER-SPEC §3, V1-V5), and it is a publisher client like a session: it sends `Joined` once, before its first forward. It waits only on its inbox, and never waits while handling an event.

**Charges per send** (HTTP-15 rows 16a and 16b; the delivery block, row 16c, is below), each with its own cap and refund:
- **A send record**, one per HTTP send: the record, the send's reply sender and its deadline timer child. At most `relay_sends_max` (1024). Taken when the relay accepts the send; refunded after the send's one Outcome is handed over, once its timer is joined and any `Expired` that timer queued has been taken (Timers, below).
- **A position**, one per forwarded (stream, position): the argument copy, the stream key, the digest, and the outcome reserve of `DONE_MAX` for the head's answer. At most `relay_positions_max` (1024) in total and 1+q (q = 4) per principal. Taken when the relay forwards; refunded only when the head's `Told` for it reaches the relay. A send's `Abandoned`, or its connection closing, never refunds a position: the head may still answer it.
- On acceptance the relay copies the argument bytes into the position, so every byte that can outlive the connection is in the relay's limit, not the connection's.
- Over any cap, the send is `Refused(Overloaded)` (503), and nothing is forwarded.

**The inbox.** Its events are a connection's `Ask(send, reply)`, a timer's `Expired(send)`, and the publisher's `Told`. Its capacity is `http_max + relay_sends_max + relay_positions_max`. Each connection has at most one ask outstanding, because its handler waits on the reply; each send record has at most one timer; and each position at most one `Told`. So no `try_send` to the inbox, from a connection, a timer or the publisher, finds it full. The publisher allocates each `Told`'s payload inside the position's outcome reserve (RUNNER-SPEC F12), as it does for a session.

**Forwarding.** A send of a position the relay does not hold takes a position and is forwarded to the head by `try_send`. A full head inbox is `Refused(Overloaded)` (503): the position and the send record are refunded at once, and nothing was forwarded.

**One Outcome per send.** Each send gets exactly one of:
- a V1 refusal at acceptance (over a cap, a full head inbox, or an expired deadline, tested by one check read at acceptance, §5);
- `Abandoned` when its own deadline timer fires while it is owed;
- a local join answer: a later send of a held position with an equal digest is owed separately and gets the head's answer; one with another digest is answered `Refused(RequestMismatch)` at once, because it reports no commit;
- the head's answer, if the send is still owed when the `Told` arrives (V2).

The relay hands every Outcome by `try_send` into the send's fresh 1-slot reply channel, so handing never waits. A closed channel (the connection ended) makes the hand-over a no-op. Either way the send record is refunded; the position stays until its `Told`.

**Keyed sends** (§4) are Part B. The local join rule above is for `Stream` sends only. A keyed send is forwarded under its own send id with no local decision, and the head decides liveness from the store (K-5). Its response is released by the Release rule below, at its own connection.

**Delivery blocks** (HTTP-15 row 16c). To hand an Outcome over, the relay copies it into a fresh delivery block under its own limit and moves that block into the reply channel; then it drops its own copy. MEM-20 keeps the block's charge on the relay until the connection drops it, which is after its response is written (or at once, when the channel is closed and the `try_send` returns the block). Each connection has at most one ask outstanding, so at most `http_max` blocks are with connections and one is being copied. Only then is the position's outcome reserve free for reuse at its `Told`.

**Timers.** At hand-over the relay cancels the send's deadline timer and joins it; a sleeping timer's join is immediate. The timer's result says whether it had already sent `Expired`. If it had, the send record stays charged until the relay takes that `Expired`, which it ignores, because send ids are never reused. So each live or retiring record has at most one `Expired` queued, and the inbox's `relay_sends_max` share covers every one.

**Order.** The head's answers reach the relay as `Told` from the publisher, in V3/V4 order, exactly as for a session; the relay never takes an answer directly from the head. Each `Told` carries an internal `replayed` bit that is set when the head's answer came from a `Duplicate` admission. The bit is runner-internal: the WebSocket session ignores it, and no wire tag changes.

**Release (RUNNER-SPEC v2.19).** A call response carries only an Outcome, so it passes its connection's writer with no load, as every Outcome unit does (RUNNER-SPEC §3 "The release check"). A duplicate's response is its saved reply, as it was committed. The connection does not join the publisher for it: v2.18 joined only to post `Recheck`, which is gone. A local join's answer and a keyed send's (K-5) are written the same way.

**Disconnects.** A connection that closes or passes its deadline drops its `Receiver`. Its send stays owed in the relay until its deadline or the head's answer. Execution is the head's and is never cancelled by HTTP.

**The relay's limit** is `relay_bytes` = T(0) + its inbox + rows 16a, 16b and 16c at their caps, so a timer being joined and a block being copied are inside it. So repeated expiry, disconnect and reconnect while the head is delayed hold at most `relay_positions_max` positions and `relay_sends_max` send records (law L15).

**Delivery failure after commit.** Each of these changes no durable state:
- The connection closes, or the output unit's deadline passes. The commit stands.
- A write that fails or passes its unit deadline after some bytes were sent leaves a partial response, and closes the connection (HTTP-7). A short `Ok(n)` is progress, not this.

**Recovery.** The client re-sends with the same identity and gets the head's `Duplicate`, which holds the same reply bytes, with `Idempotent-Replayed: true`.

### B-9: SSE as a subscription sink

**The request.** `GET` with `Accept: text/event-stream` on a `Subscribe` binding returns `Stream` with a subscription source (§2).
- The stream group joins the publisher as its own session and subscribes through the same path as a WebSocket `Subscribe`: PubIn, and frames funded by the reader (arrangement (a)). It holds the session `budget` (HTTP-15 row 14).
- **Pre-head** (§2). The group waits for the first Batch whose `complete` lists its subscription, an empty result included, under the pre-head deadline. Nothing is written before it. Then the head and that Batch's event are written, and the group is committed.
- An `Ended` before that is answered with B-7's status for its Outcome, as an ordinary response: `Ended(sub, Refused(Budget(Room)))`, the oversized initial result (RUNNER-SPEC §3 frame bounds), is 503 `{"refused": "budget", "requested": "<u64>", "available": "<u64>"}` with no `Retry-After`. A passed pre-head deadline is HTTP-5's 503.
- `retry: 1000` is written once, after the head. No event carries an `id`, and a `Last-Event-ID` request field is ignored.

**Events.** Each frame the subscription delivers becomes exactly one SSE event, with WIRE §3's full content, `sub` included:

| Frame | SSE event | `data` (one line of JSON) |
|---|---|---|
| `Batch(version, complete, rows, outcomes)` | `event: batch` | `{"version": "<u64>", "complete": [<u32>…], "rows": [<Row>…], "outcomes": []}` |
| `Ended(sub, outcome)` | `event: ended` | `{"sub": <u32>, "outcome": <Outcome>}`; then the stream ends |
| `Reset(version, outcomes)` | `event: reset` | `{"version": "<u64>", "outcomes": []}`; then the stream ends |

**A row is a tagged envelope**, never `V | null`. WIRE's `Row { sub, key, value: Option[Bytes] }` is written as:
- present: `{"sub": <u32>, "table": "<name>", "key": <K>, "present": true, "value": <V>}`;
- removed: `{"sub": <u32>, "table": "<name>", "key": <K>, "present": false}`, with no `value` member.

So a present row whose value is itself `null` (a V of `()`, `Option[T]`, or a nullable `via` type) is distinct from a removal. Keys and values are written in B-4's forms, from the Cover's table schemas in describe.

- `outcomes` is always empty: an SSE stream carries no sends, so it can owe no Outcome. The member is kept so that one decoder reads WebSocket and SSE envelopes.
- The completing initial Batch, an empty one included, is the event whose `complete` lists the subscription. A later event whose `complete` lists it is a repair (RUNNER-SPEC v2.20): the client replaces the subscription's image from empty, as for the first.

**Encoding.** The writer encodes each event in pieces from the frame, through `out_cap`. JSON escapes CR and LF, so the data is one line, and the event's blank line is written last, so a cut event is discarded (HTTP-13). The event's length is bounded by B-5's analysis over the frame schema, `a·n + b` for a frame of n bytes, n ≤ `max_frame`; its memory is the frame, already on the `budget` ledger.

**Control is independent of data.** Batches wait for the writer. An `Ended` or a `Reset` takes the control slot (§2), and is written as soon as the writer finishes its current unit, under its own unit deadline. If that deadline passes, the stream closes without it, and the client sees no partial event (HTTP-13).

**Slow readers.**
- A slow reader stalls the writer. When the group's owner inbox is full, the publisher ends nothing: it marks the subscription for repair and, once the owner has made progress, sends one whole replacement as a `batch` event on the open stream (RUNNER-SPEC v2.20). Rows over the group's `budget` still reset under W-D3, unchanged.
- On a Reset the owner writes `event: reset` as above if the writer can deliver it in time, and the stream ends either way. The client re-requests and gets a fresh subscription with its full state.

**Resume.** There is no version-only resume.

### B-10: Generated router, client and OpenAPI

The generator runs `place` (D1) and reads the entries and the bindings from describe. From them it builds three things.

**The router.**
- One generated handler per binding: B-4 decoding, then B-2's `Ingress`, `prepare` and reservation, then the forward, then B-5's transcoder.
- One alias route per durable `op` (B-12).
- These handlers are added to the same router builder as the raw routes.

**The TypeScript client.**
- One function per binding. `Stream` bindings call the `op` alias route (B-12), so a route rename never breaks a retry.
- `Stream` bindings draw (stream, position) from the WebSocket SDK's durable install store (WIRE-SPEC F6). So one installation never reuses a position across transports, and a pending call after a crash is re-sent with the same bytes under the same identity.
- Maps are pair arrays (B-4). Rows are B-9's tagged envelopes.

**OpenAPI 3.1.**
- JSON Schema for each B-4 form, and for B-9's row envelope.
- The `Idempotency-Key` and `Authorization` parameters, and every B-7 response.
- Keyed bindings' claim header names and `x-hirsel-retention-ms` are Part B (§4, K-1, K-6).
- It is built once at `place` from the registry, so it is deterministic. It is served as immutable bytes at `GET /.hirsel/openapi.json`.

### B-11: One-shot reads

A query entry returns an opaque `Cover`, not a codec value (RUNNER-SPEC rule 2, §4), so a one-shot read is a transient subscription.

- **The slot.** The generated handler first asks the supervisor for a read slot (`ReadSlot`, answered on the connection's 1-slot reply channel). At most `reads_max` are held; over it the answer is 503 `Refused(Overloaded)`.
- **The read.** The handler joins the publisher as its own session, with a retirement mark as §2's groups have (paid in its scope), subscribes through PubIn as B-9 does, and waits for the completing initial Batch on its own inbox, under `Request.deadline`.
- **The response** is 200 with one JSON object, the `batch` envelope of B-9's table, rows in B-9's tagged form: version, complete, rows and empty outcomes. It is written from the frame by the transcoder, as B-9 writes an event.
- **Payer.** The read's session record and its inbox are in the handler's scope (`handler_bytes`, HTTP-15 row 8); the delivered frame's rows are on the publisher's ledger (row 14a), as for a session.
- **The end.** The handler drops its inbox `Receiver` and its retirement mark's `Sender`, `try_send`s one `Drained`, and then sends `ReadDone` to release the slot. Nothing here waits on the publisher. A read that fails or is cancelled drops its mark mechanically too, and if the connection task ends holding a slot, the supervisor releases the slot at that task's end. After it joins that task, the supervisor `try_send`s one `Drained` (RUNNER-SPEC v2.14, CHANGES-v2.14 item 3). The publisher retires the session at its next sweep (§2 Publisher retirement), without any later traffic.
- **Consistency.** The rows are exactly the Cover's state at `version`, the publisher's base when the Subscribe ran (RUNNER-SPEC §4, G3).
- **Release (RUNNER-SPEC §3 "The release check", v2.19.2).** The handler joins with its Caller, like any session, and its writer applies RUNNER-SPEC's check. **The unit** is the 200 head with its whole body, a content unit at the version of its rows. A B-7 error response made from a publisher-made `Ended` is a content unit at that `Ended`'s version. A fixed status with no data passes unchecked. A unit that is dropped writes nothing of itself, and the answer is 503 `{"refused": "reset"}` with `Retry-After: 1`, so the client re-requests. A head that passed is written with its body, whatever commits meanwhile. An SSE group's writer checks each event the same way, an `ended` event included. A dropped event ends the stream with `event: reset`, as the owner's `Reset` on `Dropped` does (RUNNER-SPEC §3).
- **Bounds.** The rows allowance is the WebSocket one (RUNNER-SPEC §3 frame bounds), so equal reads give equal answers over both transports. An oversized result is the 503 of B-9.
- **Failure.** An `Ended` is answered with its B-7 status, checked as above, and a passed `Request.deadline` with HTTP-5's 503; both then end as above.
- **describe** exports a query binding's result schema as this envelope over the Cover's tables. It does not pretend `Cover` has an ordinary B-4 form.
- A `Read` binding of a compute entry returns R, through B-5.

### B-12: Stable operation IDs

- Every binding has an `op`: 1-64 bytes of `[a-z0-9_.-]`, unique in the registry, chosen by the program and independent of its route.
- **Durable identity uses `op`, never the route.** A route can move without changing any identity.
- **The alias route.** `POST /.hirsel/op/{op}` reaches every `Stream`-mode binding with the same fields: path fields move into the query. The generated client always uses it. Keyed and `Retired` bindings (Part B) are reached by the same alias, and their claim function sees the same headers and body there as on the route (K-2).
- **Removed keyed bindings** (Part B) stay answerable through a `Retired` binding and the store's op record until their claims expire (K-9).
- **Removed `Stream` bindings.** An `op` in the activation table's `previous` bindings but not in `current` stays routed through the alias. Its calls are gone calls (B-2 step 3): `Failed(Gone)` for a new identity, the stored reply for a retried one. The reply is rendered from `previous`'s describe bytes, which hold the entry's result schema, so a removed entry's reply stays decodable. After the next deploy drops `previous`, the op is unrouted (404) and its stream identities are the gone-key set's (RUNNER-SPEC §6).
- **Rendering.** A durable reply is stored as the runner's `Outcome`. A `Stream` binding's `ok_status` and `error_status` are fixed per `op`: changing them requires a new `op`, which `place` checks against `previous`. So a retry always gets the original status.
## 4. Keyed operations

**Part B, unbuilt.** This section is specified but not built; slices H9 and H12 build it (§7). Part A neither builds nor relies on it.

**Hirsel does not authenticate.** Neither Hirsel nor the runner checks a credential, a signature or a secret for a keyed operation: the application's own claim function (K-2) decides who the principal is, exactly as `Wire.bind` decides the Caller ([RUNNER-SPEC](../../core/programs/store/RUNNER-SPEC.md) v2.24 rule 13, ruling R6).

**Purpose.** Some clients bring their own retry key, an opaque string rather than a (stream, position) pair: webhook delivery IDs, payment-style `Idempotency-Key`s, and upstream message IDs. A keyed operation runs a transact entry at most once per **claim** (`op`, principal, key) while the claim is live (K-5).

**What it shares with streams, and what it adds.** It shares the admission logic of `admit` (store.hirsel:1449): a durable admit before the body runs, one worker per identity, a digest check, and a stored reply. It adds a store control extension (K-4), expiry (K-6), quotas (K-7) and a recovery queue (K-8). It is not the stream machine unchanged.

### 4.0 Prior art

**Stripe, API v1** (docs.stripe.com/api/idempotent_requests). Current Stripe documentation distinguishes API v2 behaviour; everything below is API v1.
- **What it does.**
  - Keys are up to 255 characters.
  - The first response is saved once execution begins, 500s included.
  - Parameters are compared with the original's, and a mismatch is an error.
  - Validation failures and concurrent conflicts are not saved, so they can be retried.
  - Keys are pruned after at least 24 h, and a reused key after pruning "generates a new request".
- **What Hirsel does with it.**
  - **ADOPTED:** the 255-byte bound, saving the first outcome, the parameter check, and not recording refusals.
  - **REPLACED:** the "at least 24 h" pruning. Retention here is an exact, declared contract per binding (K-6).

**Restate** (github.com/restatedev/restate)
- **What it does.**
  - Ingress parses `idempotency-key` (crates/ingress-http/src/handler/service_handler.rs:183) and applies a retention, by default 24 h (crates/types/src/schema/invocation_target.rs:29, :107).
  - The invocation id is a hash of target, scope and key (crates/types/src/identifiers.rs:367-392).
  - A duplicate of an in-flight invocation attaches a response sink, so it joins. A duplicate of a completed one gets the stored result (crates/worker/src/partition/state_machine/mod.rs:1096-1176). Completion is retained after execution (mod.rs:2923-2934).
- **Its weakness.** That dedup path compares no arguments. A reused key with other arguments silently gets the first call's result.
- **What Hirsel does with it.**
  - **ADOPTED:** deterministic identity from (scope, target, key), joining the in-flight call, and retention counted from completion.
  - **REPLACED:** the dedup without an argument comparison. Hirsel answers 422 on a digest mismatch for a live claim (K-5).

**Temporal** (Temporal documentation, Workflow Id and Run Id)
- **What it does.**
  - Reuse policies apply only to closed runs "retained within the Namespace" (:105-118). Past retention, Reject Duplicate admits a new run.
  - The conflict policy for an open run is Fail or Use Existing (:128-146).
- **What Hirsel does with it.**
  - **ADOPTED:** retention bounds the claim, stated as the contract (K-6).
  - **NOT TAKEN:** a per-call policy choice. Hirsel has one behaviour: join in-flight, return completed, refuse a mismatch.

**AWS Lambda Powertools** (Powertools for AWS Lambda, Python: aws_lambda_powertools/utilities/idempotency)
- **What it does.**
  - Records expire after a configurable time, 1 h by default (config.py:17).
  - The payload check is opt-in, through `payload_validation_jmespath` (persistence/base.py:143, :267).
  - An in-flight record raises `AlreadyInProgressError` (base.py:256-271), and its in-progress lease is the Lambda's remaining time (persistence/base.py:386-393).
  - Claiming is one conditional put: "not exists, or expired, or in-progress expired" (persistence/dynamodb.py:215-230).
- **Its weaknesses.**
  - The payload check is off by default.
  - A wall-clock lease lets a live but slow execution be duplicated once its lease ends.
- **What Hirsel does with it.**
  - **ADOPTED:** the one conditional claim, which here is one store commit.
  - **REPLACED:** the opt-in payload check (the digest check is always on) and the in-progress lease (K-8's recovery queue instead).

**Standard Webhooks** (github.com/standard-webhooks/standard-webhooks, spec/standard-webhooks.md, "Signature scheme"; cited as a document)
- **What it does.** `webhook-id` is the idempotency key, and `webhook-signature` is an HMAC over `id.timestamp.body` under the shared secret; receivers check the timestamp against a tolerance.
- **What Hirsel does with it.** Nothing built in. An app writes it as a claim function (K-2, first worked example) that checks the signature with `secret.verify_hmac_sha256` and the timestamp against its own tolerance, and returns the verified `webhook-id` as the key.

**GitHub webhooks** (docs.github.com, "Validating webhook deliveries"; cited as a document)
- **What it does.** `X-Hub-Signature-256` is an HMAC-SHA256 over the body alone. `X-GitHub-Delivery` names the delivery, and it is not signed; there is no signed timestamp.
- **What Hirsel does with it.** Nothing built in. An app writes it as a claim function (K-2, second worked example), and the weaker guarantee that example states is the app's to accept.

### K-1: The declaration

`Mode.Keyed(KeyedRule[G])` binds a transact entry (B-3). `Mode.Retired(Retirement[G])` keeps a removed keyed `op` answerable until its claims have expired (K-9). The binding's `op` (B-12) scopes its claims. `G` is the registry's grant type, so a claim function gets the place's wire grant (K-2).

```hirsel unbuilt
pub type ClaimRequest: send { headers: List[(String, Bytes)], body: Bytes }
pub type Claimed: send { caller: Caller, key: Bytes }
pub type Claim[G: send]: send {
  f: StaticFn[fn(G, ClaimRequest) -> Result[Claimed, ()] waits],   // K-2; application code
  headers: List[String],   // the header names `f` receives, lower-case
  claim_bytes: u64,        // bounds `f` as `runner.wire`'s bind_bytes bounds `Wire.bind`
  claim_steps: u64         // and as its bind_steps
}
pub type KeyedRule[G: send]: send {
  claim: Claim[G],         // K-2
  retention_ms: u64,       // K-6; required, no default
  keys_max: u64,           // K-7; claims per op
  principal_keys_max: u64  // K-7; claims per (op, principal)
}
pub type Retirement[G: send]: send { claim: Claim[G] }   // the same claim function and header names as the op's record
```

`bind` refuses:
- a `retention_ms` of 0 or above 2^53, or not above the binding's `deadline_ms` (K-6 measures retention from the reply run's start);
- a `keys_max` of 0, or a `principal_keys_max` of 0 or above `keys_max`;
- a claim header name that is not a lower-case RFC 9110 token, or a name listed twice;
- a `Retired` binding with an `entry`, `fields` or statuses (it has none: its argument schema, field mapping and statuses are the op record's, K-9). `bind` runs before any store is open, so whether its route's captures are the record's `Path` fields is checked at open (below).

A place with a keyed or retired binding and no wire grant (`PlaceConfig.wire` is `None`, or a `runner.wire_bind` wire, which has no grant half) refuses the open with `KeyedNoWire(op)`, because its claim function has no `G` to run with.

**The op record.** A keyed `op`'s policy is fixed at its first admission and kept in the store (K-4 `keyed_ops`): the entry key, the argument schema and the result schema (from describe's `Described.args` and `Described.result`), the field mapping (each field's `FieldSource`: path capture name, query name or body member, in declared field order), the method, `ok_status` and `error_status`, `retention_ms`, `keys_max`, `principal_keys_max`, `arg_max`, the entry's byte and step bounds, `deadline_ms`, and the claim function's **identity bytes** (PROG-9) and header names. It holds no secret and no credential. It never changes. The route is not in it, so a route can move (B-12); a route's captures must still name the record's `Path` fields. At open, the head compares every bound keyed or retired `op` with its record (for a `Retired` binding: method, the claim function's identity bytes and header names, and that its route's captures are exactly the record's `Path` fields); any difference refuses the open with `KeyedOpChanged(op, field)`, and the way to change a policy is a new `op`.

**Why identity bytes and not the code address.** The record must catch a binding that now derives the principal or the key from another function or other inputs, because then a live claim's retry would get another claim and run again. Identity bytes catch that: another function, a rename or a changed signature is another identity. The code address (PROG-10) also changes on every edit to any code the function reaches, std's `hmac` and `base64` included, and on a compiler that lowers a reached body differently. Pinning it would refuse the open after a toolchain upgrade for every keyed op with live claims, and a `Retired` binding, which must run the same function, could not survive any fix to that code. Entries are matched the same way: an entry key holds the identity and the schema fingerprints, not the revision (launch.md), and `Wire.bind` is not pinned at all. A body edit that changes what the function means is the application's to name: it renames the function or binds a new `op`.

### K-2: The principal and the key come from application code

A claim is scoped by its principal. The application decides the principal and the key, in its claim function; Hirsel does not authenticate (RUNNER-SPEC v2.24 rule 13). The claim function is `Wire.bind` for a keyed binding: the same kind of value, run at the same point, under the same rules (RUNNER-SPEC §3, "Binding"), and this rule states only what differs.
- **Its input.** A copy of the place's wire grant `G`, and a `ClaimRequest`: each received header field whose name, compared case-insensitively, is in `claim.headers`, in arrival order, with its name lower-cased and its value as received (so the function sees a repeated header and decides what to do with it); and the body bytes, as read under `body_max`. `Wire.bind` gets the `Authorization` value alone; a keyed binding that wants it lists `authorization`.
- **When.** Once per request, before `prepare`, inside `bound bytes claim_bytes` and `bound steps claim_steps`. It runs in place of `Wire.bind` for that request: a keyed binding's Caller is the claim's, and `Wire.bind` is not called.
- **Its answer.** `Ok(Claimed{caller, key})` makes the claim (`op`, caller, key). The caller follows RUNNER-SPEC's Caller rules (an issuer from the fixed table, a subject of at most `SUBJECT_MAX` = 64 B), and the key K-3's form. `Error(())`, a fault or an exhausted bound gives 401 and records nothing, as a refused `Wire.bind` does.
- **Required.** A keyed or retired binding always has a claim function. Without one, each request would get a fresh anonymous Caller (RUNNER-SPEC v2.24), so no claim would ever repeat.
- **What the runner does not do.** It reads none of the headers or the body for identity, holds no secret, keeps no list of senders or tenants, and compares no signature. Constant time is the primitives' guarantee: `secret.verify_hmac_sha256` compares inside the runtime and returns only the verdict, and `hmac.equal` compares in time that depends only on the lengths ([hmac](../../core/std/api/hmac.md)). The runner trusts the claim's answer, as it trusts `Wire.bind`'s.
- **Time.** A function that checks freshness reads `host.wall_ms` through a `Clock` in its own `G`. That read passes the replay point through the grant (CAP-8), like any read of application code; the runner takes no clock read for it (§5).
- **Replay.** The function is program code, and its secret operations are provider operations whose verdicts are recorded (hmac "Replay"), so a replay computes the same claim.
- **The route and the alias give the same claim:** the function sees the same headers and body on both (B-12).
- **Tenants.** A tenant is a Caller the function chooses. A tenant id that the function reads from a header or the body may select which of its secrets to check; the signature, not the id, is the authority.

**Worked example: Standard Webhooks.** The app's grant holds one `Secret` per sender, granted for `verify` only (`--grant 'Secret:hook=env:WEBHOOK_KEY verify'`, hmac), and a `Clock`. The binding lists `webhook-id`, `webhook-timestamp` and `webhook-signature`.

```hirsel unbuilt
type Hooks: send { hook: Secret, clock: Clock }
fn claim_hook(g: Hooks, r: ClaimRequest) -> Result[Claimed, ()] waits
```

1. Take exactly one of each listed header; otherwise `Error(())`. The timestamp is decimal digits, at most 19, parsed as u64; otherwise `Error(())`.
2. Build `id.timestamp.body` from the raw bytes. For each space-separated `v1,<base64>` entry of the signature, decode it with `base64.decode` and ask `secret.verify_hmac_sha256(g.hook, data, tag)`. No `Ok(true)`, or a `Denied`, gives `Error(())`.
3. With `now_s` = ⌊`host.wall_ms(g.clock)` / 1000⌋, require `max(now_s, ts) − min(now_s, ts) ≤ 300` (the tolerance is the app's), which cannot overflow; otherwise `Error(())`.
4. Answer `Ok(Claimed{caller: Caller{issuer: <the app's issuer>, subject: "hook"}, key: webhook-id})`.

**Guarantee:** the key is signed with the body and the time, so a delivery replayed inside the tolerance is a duplicate, and one outside it is refused. With several senders, `G` holds a secret per sender, and the function picks one by a selector it chooses (a header it lists) or tries each in turn; the subject names the sender, so senders' claims never meet.

**Worked example: GitHub.** The binding lists `x-hub-signature-256` and `x-github-delivery`. The function takes exactly one of each, requires `sha256=` and 64 lower-case hex digits, decodes the hex, asks `secret.verify_hmac_sha256(g.hook, body, tag)`, and answers the delivery id as the key; anything else is `Error(())`. There is no timestamp check, because GitHub signs no timestamp.

**Its guarantee is weaker, and accepting it is the app's choice:** the body is authenticated, but the key and freshness are not. Someone who has seen one delivery can resend its body under a new delivery id, and that is a new claim that runs again. An entry bound this way must therefore be safe to run twice for one body, or must dedup on the body's own content. An app that states this to its clients writes it in its own API description.

**An API client's `Idempotency-Key`.** The binding lists `authorization` and `idempotency-key`. The function maps `authorization` to a Caller exactly as the place's `Wire.bind` does (the app can call the same function), and answers the `idempotency-key` value as the key.

### K-3: The key

- The key the claim function answers is 1-255 bytes of visible ASCII (0x21-0x7E). Anything else, the empty key included, gives 400 and records nothing.
- So a function that accepts the request but finds no key answers the empty key (400), and one whose key is part of what it verifies answers `Error(())` (401).

### K-4: The store control extension

Keyed claims live in the store's **control section**, beside `stream_ops`, not in application tables. This is an explicit extension of STORE F2 (store.hirsel:811-918), the journal and checkpoint roots, and the call record.

**Commit rules (F2 extended).**
- A body's run that succeeds commits its application writes, the claim's reply and the claim's control writes together.
- An `Error` (`Aborted`) or a `Failed` run commits a **control-only** commit: the reply, the expiry index entries, the counts and the call record are kept; application writes are dropped, so the commit holds zero application effects. Today's F2 keeps only `stream_ops` on `Error` (store.hirsel:867); the extension keeps the keyed control writes the same way.
- `record` (store.hirsel:1412) accepts a reply for a keyed claim in the control write set, as it accepts a stream reply today. The call record identifies a claim by (`op`, caller key, key) in place of (stream, position).
- The journal's section encoding and the checkpoint's roots gain the keyed trees. Existing stream keys and their encodings are unchanged, and AE1-AE3 and AH1-AH3 are untouched.

**Trees.** Wherever a key below has `caller`, it holds the caller key `u8 issuer ‖ len8 ‖ subject` (RUNNER-SPEC v2.15), at most 66 B because the subject is at most 64 B. It is the Caller the claim function answered (K-2), so no credential byte is stored. Lengths are bounded before encoding: `op` ≤ 64 B (B-12), the caller key ≤ 66 B, key ≤ 255 B. Write ‖ for concatenation and len8 for a one-byte length.

| Tree | Key | Value |
|---|---|---|
| `keyed_ops` | len8 ‖ op | the op record (K-1), ≤ `op_record_max` (16384 B, both schemas included; `bind` refuses a larger one) |
| `claims` | len8 ‖ op ‖ caller ‖ len8 ‖ key (≤ 65 + 66 + 256 = 387 B) | state u8 (1 Admitted, 2 Replied) ‖ request digest (32) ‖ expires_ms u64 (0 while Admitted), a 41 B header; while Admitted: the argument bytes (≤ `arg_max`); while Replied: the scope digest (32, RUNNER-SPEC v2.17 B2), so a 73 B header, then the reply (≤ `DONE_MAX`) |
| `claim_exp` | len8 ‖ op ‖ u64-BE expires_ms ‖ the claim key (≤ 460 B) | empty |
| `claim_pexp` | len8 ‖ op ‖ caller ‖ u64-BE expires_ms ‖ the claim key (≤ 65 + 66 + 8 + 387 = 526 B) | empty |
| `claim_count` | len8 ‖ op | u64: live-or-unreclaimed rows of this op |
| `claim_pcount` | len8 ‖ op ‖ caller (≤ 131 B) | u64: live-or-unreclaimed rows of this (op, caller) |
| `claim_open` | the claim key | empty: present exactly while the row is Admitted |

- **The digest** is `request_digest(entry key, argument bytes)` (B-2), the same function as for streams.
- **Statuses, the result schema and the run bounds** are the op record's, one copy per op, so a retry renders the original status and body even after the binding or its entry changed or was removed (K-9).
- **The op record** is written in the commit of the op's first admission, and deleted by the orphan sweep (K-6) once the op is unbound and has no rows.
- **Counts** are deleted when they reach 0, so count rows exist only for ops and principals with rows.
- **What the row omits.** It carries no field that only checks the store against itself.
- **Keyed time.** The extension stores no clock of its own. Keyed time is the `at` of the store run on main that makes the decision: the store sets each run's `at` to max(its one `host.wall_ms` read, the branch's last committed `at`), writes it in the commit's frame and checkpoint, and restores it at reopen (STORE §1.7). So `at` never decreases across commits, restarts and wall-clock rollback. A run that writes nothing need not commit. A decision that judges a row expired either deletes or replaces that row in the same commit (admission, reclaim, the orphan sweep) or, where it leaves the row (K-9's open check), commits its run, so the `at` it relied on is durable before anything depends on it. K-5's expiry test, K-6's expiry, reclaim and orphan sweep, K-8's replies and K-9's open check all read it; K-1, K-2, K-3 and K-7 do not (a claim function that checks freshness reads its own `Clock`, K-2).

### K-5: Admission

**Liveness.** `NEVER` = 2^64 − 1 is a reserved `expires_ms` meaning "does not expire" (K-6). A Replied row is **expired** when `expires_ms ≠ NEVER` and `expires_ms ≤ at`, where `at` is the keyed time (K-4) of the run that decides; this is the one expiry test, used by admission, both reclaim indexes, the orphan sweep and K-9's open check. Because committed `at` never decreases, a row once judged expired by a committed decision is expired at every later decision. A claim's row is **live** when it is Admitted, or Replied and not expired. An expired row is **absent**, whether or not it has been reclaimed. Every rule below reads liveness first, and only the store's row decides it.

**The function.** `store.admit_claim(mut db, claim, d, quotas) -> Result[ClaimAdmission, Error] waits`. It decides inside one store run on main and reads that run's `at`; it takes no clock read of its own.

**The cases**, checked in this order:

| Row | Answer |
|---|---|
| Absent (no row, or an expired one) | Reclaim (K-6). Then, if `claim_pcount ≥ principal_keys_max` or `claim_count ≥ keys_max`, `Error(Full)` (503), with nothing committed. Otherwise the store's resident admission (K-7) must also accept the row and its terminal reserve; if it does not, `Error(Full)`, with nothing committed. Otherwise commit the row as Admitted with d and the arguments, add `claim_open`, add 1 to both counts (an expired row being replaced is one delta: its count, index and open entries are removed and the new ones added, a net 0 on the counts), take the terminal reserve, and execute. |
| Live, digest ≠ d | `RequestMismatch` (422). |
| Replied (live), digest equal | `Duplicate(Stored{reply})`: the op record's status and the stored body, unchanged, plus `Idempotent-Replayed: true` (RUNNER-SPEC v2.19, ruling 73 as amended). |
| Admitted, with a live worker in this open | The send joins the worker if its digest equals the worker's (H1); at its own deadline it gets `Abandoned` (409). |
| Admitted, with no live worker | **Ownerless** (K-8). The send starts a worker and re-runs the body, or joins the recovery run if one holds the claim; an earlier attempt never committed its body, because its effects and its reply commit together (F2). |

**The head.** The head treats a claim key like a stream key for H1-H3: one worker per live claim, and a same-digest send joins (RUNNER-SPEC L21). Its worker table is the record of which Admitted rows have a live worker.

**Keyed sends through the relay.** The relay makes no liveness or digest decision for a keyed send: it holds no local join for keyed sends, and forwards every one to the head under the send's own **send id**. It is charged as every HTTP send is: a send record (B-8 row 16a, retired as B-8's Timers says), a position (row 16b), and a delivery block for its Outcome (row 16c). The head answers each send by its send id, through the publisher's `Told`, as it answers a stream send by (stream, position). Each send's response is written at its own connection by B-8's Release rule, whatever another send of the same claim was answered. So:
- a send that joins a worker gets that worker's outcome, and a send admitted as a new claim gets the new claim's;
- when a claim expires while its old reply's delivery is still delayed, a retry with other arguments is admitted as a new claim (the row is absent), and the old send still gets the old outcome, because answers follow send ids, never claim keys. No generation field is stored: the send id carries the identity of the delivery.

**Recording.** Refusals before admission (the claim function's 401, K-3, B-4 and `prepare`) record nothing. `Full` records nothing.

### K-6: Retention, expiry and reclaim

**Expiry time.**
- A claim's expiry is set from the `at` of its reply commit (K-4), the same keyed time the expiry test reads. That `at` is fixed when the run starts, so for a success it precedes the reply by at most the run's `deadline_ms`, which K-1 keeps below `retention_ms`: a reply stays live at least `retention_ms − deadline_ms` after it commits. `expires_ms` = `at` + `retention_ms` when that is below 2^64 − 1, and otherwise `NEVER` (K-5): an `at` at or above 2^64 − 1 − `retention_ms`, `u64::MAX` itself included, gives a claim that never expires. A clock that far ahead is broken, and the policy errs towards a live claim (no second execution), never towards a wrapped or already-expired one; such a claim keeps its op's record and rows, and K-9's open check reports it as `KeyedOpDropped(op, Never)`. Since the reply commit's `at` is at least every earlier decision's, a reply is never born expired, also after a wall-clock rollback. No other keyed arithmetic adds to `at`. In `claim_exp` and `claim_pexp`, `NEVER` sorts after every finite expiry, so an index front reaches it only when no expired row is ahead of it, and a `NEVER` front is live.
- The reply commit writes `expires_ms`, the `claim_exp` and `claim_pexp` entries, and removes `claim_open`.
- An Admitted row never expires: there is no in-progress lease.

**What a claim promises.**
- A claim is live from admission until `retention_ms` after its reply run's `at`. After that the key is free, and a request with it is a **new operation**. Stripe, Restate, Temporal and Powertools all end the same way.
- The contract is written into OpenAPI (`x-hirsel-retention-ms`). A binding must set `retention_ms` at least as long as its sender's retry horizon.
- A tombstone would only move the cliff, at the cost of a row per key, so there is none.

**The clock.** Retention is wall-clock time, because it has to survive restarts, read through the nondecreasing keyed time (K-4). Moving the wall clock backwards does not move keyed time: rows already judged expired stay expired, rows not yet judged keep their expiry, and replies written meanwhile measure retention from the floor, so the rollback lengthens retention by its size. Moving it forwards shortens retention, and for good: one commit at a far-future reading raises `at` for every later decision, expiring finite rows early and giving later replies `NEVER` once `at` passes 2^64 − 1 − `retention_ms`. Every body's `at` (CallInfo) already carries that cost. Both are the stated cost; neither lets a claim judged dead come back.

**Reclaim**, inside each admitting commit, in each scope whose count is at its quota:
1. If `claim_pcount` is at `principal_keys_max`: delete up to 2 expired (K-5) entries at the front of `claim_pexp` for this (op, principal), with their rows, their `claim_exp` entries and both counts.
2. If `claim_count` is then at `keys_max`: delete up to 2 from the front of `claim_exp` for this op the same way.

**Progress.** A full scope whose index front is expired always frees a row in its step. The admission then proceeds exactly when both quotas and the store's resident admission (K-7) are satisfied. `Full` is answered when a scope at its quota has a live index front (every row in it is live), or when resident admission refuses. A full per-principal scope of live rows refuses even when the op has expired rows elsewhere; that is the per-principal quota working.

**Orphans.** An `op` that is neither bound (`Keyed` or `Retired`) has, by K-9's open check, no live row, so all its rows are expired. The head retires them with a bounded cursor traversal of the store's own trees; it keeps no list of orphan ops.
- **The cursor** is one in-memory position (op ≤ 65 B, a `claim_exp` position ≤ 460 B), in the head's fixed state. It walks `keyed_ops` in key order and, inside an orphan op, that op's `claim_exp` range.
- **A sweep** runs after each group and at each recovery tick (K-8). It is one store run on main, judges expiry at that run's `at` (K-4) and commits only when it deletes; it examines at most 3 entries in all, records and index entries counted alike: a `keyed_ops` record whose op is bound (checked against the registry, already in memory) is passed over at the cost of one examined entry; in an orphan op it deletes up to 2 expired rows, each with a fixed set of mutations (its `claims` row, its `claim_exp` and `claim_pexp` entries, and its two count updates or deletions); an orphan op with no rows left has its counts and record deleted. At the end of `keyed_ops` the cursor wraps to the start. It restarts at open.
- **Its commit** is admitted like any commit, against `resident_limit − db.reserved`; a refused sweep commit changes nothing and leaves the cursor where it was.
- **Completion.** A full pass over N op records, of which the orphan ops hold r_o rows each, takes at most N + Σ_o (⌈r_o / 2⌉ + 1) sweeps whose commits the store admits, so every orphan row is deleted within that many sweeps of the pass's start. Bound ops cost one examined entry each per pass, never more.

### K-7: Cardinality and bytes

**Quotas.** At most `keys_max` live-or-unreclaimed rows per op, and at most `principal_keys_max` per (op, principal). A binding with one tenant sets them equal. Both counts change atomically in the commit that changes the rows.

**Bytes.** With E(k, v) = 62 + round8(k) + round8(v), the store's per-entry charge (store.hirsel:1164), one claim costs at most:
- `claims`: E(387, max(41 + `arg_max`, 73 + `DONE_MAX`)) (RUNNER-SPEC v2.17 M2: an Admitted row has a 41 B header, a Replied row 73 B);
- `claim_exp` and `claim_pexp`: E(460, 0) + E(526, 0) (RUNNER-SPEC v2.16 M2: `claim_pexp` nests the caller key twice, 65 + 66 + 8 + 387 = 526);
- `claim_open` while Admitted: E(387, 0);
- and per op and per (op, caller) with rows: E(65, 8) and E(131, 8); per op, its record E(65, `op_record_max`).

A binding's resident bound is `keys_max` × (the claim cost) + E(65, 8) + E(65, `op_record_max`) + `keys_max` × E(131, 8). The sum over bound ops counts against the store's `resident_limit`.

**The terminal reserve.** At admission the claim takes a reserve **R_t** that pays for its whole terminal transition, so that the control-only reply commit can never be refused for room:
- the row's growth: none, because the Admitted row is charged at max(41 + `arg_max`, 73 + `DONE_MAX`) from admission;
- the two expiry index entries, E(460, 0) + E(526, 0) (v2.16 M2; `claim_open`'s removal frees E(387, 0), not counted against it);
- the call record and the reply commit's journal frame, at their bounds for one claim: `frame_max(claim)` = the frame header + the call record (≤ `call_record_max`) + the control delta's writes, each at the journal's per-write overhead plus its key and value bounds: the `claims` row (387, 73 + `DONE_MAX`), the `claim_exp` and `claim_pexp` inserts (460 and 526, empty values; v2.17 M2 corrects 525) and the `claim_open` delete (387);
- the retained-root peak: the control delta is exactly 4 mutations (`claims`, `claim_exp`, `claim_pexp`, `claim_open`); the call record is a frame section, not a tree (store.hirsel:870). One std `omap` insert or remove copies its path and may split, merge or redistribute one node per level (api/omap.md), so it allocates at most 2h + 1 nodes, where h, the height bound, is 1 + ⌈log₁₆ n_max⌉ (non-root nodes hold at least 16) with n_max = `resident_limit` / 70, since no entry charges less than E(1, 0) = 70, orphan rows and every other table included. A node charges at most N = 104 + 32 × (24 + 8) + 32 × round8(k_logical), its header, its slots and its cloned keys (api/omap.md), with k_logical ≤ 537 the longest logical key of the keyed trees (v2.17 M2: 526 + 11). So the peak is 4 × (2h + 1) × N; these copies stay resident while a snapshot retains the old root. The terminal delta changes no count, and a success's application writes are not in it.

The store keeps `db.reserved`, the sum of R_t over Admitted claims. Its resident admission tests every commit against `resident_limit − db.reserved`, except a claim c's own terminal commit, which is tested against `resident_limit − (db.reserved − R_t(c))`: it may spend its own reserve and nothing of any other claim's. That commit subtracts R_t(c) from `db.reserved` atomically with the reply; a refused commit leaves R_t(c) reserved. So every Admitted claim's control-only terminal transition fits whatever the others commit, retained snapshots included. A successful run's application effects are not reserved: if that commit is refused for room, the run is an ownerless refusal (K-8), never a lost claim.

**At reopen** `db.reserved` is rebuilt before any admission or recovery: it is Σ R_t over the `claim_open` entries, each computed from its op record's bounds. It is not stored. If the resident set plus that sum exceeds `resident_limit` (the limit was lowered by configuration), the open is refused with `Capacity(reserved)`, naming the shortfall.

**Open-admission tracking.** Keyed claims do not use `db.admits` (store.hirsel:1511). Their open admissions are the `claim_open` tree and the head's worker table, both removed when the claim replies.

### K-8: Crash recovery and the recovery queue

- **The admit commit** is durable, so an Admitted row survives a crash, with its arguments; the entry key and run bounds are the op record's.
- **Membership.** The head keeps two in-memory maps (std `omap`):
  - **members**: claim key → (refusals u8, queued seq or none), one entry per Admitted claim that has become ownerless in this open or was found at open;
  - **fifo**: seq u64 → claim key, the queue in order; a claim has at most one fifo entry, named by its member's seq.
  - The queue is filled at open from `claim_open`, and a claim is enqueued when it becomes ownerless in this open (its worker's commit was refused, RUNNER-SPEC H2's `NoRoom(p, c)`). Enqueueing a queued claim is a no-op.
  - **It follows the durable lifecycle.** The commit that replies to a claim, by any worker (client-started or recovery), deletes its member and its fifo entry in the same step as the head's worker-table removal. A claim is therefore in the maps only while its row is Admitted, and their size is at most the number of Admitted rows, which is at most Σ `keys_max` over the bound ops' records (a removed op's rows are bound through `Retired`, and quotas never change for an `op`).
  - **Refusal count.** `refusals` stays with the member across dequeue, re-enqueue and client-started workers; a refused commit by any worker of the claim raises it. It is reset only by the next open.
- **Its charge.** From std `omap`'s documented costs (api/omap.md): `new` allocates nothing, a node charges 104 B plus 32 slots of key and value, and owned payloads are charged with it. Leaves and branches are at least half full, so a map of n entries with slot sizes s_k and s_v and a payload of p bytes per entry charges at most C(n) = 2⌈n/16⌉ × (104 + 32 × (s_k + s_v)) + n × p. For **members** s_k = 24 (a `Bytes` header), s_v = 24 and p = 392 (the claim key); for **fifo** s_k = 8, s_v = 24 and p = 392. The head updates both with `insert_owned` while unshared, so no old version is retained. With n_r = Σ `keys_max` over the bound ops' records, the head reserves at open C_members(n_r) + C_fifo(n_r), plus the recovery timer child (T(0) and the timer's 4096-byte budget, as B-8 row 16a's deadline timer) and one more slot of the head's inbox for its tick. All of it is refunded when the head ends.
- **Scheduling.** The head has a recovery timer child (following the periodic pattern in [`stream.hirsel:209`](../../core/std/stream.hirsel#L209)) that ticks every `recover_ms` (1000) into the head's inbox; a dropped tick is harmless because the next one comes. On a tick, and after each group, if no recovery run is in flight, the head takes the queue's front and clears its member's seq. Before starting, it checks afresh: the claim's row is Admitted (`claim_open` present in the head's current snapshot) and no worker holds it. If a worker holds it, the entry is dropped and the member stays (with its refusals) for that worker's outcome; if the row is no longer Admitted, the member is gone already (above). Otherwise the head starts one run. So recovery needs no client retry and no unrelated traffic.
- **Run inputs.** A recovery run uses only the row's arguments and the op record's entry key, byte bound, step bound and `deadline_ms`, so it is bounded like any send. A send that arrives during it joins it.
- **Fair retry.** If the run's commit is refused (`NoRoom(p, c)`), the claim goes to the back of the queue with its refusal count raised, so a refused front never blocks the claims behind it. At `recover_max` (3) refusals in one open, by any of its workers, the head commits the claim's reserved control-only reply `Failed(OutOfBytes(Room{requested, available}))`, the two numbers of the third refusal's `Capacity` as `serve` returned it with `NoRoom(p, c)` (STORE AM1; ATTACK-4 B7), which K-7's terminal reserve guarantees fits. Retrying the same refused transition cannot supply missing room, so the bound is not a policy choice but the guarantee of progress.
- **A gone entry** (the op is `Retired`, or its entry key is not in this registry) gets the control-only `Failed(Gone)` reply from its reserve.
- **The reply commit** writes the reply, `expires_ms`, the index entries and, on success, the body's effects atomically. An `Error` or a `Failed` is a control-only commit (K-4).
- **A crash after the reply commit and before HTTP delivery** gives `Duplicate` on the retry.
- **Reclaim** happens inside the admitting commit, so a crash never leaves the trees and counts disagreeing.

### K-9: Removing a keyed op

A removed keyed op must still answer its live claims' retries, with the original status and body, until they expire.
- **The program keeps a `Retired` binding** for the `op`: the same method, the same claim function and header names as the op record (a different one is refused **at open**, by K-1's comparison with the record, since `bind` runs before any store is open), a route whose captures are the record's `Path` fields, and no entry, fields or statuses. It routes on its route and the alias and runs its claim function (K-2), so a retry gets the same principal and key.
- **Retired preparation.** In place of B-2 step 3, it calls `runner.prepare_retired(record, a: Admitted, request) -> Result[Send, Refusal]`, which needs no registry entry and no decoder. It checks the op against `a`'s attenuation, as `prepare` checks the entry. It assembles the argument record from the path, query and body by the record's field mapping (B-4's forms and assembly; on the alias, `Path` fields come from the query, B-12), encodes it in declared field order with the record's argument schema inside `bound bytes arg_max`, refusing a value that does not fit the schema (400, `Bad.kind`), and computes `request_digest(record's entry key, those bytes)`. B-4's assembly reads only the schema and the mapping; the live path's decoder only validates the bytes it produced. So the bytes are canonical for the schema, and equal arguments give the original digest even after the entry and its decoder are gone. A `via` validator does not run; it ran when the claim was admitted, and a request whose bytes differ has another digest. It then checks the byte budget and the deadline against `a`'s arrival time, as `prepare` does. Its `Send` holds the record's entry key, the claim key (op, principal, key) in place of a stream key, the argument bytes, the digest, the budget and the deadline.
- **The answer**, from the store, by K-5's order: liveness first, then digest. A live claim with another digest gets `RequestMismatch` (422). A live Replied claim with an equal digest gets `Duplicate`, rendered from the op record's result schema and statuses. A live Admitted claim with an equal digest is recovered (K-8) as a gone entry, and the send joins that run. An absent claim gets `Failed(Gone)` (B-7's 500 row) with nothing committed. It judges liveness at main's last committed `at`, so it relies on no time the store has not persisted, and it runs nothing.
- **The open check.** At open, after `db.reserved` is rebuilt (K-7) and before any admission, recovery or sweep, when some op record's `op` is not bound, the head runs one commit on main that writes nothing and judges at its `at` (K-4); the store still writes that commit's frame, so the `at` the decision relies on is durable before the open proceeds. An unbound op with a live row (an Admitted row, or a Replied row not expired (K-5) at that `at`) refuses the open with `KeyedOpDropped(op, last_expiry_ms)`, where `last_expiry_ms` is `Never` when a row's expiry is `NEVER` or the row is Admitted. If the store refuses that commit for room, the open is refused with `Capacity(reserved)`, as K-7's reopen check. So a deploy can never silently strand a live claim, the error says when the `Retired` binding can be dropped, and a later wall-clock rollback cannot make an unbound op's rows live again: every later decision reads an `at` at least as large.
- **Two deploys and schema changes.** The check is against the store, not against `previous`, so any number of deploys inside retention keep the claims answerable. An entry whose result type changes has a new entry key; its old op's record still holds the old result schema, so old replies stay decodable.
- **Then.** Once every row of the op has expired, the `Retired` binding can be removed; the orphan sweep (K-6) deletes the rows, the counts and the op record.

### K-10: What keyed operations are not

- Not the stream machine unchanged. It shares `admit`'s logic, and extends F2's control section (K-4).
- Not an HTTP cache. The store is the only memory.
- Not available to compute, query or granted entries.
## 5. Outside inputs

Every value the HTTP library and the runner's bindings receive from outside the run passes the replay point ([CAP-8](capabilities.md#cap-8-the-replay-point)). This section names each one, so that no read is ambient and a recording build observes each once. None of them is durable: keyed state survives a crash only through the store's commits ([CAP-9](capabilities.md#cap-9-recording-is-outside-the-program)).

**Logs (RUNNER-SPEC v2.16 M5).** Routine logs and diagnostics name a session by its Caller, and never include Hello or Welcome bodies, token headers or Random deliveries. The keyed record's Caller is decoded by K-4's claim grammar, and K-8's ownerless recovery runs the policy with it. What a recording of these inputs holds, and who may read it, is the tools' ([recording](../recording.md#credentials)).

**The clock contract.** Every reading of time is an explicit provider read at a named point. No rule reuses a reading across operations, and no code reads time ambiently. Each `host.mono_ns` read is either a **set read**, which computes a deadline, or a **check read**, which tests one already set (HTTP-5). The named points are exhaustive for Part A:

| Clock | Kind | Point |
|---|---|---|
| `host.mono_ns` | set | accept (the first-head deadline); each phase start (HTTP-5's phase table); each output unit's start, in the connection and in a stream writer; a refusal task's start; `net.finish`'s start (`linger_ms`); Ingress arrival (B-2); each relay timer (B-8); the commit read that sets `lease_end` (§2); each §2 timer arm. There is **no read at a hand-over**: a stream group's pre-head deadline is the handler's `Request.deadline`, carried unchanged (HTTP-10) |
| `host.mono_ns` | check | between two successful operations under one deadline, in the connection, a refusal task, a stream writer and `net.finish`; after each wake of a handler's std wait (`Body.read`, `ctx.call`, a one-shot read's wait for its Batch, B-11), against min(its own deadline, `Request.deadline`); the stream owner's one read at every event (§2); the relay's one read when it accepts an `Ask`, against the send's deadline (B-8) |
| `host.wall_ms` | — | once per response head for `Date` (HTTP-9) |

A check read is taken only between operations, never inside one, so a phase makes at most one check read more than it has operations.

**Part B, unbuilt** (lands with §4): `host.mono_ns` as a set read at each arm of the head's recovery timer (K-8). Keyed admission (K-5), the reply commit (K-6), the orphan sweep (K-6) and K-9's open check take no read of their own: each reads its store run's `at`, which comes from the store's one `host.wall_ms` read per run (K-4). The runner takes no clock read for a claim function (K-2): a function that checks freshness reads its own `Clock`, which passes the replay point through its grant, as every read of application code does.

**The inputs**, each at issue and at delivery, through the `Net`, `Clock` and `Random` grants:
- each listener's bound address and each accepted connection's peer address;
- each receive's delivered bytes, exactly as fragmented;
- each send's offered buffers and deadline, checked at issue, and accepted count, including partial sends;
- each readiness or deadline wake;
- each clock read named above, with its point and kind;
- each `host.random` delivery (an anonymous Caller's subject).

**The scheduler's choices** are inputs too (CONC-10). These include the 16-request batch point (HTTP-9), every refused admission, and each `try_send` and `try_receive` outcome (Ingress, the relay, the stream timer, an ending session's `Drained`, and the publisher's sweep of retirement marks). Part B adds the recovery queue's starts (K-8).

**Computed by the run, never an input:**
- parsing, framing and routing;
- JSON decoding and transcoding;
- argument bytes and `request_digest`;
- SSE encoding;
- credit and timer decisions;
- buffer charges and step accounting (MEM-30 slice ends are events in the run's one order).

**What the reads are.** The inputs are the `Net` deliveries, the clock reads and the `Random` deliveries. Everything a handler reads from `Request` is computed from them.

**Static assets** are part of the build (HTTP-14), so serving them reads no outside input.


## 6. Cutover

The WebSocket upgrade moves onto std `http`. The parallel parse and serve paths are deleted in the same change. There is no adapter and no period with both.

| Today (fbdb7d69) | After |
|---|---|
| `websocket.server(b)` parses the head itself (core/std/websocket.hirsel:126) | `websocket.accept(head: Head, b) -> Result[Accept, Refused(u16)]` checks the `Head` the connection task already parsed. `server` is deleted. |
| `wsock.source(b, peer, ingress)` parses the head a second time (core/std/wsock.hirsel:210) | `wsock.source(head: Head, b, peer, ingress)` reads the same `Head`. |
| The runner's upgrade, Hello and session path (WIRE-SPEC W3, not yet on main) would own its own accept loop and head read | The runner registers `GET /hirsel` as a raw route that returns `Upgrade`. The supervisor admits the session (U) before the 101 is committed; `upgrade.start` writes the whole 101, then the session starts from `Taken` (HTTP-10): the halves, plus the read buffer with the exact tail. |
| bench/wire/reference/mod.rs has a hand HTTP server | It stays as a reference bench outside the product. No product path calls it. |
| The `websocket_upgrade` tests call `websocket.server` | They move to `http.serve`, and each case goes through the connection task. The `http1` head-limit cases stay in `core/std/tests/http1.hirsel`. |

**WIRE-SPEC §2.1's values are unchanged.**
- `pending_max` (1024) is `conns_max`, released at Hello accepted for an upgrade, and at the first response's end for HTTP.
- `upgrade_ms` (5000) is `first_head_ms`, measured from accept: a socket's first head must be complete within it, whatever the route.
- `hello_ms` (10000) stays the session's, from the 101.
- The per-IP socket limit is answered 503 at `serve`, before any upgrade, while a refusal slot is free; otherwise the socket gets a bare close with no response (HTTP-1). A full per-connection frame pool after upgrade is WebSocket Close 1013 (B-2).

## 7. Build slices and acceptance laws

**Prerequisites not built at fbdb7d69:**
- RUNNER-SPEC v2.12's PubIn (`place_head` still takes `Receiver[Join]`, runner.hirsel:810);
- the wire session over `Net` (WIRE-SPEC W3; `connect` still takes `Sender[Down]`, runner.hirsel:1203);
- runner.md still says v2.4;
- **retirement marks** (specified in RUNNER-SPEC v2.14, CHANGES-v2.14 and law L25 `retired_without_waiting`; not built; §2 Publisher retirement): `Joined` may carry a session-owned retirement mark, and after every message the publisher sweeps the marks and retires each session whose mark is closed; an ending session's wake is v2.13's `Drained`. PubIn's variants are unchanged. It must be in RUNNER-SPEC before H6's one-shot reads and H8 start; H4 builds raw streams and the group's subscription plumbing, and its subscription cases run in H8.

**Columns.** "Starts" is what must be built before a slice starts. "Lands after" is what must land first, when that differs. Part A (§0-§3, §5-§8) slices never wait on H9 or H12. Part A lands alone; §4 and Part B's rows below are marked unbuilt until they land.

**Oracles.** Every law's expected value comes from an oracle independent of the code under test: a hand-derived corpus, the RFC or WHATWG text, a reference parser, or an invariant over the recorded trace. A count of examined bytes or a credit sum is an invariant read from instrumentation the slice adds, not from the code's own return value.

**Engine parity.** Each law runs on both engines. A slice lands with both green, except where "Known failures allowed at landing" says otherwise.

### Part A: std `http` and runner bindings

| # | Slice | Starts | Lands after | Acceptance laws (mutant each catches) |
|---|---|---|---|---|---|
| H1 | `http1.resume` with a retained cursor, and HTTP-4 framing over `Head` | — | — | **L1** resume parity and work: for every corpus head and every split point, `resume` over the fragments equals `request` over the whole, and the total bytes examined (instrumented) are at most the head's length plus one byte per fragment boundary (`restart_cursor_loses_field`, `rescan_from_zero`). **L2** framing corpus: the RFC 9112 §6.3 and §7 vectors plus the smuggling set (TE+CL, duplicate CL, a list in CL, TE in 1.0, unknown coding, chunk-size overflow, a lone CR in a chunk line, oversize trailers), each with its hand-derived status and close; after trailers the parsed `Head` is byte-identical to the head before the body (`te_wins_over_cl`, `accept_equal_dup_cl`, `chunk_size_wraps`, `trailer_merged`) |
| H2 | The `serve` task group and the connection task: buffers, writer and cursor, absolute deadlines, drain-or-close, pipelining, HTTP-15 rows 1-9b | H1 | — | **L3** absolute deadlines: a client sending one head byte every `head_ms/4` is closed at its deadline (±1 wake), and the first head of a fresh socket at `first_head_ms` (`deadline_reset_on_progress`, `first_head_from_head_start`). **L4** committed prefix: under a scripted provider that accepts 1..n bytes per send, the bytes received equal the response exactly (`resend_from_start`, `skip_after_partial`). **L5** close and drain: after a terminal send failure or a passed output-unit deadline, an unread body over `drain_max`, a drain timeout or a framing error, the next pipelined request is never answered. A send that returns `Ok(n)` with n short of the unit is progress, not failure: the writer continues and the connection may stay open; only a terminal send failure or a passed unit deadline closes. A drain reads at most `drain_max` bytes and issues at most ⌈`drain_max`/8192⌉ + 1 reads; every close ends within `linger_ms` of its start (`keepalive_after_terminal_failure`, `close_on_short_progress`, `drain_unbounded`, `finish_without_deadline`). **L6** one handler and bounded memory: with 3 pipelined requests the second handler starts after the first response's last byte, and the scripted provider's call log shows no socket read issued while a handler is active, except those the handler's own `Body.read` calls cause (a lazy body is read through, and its reads are tested to deliver exactly the body's bytes); peak charged bytes per connection ≤ C; a response larger than `out_cap` is written with the writer buffer never above `out_cap`; a partial write is never reported as a completed response, and a terminal error never as partial (`read_while_handler`, `grow_out_buf`, `partial_as_complete`). A body or chunk limit hit inside a handler that catches the error still closes the connection (`limit_caught_reuses`). **L11a** socket caps: interleaved HTTP and WebSocket clients past `sockets_max`, `http_max`, `upgrades_max`, `conns_max` or `ip_sockets_max` get the static 503 while a refusal slot is free and a bare close otherwise, and no admitted socket's reservation is touched; a server with no Upgrade route holding `http_max` idle keep-alive connections, with a full supervisor inbox and blocked acceptors, stays within HTTP-1's populations bound (`count_http_only`, `refuse_in_supervisor`, `release_c_at_first_response`, `cap_after_inbox`). **L2p** interoperability (HTTP-4 rule 12): Send L2's corpus through nginx and HAProxy with stock, documented framing settings. Each vector passes if the proxy itself refuses the request or the client receives the answer direct `serve` gives to the request the proxy actually forwarded, captured on the wire. A second smuggled request must never reach `serve` (`proxy_desync`). The checker refuses a view returned in an `Outcome`, captured by a producer, or live across a compaction, and accepts a handler that streams a body through `Body` (`ui` cases; `view_escapes`) |
| H3 | Router and raw handlers, HTTP-12, the static manifest | H2 | — | **L-route** conflicts: overlapping patterns with neither more specific are refused at `build`, and a more specific pattern wins regardless of order (`first_added_wins`). A 405 lists exactly the routed methods. **L-asset**: the ETag is per representation, `If-None-Match` gives 304 only for that representation, and HEAD has no body (`etag_from_identity_only`) |
| H4 | `Stream`, the SSE encoder and §2's ownership | H2 | — | **L7** atomic events: cutting the stream at every byte offset, a parser transcribed from WHATWG "Interpreting an event stream" yields exactly the complete events before the cut; a heartbeat never splits an event; an `EventBuf` after a failed append is byte-identical to before it, and the bytes a client receives never include any byte of a failed or incomplete append, observed on the wire under every write fragmentation (`flush_partial_event`: the writer publishes bytes of a failed or incomplete append; `heartbeat_in_event`; `append_leaves_partial`). **L8** credit and progress: at every recorded step `available + building + queued + writer-held = k`, plus the one control slot; with a reader taking 1 B/s and a producer at full speed, peak charged bytes ≤ S, where S includes every transient peak of §2 (the owner's queue and control slot, the writer's J, the `net.finish` scratch, the decoded P within `producer_bytes`), and the encoded P is row 13c's hand-over reserve outside S, so the group's peak is at most S + W_s, the hand-over block (HTTP-1) that holds the encoded head and the encoded P; a stalled producer gets heartbeats until the lease passes, the lease is checked at every event and not only at a Tick, and then the stream ends; a ready producer with credit always makes progress, and the admitted workload (`streams_max` groups at full rate, with S as above) never sees `Capacity`. (`unbounded_inbox`, `send_without_credit`, `lease_reset_on_heartbeat`, `lease_only_on_tick`, `credit_leak`). **L9** release: after the client closes, every task in the group ends within `write_ms` + `linger_ms` + one item's steps plus scheduling delay (CONC-10 promises no wall-time bound for runnable tasks, so the law measures recorded events), and P's drop is mechanical (`join_before_cancel`). |
| H5 | `Ingress`, `runner.prepare`, the HTTP relay; the session moves onto them | PubIn; WIRE W3 session | — | **L10** equal identity: over the unsorted B-4 corpus (maps in non-sorted order, every shape), an HTTP request and a WebSocket `Call` with the same principal, stream, position and value give byte-equal argument bytes, equal digests and equal durable outcomes; a body that increments a counter shows exactly one increment per identity; and one decode path is used, checked structurally (the session's `check_arg` and `Req` building are gone) (`digest_over_json`, `sort_map_members`, `session_keeps_own_digest`). **L11** refusal before entry: a bad principal, a disallowed entry, a malformed argument or an expired deadline each commit nothing and run no body, over both transports; a full Ingress inbox, bucket table or relay is `Capacity` (503) (`admit_before_authority`) |
| H6 | `HttpBinding`, B-4, B-5's transcoder, B-11, B-12, the generated router, client and OpenAPI | H3; H5 | — | **L12** lexical round trip: for every admitted shape, value → JSON → value is the identity, and canonical JSON → value → JSON is the identity; floats are checked at the bit level against hand-derived spellings, and invalid spellings (bare `NaN`, `Infinity` and `-Infinity` tokens, which are not JSON, and an exponent past the width) give 400, while the quoted strings `"NaN"`, `"Infinity"` and `"-Infinity"` are B-4's valid float spellings and decode to those values; Maps with integer-like and negative-zero-looking string keys, inserted out of order, keep their order through the generated TypeScript client in Node and a browser; the oracle is hand-written canonical spellings per table row (`u64_as_number`, `map_as_object`, `quoted_nan_refused`, `float_double_rounding`). **L13** bind refusals: each refused B-4 row (including a real `Option[Option[T]]` field and one source per field) fails `place` with its named check (`option_option_admitted`, `refuse_at_request_time`); the transcoder's output for every corpus reply is within B-5's `a·n + b` bound for the n bytes it consumed, including deep values, empty records, `Map[String, ()]` and long variant names on a recursive type; peak charged bytes while transcoding stay at J plus `out_cap`; a depth-65 reply gives 500 `{"fault":"depth"}` (`depth_unchecked`, `bound_from_value_size`, `buffer_whole_reply`). **L14** status table: each B-7 row is produced by a scripted head answer, and the status, body, `Idempotent-Replayed` and `Retry-After` are as tabled (`abandoned_as_504`, `duplicate_without_replayed`). **L-op**: moving a binding's route keeps its retries' identity through the alias; a retry after its binding is removed gets the stored reply with the original status; a status change in a new `op` never re-renders an old reply (`route_in_identity`, `status_from_current_binding`) |
| H7 | `Idempotency-Key` and delivery failure | H6 | — | **L15** disconnect and restart: the client closes (a) before the head admits, (b) during execution and (c) after commit but before the response; a retry with the same key gets the right answer in each case (a new run, a join, or the original status and bytes plus `Idempotent-Replayed`); the body's execution events in the trace number exactly one per identity, and a committed counter increment shows exactly one commit. The same after a process restart, where HTTP holds no memory. With the head's answer delayed, a principal repeats expiry, disconnect and reconnect 10³ times: each send gets its answer once, the relay's charged bytes, delivery blocks still held by connections included, stay within `relay_bytes`, every send record's timer is joined and no stale `Expired` outlives its record, and the held positions within `relay_positions_max` and 1 + q per principal at every step, and a position is refunded only at `Told` (`execute_in_http_task`, `cache_in_http_layer`, `relay_drops_owed`, `refund_position_at_abandoned`, `refund_before_timer_join`, `outcome_payload_unpaid`) |
| H8 | SSE subscription sink | H4; H6; W-D3 Reset on main | — | **L16** full envelope: each event's JSON is the full WIRE envelope of exactly one frame, `sub` included, in order, so the events concatenate to the session's frame sequence; `outcomes` is always `[]`; an empty initial result is one completing `batch` event; a request with `Last-Event-ID` after changes gets a full fresh state; an initial oversize is 503 with the budget body; a row whose present value is JSON `null` (a V of `()`, and of `Option[T]` at `None`) and a deleted row decode to different frames, over SSE and B-11 alike; an initial Batch delayed past the pre-head deadline gives 503 and no event; on a Reset a slow reader gets the whole `event: reset` and an end when the writer delivers it in time, and otherwise a close within the unit deadline with no partial event applied by the WHATWG parser (`split_batch`, `merge_batches`, `version_resume`, `drop_empty_complete`, `null_as_removed`, `drop_sub`, `head_before_initial`). **L9p** publisher retirement: a subscription group or a one-shot read ends, normally or by failure or cancellation, (a) with PubIn empty and (b) with PubIn full so that its `Drained` is dropped, while other sessions keep PubIn contended, and then the publisher goes quiet after taking what is queued. In both cases the group's own teardown performs no wait on the publisher, and after the publisher's next message the subscription's Cover, read set and held content are released (instrumented publisher state returns to its value before the subscription) and the read slot is free. 10⁴ repeated completed reads and streams keep the publisher within `streams_max` + `reads_max` subscriptions. A live WebSocket session's subscriptions are unchanged by every sweep (`teardown_waits_on_pubin`, `wake_required_to_retire`, `session_kept_after_end`, `slot_leak_on_failure`, `sweep_retires_live`) |
| H10 | Cutover (§6) | H3; WIRE W3 session | integrated atomically with W3's upgrade route | **L22** exact tail: for every split of `head ‖ frames`, with every WebSocket frame type, the whole decoded frame sequence equals the frames sent, and the session's input is exactly the tail bytes followed by later reads, none dropped or repeated; the client receives the complete, byte-exact 101 before any frame output; the head is parsed once per socket: instrumentation counts exactly one parser initialization and one completed head, any number of resumptions between them, and total examined bytes within L1's bound (`drop_tail`, `reparse_head`, `partial_101`). The upgrade refuses a body before the 101 (`tail_as_body`). grep: `http1.request(` appears only in std `http`, clients and `http1` tests |
| H11 | Replay matrix (Part A) | H4; H7; H8; H10 | — | **L23** same-build record and replay through the test runner ([recording](../recording.md#testing-with-recordings)), with a clock provider that fails on any read at a point §5 does not name, over a scripted client covering fragmentation at every byte, `Capacity` at each HTTP-15 admission, partial writes, heartbeat and lease timers, disconnect after commit, an upgrade with a tail, and a partial 101. The recording is repeated with varied clock values and event orders, and each replay's sends, charges and step trace equal its recording's on both engines. Separately, for a hand-scripted session per named point, the trace's clock reads equal a hand-derived sequence of labelled reads (point, and set or check), the check reads between operations included and no read at a stream's or an upgrade's hand-over (the pre-head deadline equals the handler's `Request.deadline`), so wrong sampling that recording and replay share is caught (`unrecorded_date_read`, `reading_reused_across_operations`, `ambient_clock_read`, `set_read_as_check`) |
| B1 | `bench/micro/http/instructions`, http1-plaintext-keepalive | H2 | — | Server instructions per request (Callgrind) against Tokio+httparse (`bench/micro/http/tokio`) and hyper/axum (added in this slice), on CPUs 0-11 and 16-27. Report the ratios. The regression gate is set from the first measurement. **Measure next:** a SIMD byte-class head scanner with scalar parity (L1 extended) |

### Part B: keyed operations (unbuilt)

| # | Slice | Starts | Lands after | Acceptance laws (mutant each catches) |
|---|---|---|---|---|---|
| H9 | Keyed operations (§4): store control extension and terminal reserve, admission, claim functions, recovery queue, `Retired` bindings | H5; H6; Part B reviewed | — | **L17** concurrent duplicates: two keyed sends with equal digests run one body, and both get its outcome; the oracle is the body's execution events in the trace, not the reply (`no_join_runs_twice`). **L18** liveness first: at every live row state (Admitted, Replied before expiry) an equal key with another digest gives 422; at and after expiry, with and without reclaim having run, it is a new operation (`replied_before_digest`, `expired_as_mismatch`). **L19** expiry from reply: with admission and reply at distinct recorded times, and a reply delayed past `retention_ms` after admission, the claim is Duplicate at `reply + retention_ms − 1` and new at `reply + retention_ms`; an Admitted row never expires; reply-commit `at` values of 2^64 − 2 − `retention_ms`, 2^64 − 1 − `retention_ms` and `u64::MAX` itself give a finite expiry, `NEVER` and `NEVER`, and a `NEVER` claim is Duplicate at an admission `at` of `u64::MAX`, is never reclaimed or swept, and makes K-9's open check report `Never`; with `retention_ms` 50, a commit at wall 200 followed by a reply commit whose wall read is 100, the reply's expiry is 250 and the claim is Duplicate at the next admission (`expire_from_admit`, `lease_on_admitted`, `expiry_wraps`, `never_reclaimed`, `reply_from_raw_wall`). **L19r** old release, new claim: a claim expires while its old reply's delivery is held back (scripted delay), the same key is admitted again through the relay with other arguments and another outcome, and each send gets its own claim's outcome by send id (`join_across_claims`, `relay_local_keyed_mismatch`). **L20** quotas and bytes: with two bindings and two principals, one principal saturated and one not, the saturated one gets 503 while the other admits; a per-op scope full only of expired rows, behind live rows of unrelated bindings, admits after reclaim; each quota alone (principal full with op free, op full with principal free) refuses or reclaims as K-6 says; a resident-admission refusal is `Full`; `Full` commits nothing; under a 10⁵-key sweep the rows, index entries, count rows and charged bytes stay within K-7's bound; with unrelated bound ops holding rows, the orphan sweep's examined entries per sweep stay ≤ 3 and the head's recovery charge equals K-8's C_members + C_fifo equation (`evict_oldest`, `count_not_deleted`, `reclaim_wrong_scope`). **L20t** terminal reserve: at `resident_limit`, with a snapshot retaining old roots, every admitted claim's control-only reply commits; with two Admitted claims, a success of the first whose application writes would consume the second's reserve is refused and the second's control-only reply still commits, while a success within the first's own reserve commits and releases only its own; after a checkpoint reopen `db.reserved` equals the hand-summed R_t of the open claims before the first admission (`reserve_row_only`, `reserve_ignores_retained`, `terminal_spends_others`, `reserve_not_rebuilt`). **L21k** crash cuts: at every cut of admit, run, reply and reclaim, and across a checkpoint reopen, with two claims Admitted at once: a Success leaves exactly one committed application effect (a counter increment) and its control rows; an `Error` or a `Failed` leaves zero application effects and exactly its control rows (row, indexes, counts, call record); trees and counts agree; the body's execution events are counted per retry and equal the runs the K-8 rules allow (`index_written_after_reply`, `error_drops_claim`, `reclaim_outside_commit`, `error_keeps_effects`). **L21r** recovery: with no client retry and no other traffic, ownerless rows found at a quiet reopen and rows made ownerless in the same open are run from the queue by the recovery timer; a refused front followed by a recoverable claim lets the second recover, and the front ends `Failed(OutOfBytes)` after `recover_max` refusals, counted across re-enqueues and an intervening client retry whose commit is also refused; enqueueing twice runs once; a queued claim completed by a client's run before its recovery turn is never run again and leaves no queue entry; the head's charged bytes include both maps, the timer and its inbox slot at their K-8 equations (`queue_not_rebuilt`, `recovery_needs_traffic`, `front_blocks_queue`, `queue_duplicates`, `stale_queue_entry`, `refusals_reset_on_requeue`, `queue_uncharged`). **L-retire**: two deploys and a result-type change inside retention, the entry and its decoder removed: a retry through a `Retired` binding, on its route and on the alias, with equal arguments gets the original status and body, and with changed arguments gets 422, both for a Replied claim and for an Admitted one (which is recovered as gone); dropping the binding while a claim is live refuses the open with `KeyedOpDropped`; a row expiring at 100, the binding dropped at an open at wall 101, then the wall clock set to 99 before the sweep has run, and a restart at 99: the restart opens and the sweep deletes the row (`open_check_unpersisted`, `expiry_from_raw_wall`); after expiry the sweep removes rows, counts and op record, examining at most 3 entries per sweep and passing bound ops (unrelated bindings with rows) at one examined entry each, within K-6's completion bound (`render_from_current`, `strand_live_claim`, `sweep_unbounded`, `retired_skips_digest`, `retired_needs_decoder`). **L-claim**: a keyed binding's claim function runs once per request, before `prepare`, with a copy of the wire grant and exactly the listed headers (lower-cased, repeats kept, in arrival order) and the body; its `Error(())`, a fault and an exhausted byte or step bound each give 401 with nothing recorded and the entry's decoder never run, and an empty or out-of-form key gives 400; `Wire.bind` is not called for a keyed request; the original route and the alias give the same claim for the same request; a `Retired` or keyed binding whose claim function or header names differ from the op record refuses the open with `KeyedOpChanged`, while an edit to the function's body or to code it reaches does not; a keyed binding on a place with no wire grant refuses the open with `KeyedNoWire`; the oracle is the function's own call count and inputs, recorded by the test program, and the store's rows. The two K-2 worked examples, built as test programs, give their stated answers on Standard Webhooks' published signature vectors and on GitHub's documented example delivery, and a GitHub body resent under a new delivery id is a new claim (`claim_after_prepare`, `claim_unbounded`, `wire_bind_also_runs`, `alias_skips_claim`, `claim_unpinned`, `claim_pinned_to_address`) |
| H12 | Keyed replay | H9; H11 | — | **L23k**: L23 extended with keyed admissions on both engines, covering a claim function's own clock read and HMAC verdicts at the edges of its tolerance, expiry at `−1`, `0` and `+1` ms, reply-commit and admission `at` of `u64::MAX` itself (`NEVER`), wall-clock jumps backwards and forwards with every keyed decision reading its commit's nondecreasing `at`, `Full`, reclaim, reopen with the recovery queue and its timer ticks, the open check, orphan sweeps, and reply-time reads, each read compared with a hand-derived labelled sequence as in L23 (`unrecorded_wall_read`, `queue_order_unrecorded`) |

**Start now:** H1, and B1's Tokio and hyper sides. B1's Hirsel side starts after H2.

**Critical path:** H1 → H2 → H4 / H5 (PubIn and W3) → H6 → H7 / H8 → H11. H10 lands with W3's upgrade route. Part B (H9 → H12) runs beside it and nothing in Part A waits on it.

**Known failures allowed at landing:** B1's hyper side, if its build lags.

**Failures that block landing:**
- L4, L5, L10, L11, L15, and in Part B L17-L21k, L19r, L20t, L21r, L-retire and L-claim: a wrong answer there loses data or runs a body twice;
- either engine red on any law;
- any compile, lint or schema red.

## 8. Decisions

**Owner and framing**

| # | Question | Alternatives | Adopted | End state |
|---|---|---|---|---|
| 1 | Who owns a socket | a separate server per protocol; a reader task plus a writer task per request | one connection task per socket, with the only parser, framing and writer | the same; §6 deletes every other path |
| 2 | Deadlines | reset on progress (goose-http, Roc); per-operation timeouts | one absolute deadline per phase and per output unit, from a set read, with check reads between operations; the first head within WIRE's `upgrade_ms` from accept; a subscription stream's head within its request's deadline | the same |
| 3 | Duplicate equal `Content-Length` | accept (Go) | refuse | the same: one lexical form |
| 4 | Pipelining | parallel handlers; refuse pipelining | one handler, a bounded unread tail, read only between requests | the same |
| 5 | Big bodies | grow the write buffer (goose-http) | a fixed buffer, and a body written from the handler's bytes through the cursor | the same |

**Streams**

| # | Question | Alternatives | Adopted | End state |
|---|---|---|---|---|
| 6 | Stream structure | an unbounded channel (Convex); the producer writes the socket | a stream group spawned by the `serve` supervisor, in two phases (pre-head, committed): owner, sole writer, reader monitor, timer and a credited producer, k = 2; the lease checked at every event | the same |
| 7 | SSE resume | `Last-Event-ID` as a version | none: W-D3 Reset and a fresh subscription | the same; a version alone cannot resume |
| 8 | Static assets | read from a `Dir` at run time (WIRE-SPEC §5) | build manifest, per-representation ETags | the same; WIRE-SPEC §5 is superseded |

**Bindings**

| # | Question | Alternatives | Adopted | End state |
|---|---|---|---|---|
| 9 | Binding syntax | annotations; a new declaration form | library `HttpBinding` values, finalized in describe | the same |
| 10 | Handler authority | the application context (Go, Convex) | the sealed `Ctx` | the same |
| 11 | Admission location | inside each transport | `Ingress` plus `prepare`, shared; `Stream` sends held by the runner's relay as a session's are (rows 37, 38) | the same; the session's copies are deleted |
| 12 | 64-bit ints in JSON | a number; number or string (protobuf) | a string only | the same |
| 13 | Map form | a JSON object, sorted or in insertion order | an array of `[key, value]` pairs, in the value's order, for every key type | the same: one form that every JSON library keeps in order |
| 14 | Granted entries over HTTP | allowed without identity | refused at bind | the same until a granted-entry identity exists |

**Retry identity and keyed operations** (rows 16-19 are Part B, unbuilt)

| # | Question | Alternatives | Adopted | End state |
|---|---|---|---|---|
| 15 | Retry identity under `Stream` | an opaque key → mapping table; an HTTP cache | `Idempotency-Key` = (stream, position) | the same |
| 16 | After keyed expiry (Part B) | a tombstone; refuse forever | liveness is checked first: an expired claim is absent, and the key starts a new operation; retention declared | the same; it is the contract |
| 17 | In-flight keyed rows (Part B) | a wall-clock lease (Powertools) | no lease; ownerless Admitted rows run from a deduplicated, bounded recovery queue driven by its own timer, with fair retry and `recover_max` then a reserved `Failed` | the same |
| 18 | Keyed table full (Part B) | evict the oldest | refuse with 503, per op and per principal | the same: eviction is silent re-execution |
| 19 | Keyed state machine (Part B) | a separate machine; `admit` unchanged | `admit`'s logic shared, plus a store control extension of F2 (control-only commits, keyed trees, call record) and expiry | the same |

**Additions**

| # | Question | Alternatives | Adopted | End state |
|---|---|---|---|---|
| 21 | What bounds sockets | per-phase counts only (earlier draft) | `sockets_max` over each socket's whole life, with every socket task spawned by one supervisor | the same |
| 22 | A stream's connection after it ends | return to keep-alive | the connection closes | the same: a stream has no framing after its end that a client could trust |
| 23 | `Date` | cached per second; ambient | one `host.wall_ms` read per response head | the same |
| 24 | Reply nesting past depth 64 | grow a stack; fail at bind | 500 `{"fault":"depth"}` as a transport failure | the same; the bound comes from J |
| 25 | What identifies a durable binding | its route | a stable `op`, with `POST /.hirsel/op/{op}`; a removed `Stream` op served from `previous`, rendered from `previous`'s describe (keyed retention is Part B) | the same |
| 26 | SSE event size | an `event_max` buffer per event | piecewise encoding from the frame; no `event_max` for subscriptions | the same |
| 27 | One-shot reads of a query | encode `Cover` as a value | a transient subscription on its own session, under a `reads_max` slot, returning the batch envelope, retired as row 36 | the same |
| 28 | Keyed time unit (Part B, unbuilt) | ns (B9) | ms of `host.wall_ms`, read through the commit's nondecreasing `at` (K-4), with `retention_ms` ≤ 2^53 | the same |
| 29 | GitHub webhooks (Part B, unbuilt) | refuse them; treat the delivery id as authenticated; a built-in verifier with the weaker guarantee in OpenAPI (earlier draft) | nothing built in: a worked example of a claim function (K-2), with its weaker guarantee stated there | the same; GitHub signs no key or time, and R6 keeps credential checks in application code |
| 30 | Who holds an HTTP send | the connection task | the runner's relay, with its own units and limit (row 37) | the same |
| 31 | An oversized initial subscription result | a stream that ends at once | 503 with the budget body and no `Retry-After` | the same: retrying cannot help |

**Part A, later additions**

| # | Question | Alternatives | Adopted | End state | Reversible |
|---|---|---|---|---|---|
| 32 | What the server reserves | `conns_max` × C plus upgrades (earlier draft) | one count per population, each with its cap and check point: pending (`conns_max`), live HTTP (`http_max`), streams, upgrades (`upgrades_max`), reads (`reads_max`), all inside `sockets_max` from the supervisor's receipt; sockets not yet received bounded by inbox and acceptors | the same: every allocation has one payer | until H2 lands |
| 33 | Task result size | T = 288 | T(r) = 256 + r + 32, MEM-24's result term included | the same | — (MEM-24) |
| 34 | Allocations that outlive the connection | charged to the connection (earlier draft) | one encoded hand-over block per transfer (stream: head and P, W_s; upgrade: read buffer, 101 head and argument, W_u), paid by the supervisor after the connection ends and rebuilt under the recipient's reservation | the same: nothing else a handler allocated crosses | until H4 lands |
| 35 | A raw producer's retained state P | arbitrary sendable P | P = codec + send: encoded in the handler scope within `state_max`, decoded into the producer's limit; codec types hold no resources (OWN-4), so dropping P is mechanical (FAIL-13) | the same: a cleanup that can wait would void the release bound | until H4 lands |
| 36 | How the publisher learns a stream or read ended | `Unsubscribed` may be dropped when PubIn is full (earlier draft); a blocking `Unsubscribed` plus a probe (earlier draft) | v2.13's pattern: a session-owned retirement mark that the publisher sweeps after every message, and a `Drained` wake that is safe to drop; teardown never waits; each group and read is its own session | the same: no step depends on slot fairness | until the RUNNER followup lands |
| 37 | Relay accounting | one unit per send, refunded at hand-over (earlier draft) | three charges: a send record (`relay_sends_max`), refunded after hand-over once its timer is joined and any queued `Expired` taken; a held position (`relay_positions_max`, 1 + q per principal) with its Outcome reserve, refunded only at `Told`; a delivery block per handed Outcome, ≤ `http_max` + 1, live until the connection drops it | the same | until H5 lands |
| 38 | Relay forwarding | blocking send to the head | `try_send`; a full head inbox is 503 with both charges refunded; the relay's own inbox is sized so a send into it is never refused as full, and each Outcome goes by `try_send` into the send's own 1-slot reply channel | the same | until H5 lands |
| 39 | A row in SSE and one-shot reads | `value: V \| null` (earlier draft) | a tagged envelope `{sub, table, key, present, value?}` | the same: lossless for every V, `sub` as WIRE has it | until H8 lands; the client wire form after |
| 40 | Clock reads | one list of deadline-setting reads (earlier draft) | set reads and check reads, each at a named point (§5) | the same | — |
| 41a | Refused sockets and `sockets_max` | count refusing sockets inside `sockets_max` (earlier draft) | a separate population of at most `refuse_max`, never admitted; process sockets ≤ `sockets_max + refuse_max + inbox_cap + listeners` | the same | until H2 lands |
| 41 | Refusal when no refusal slot is free | always 503 | a bare close with no response (accepted A8) | the same: refusing must not cost unbounded work | — |

**Part B, later additions**

| # | Question | Alternatives | Adopted | End state | Reversible |
|---|---|---|---|---|---|
| 42 | Where a keyed op's rendering and run policy live | in each claim row; in the current binding | one immutable op record per `op` in the store; a change needs a new `op` | the same: one copy, and old replies stay decodable | until H9 lands; the store format after |
| 43 | Removing a keyed op with live claims | drop after `previous` (earlier draft); keep secrets in the store | a `Retired` binding keeps the route and the claim function; the op record keeps the argument schema and field mapping, so `prepare_retired` recomputes the digest without the decoder; open refuses to strand a live claim | the same: no secret is in the store, and no live retry goes unanswered | until H9 lands |
| 44 | Terminal reply room | reserve the row only (earlier draft); a terminal commit tested against the whole limit (earlier draft) | a terminal reserve R_t per Admitted claim: index entries, call record, frame and retained path copies; other commits see `resident_limit − db.reserved`, a claim's terminal commit `resident_limit − (db.reserved − R_t)`; rebuilt at reopen | the same: a control-only reply can never be refused | until H9 lands; the store format after |
| 45 | Recovery scheduling | one run per group (earlier draft); a set beside the FIFO, emptied only at dequeue (earlier draft) | a recovery timer, charged member and fifo maps that follow the claim's durable lifecycle, a fresh check before each start, back-of-queue retry, `recover_max` 3 refusals per open then the reserved `Failed` | the same | until H9 lands |
| 46 | Keyed liveness in the relay | a local digest check (earlier draft) | none: every keyed send is forwarded under its send id, and answers follow send ids | the same: the store alone decides liveness | until H9 lands |
| 47 | Who decides a keyed claim's principal and key | a route capture that is also an argument; built-in `Bearer`, `StandardWebhooks` and `GitHub` verifiers over a place-wide namespace registry of sender secrets, chosen by a reserved `{sender}` capture (earlier draft) | the application's claim function, `Wire.bind`'s mechanism with the named headers and body as input; secrets checked by `secret.verify_hmac_sha256`; no registry, selector or verifier kinds (ruling R6) | the same: one binding mechanism for the wire and HTTP, and no authentication in Hirsel | until H9 lands |
| 48 | Expiry overflow | assume `wall_ms` < 2^63; saturate to 2^64 − 1 under the ordinary ≤ test (earlier draft) | `NEVER` = 2^64 − 1 is a reserved non-expiring value, excluded from the one expiry test | the same: erring towards no second run | — |
| 49 | Which clock decides keyed expiry | a fresh `host.wall_ms` read per decision (earlier draft: a rollback revives a claim the open check judged dead); a separate expiry floor in the control trees | main's commit `at`, max(read, previous `at`), already persisted in each frame and checkpoint; it both sets and tests expiry, and the open check commits before relying on it | the same: one nondecreasing clock the store already keeps, no second copy | until H9 lands |
| 50 | What the op record keeps of the claim function | its code address or revision (PROG-9, PROG-10); nothing | its identity bytes and header names: a different function or other inputs refuse the open with `KeyedOpChanged` | the same: an address would refuse the open on every std or compiler change for every op with live claims, and entries are matched by identity, not revision | until H9 lands; the store format after |

**Out of rung 1**

| # | Question | Alternatives | Adopted | End state |
|---|---|---|---|---|
| 20 | HTTP/2 and /3, imported C servers, worker-global cork; NET's reactor | — | HTTP/2, HTTP/3, C servers and cork are not in rung 1; NET runs on io_uring when the kernel allows it and on epoll otherwise, chosen once at runtime start | HTTP/2 is a later library over the same owner and authority; the reactor never changes a program-visible result |
