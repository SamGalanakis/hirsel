# TLS and secrets

TLS is a provider operation. The provider performs the cryptography: the OS provider
uses rustls. A program never handles keys, records or certificates. A program asks for a
client handshake on a connected socket and gets back the two halves of an encrypted
stream. Those halves are the `ReadHalf` and `WriteHalf` of [Net](net.md). After the
handshake, `net.recv_bounded` and `net.send_pair` move plaintext, so code written over the
halves works over TCP and over TLS without change.

A `Secret` is a credential that program code cannot read. Its bytes are held by the run,
outside program memory. The one operation that sends them, `tls.send_secret`, writes them
on a TLS stream whose verified name the secret permits.

The declarations for later slices use `hirsel unbuilt` fences.

## Types

Module `tls` declares `connect` and `send_secret`. Module `secret` declares `seal`,
`narrow` and `render`.

```hirsel
pub type Trust = System | Roots(Bytes)
pub type Handshake { protocol: Option[u64], peer: (u64, u64, u64, u64) }
pub type TlsFailure = Certificate(CertFailure) | Name | Alpn | Roots | Protocol(u8) | Closed | Truncated
pub type CertFailure = UnknownIssuer | NameMismatch | Expired | NotYetValid | Invalid
```

`NetError` gains the case `Tls(TlsFailure)`. Every other error a TLS operation reports is
an existing `NetError` case with its existing meaning.

| type | kind | bounds | logical size | cleanup |
|---|---|---|---|---|
| `Secret` | plain | `shareable`, `send` | 8 | capability |

`Secret` is a capability type ([CAP-2](capabilities.md#cap-2-capabilities-are-opaque)).
It designates bytes that let a program act as an account. It has neither `key` nor
`codec`. So no program can compare two secrets, encode one, or store one in a type that
declares `codec` ([OWN-4](ownership.md#own-4-bounds)).

### TLS-1: Client handshake

```hirsel core
pub fn connect(n: Net, take s: Socket, name: String, trust: Trust, alpn: List[String], until: Until) -> Result[(ReadHalf, WriteHalf, Handshake), NetError] waits
```

`tls.connect` consumes `s` and performs a TLS 1.2 or 1.3 client handshake on it.

- **Verification name.** `name` is the verification name and also the SNI name. It is a
  DNS name or an IP address literal. A DNS name has 1 to 253 bytes of ASCII labels and is
  compared case-insensitively. An internationalised name must be given as A-labels. A
  trailing dot is removed. An IP literal is verified against the certificate's IP
  subject alternative names and is not sent as SNI (RFC 6066 §3). The certificate must
  be valid for `name`. A certificate valid only for the socket's address is not enough.
- **Trust.** `System` uses the host's root store. The OS provider loads it once per run,
  at the first `System` handshake. `Roots(b)` trusts exactly the certificates in the PEM
  bundle `b`, and the host store is not used. Verification uses the provider's own wall
  clock. It cannot be turned off. Revocation is not checked.
- **ALPN.** `alpn` lists the protocols to offer, in order of preference. An empty list
  offers none. Each entry is 1 to 255 bytes, no entry appears twice, and there are at
  most 16 entries. If the server chose a protocol, `Handshake.protocol` is its zero-based
  index in `alpn`. Otherwise it is `None`.
- **Peer.** `Handshake.peer` is the SHA-256 of the DER encoding of the peer's leaf
  certificate, split into four big-endian `u64` words. The chain itself is never returned.
- **No resumption.** There are no session tickets, no resumption and no early data, so a
  handshake does not depend on earlier runs or earlier connections.

The following checks run in order, before issue. Each refuses with no host effect.

1. `s` was not made by `net.connect`: `Denied`. A socket from `accept` cannot be the
   client side of a handshake.
2. `name` is invalid: `Tls(Name)`.
3. `alpn` breaks its limits: `Tls(Alpn)`.
4. `Roots(b)` holds no certificate, or holds malformed PEM: `Tls(Roots)`.
5. No `Tls` rule of `n` covers the handshake (TLS-3): `Denied`.

When the provider is asked to perform the handshake, these failures are possible:

| case | result |
|---|---|
| chain does not reach a trusted root | `Tls(Certificate(UnknownIssuer))` |
| leaf not valid for `name` | `Tls(Certificate(NameMismatch))` |
| leaf or chain outside its validity at the provider's time | `Tls(Certificate(Expired))` or `Tls(Certificate(NotYetValid))` |
| a malformed or unacceptable certificate, including a bad signature or a disallowed key usage | `Tls(Certificate(Invalid))` |
| the peer sent a fatal alert | `Tls(Protocol(d))`, where `d` is the alert's description code (RFC 8446 §6). Example: 120 is `no_application_protocol`.
| the client detected a protocol violation | `Tls(Protocol(0))`. 0 means no alert code was reported. Hirsel does not report the alert rustls sends in this case. |
| the transport ended before the handshake finished | `Tls(Closed)` |
| a transport error | `Reset`, `Unreachable` or `Other(errno)`, as for a plain receive |
| `until` passed | `TimedOut` |
| the task was cancelled while the handshake was in progress | `Aborted` |

`s` is consumed whatever the result. On success it lives on as the two halves. On every
failure, cancellation or refusal, it is closed exactly once and every byte of TLS state
behind it is released. This is how CLOSE consumes a `HostFile` ([core](core.md)).

### TLS-2: Plaintext on a TLS stream

The halves from `tls.connect` are ordinary halves. The rules of
[NET-1](net.md#net-1-socket-operation-declarations) and
[NET-7](net.md#net-7-owned-connections) hold for them, with the following meanings:

- **Receive.** `net.recv_bounded` appends decrypted plaintext only: at most
  `max_receive` bytes, and never more than the caller's spare capacity. If plaintext is
  already decrypted and buffered, the receive completes without socket I/O. Otherwise it
  waits until a whole record has arrived, so a readable socket can still leave it
  pending. A delivery of 0 bytes means the peer sent `close_notify`. If the transport
  ends without `close_notify`, the receive gives `Tls(Truncated)` (RFC 8446 §6.1). After
  a `Tls` failure, every later receive on that half gives the same failure.
- **Send.** `net.send_pair` returns the number of plaintext bytes the TLS stream
  committed. A byte is committed when the session has taken it into a record. The
  session sends the ciphertext of committed bytes before the ciphertext of any later
  send, and before `close_notify`. `Ok(n)` means `n` bytes are committed. If the deadline
  passes or cancellation arrives after some bytes were committed, the send returns
  `Ok(n)`. If it committed zero bytes, it returns `TimedOut` or `Aborted`. The session
  holds at most one record (16 KiB of plaintext) of committed ciphertext beyond what
  the socket has accepted. The next send or drop flushes it first. Each byte reaches
  the peer exactly once when the caller continues from the returned count.
- **Both directions.** One TLS session serves both halves. The provider serialises the
  two halves' use of it, but only inside a callback and never across a wait. So a
  receive in one task and a send in another proceed together, as they do on a plain
  socket. Records that the session must send in reply, such as a key update response,
  are sent by the next callback that can write.
- **Drop.** Dropping a TLS write half queues `close_notify` and makes one nonblocking
  attempt to send it with any committed ciphertext, then performs `SHUT_WR`. Like the
  plain drop, it ignores the result, cannot wait, and passes no replay point. Dropping
  the read half releases its ownership. The last owner closes the descriptor once and
  frees the session. Hirsel makes no promise that the crypto library wipes key-schedule
  memory.

A program that must know the peer received everything waits for the peer's reply, as
on a plain socket. A send that returned `Ok` does not mean the peer has read it.

### TLS-3: Authority

Two things are required for a handshake. The socket must exist, which means its
`Connect` rule has already passed. And the `Net` passed to `tls.connect` must hold a
`Tls` rule that covers the handshake. [NET-6](net.md) defines the rule:

`Tls <names> <lo>-<hi> <trust>`

- `<names>` is a comma-separated list of name patterns. A pattern is either an exact
  name (a DNS name or an IP literal, normalised as in TLS-1) or `*.` followed by a DNS
  name. `*.example.com` covers every name with at least one more label that ends in
  `.example.com`. It does not cover `example.com` itself.
- `<lo>-<hi>` is checked against the peer port of the socket.
- `<trust>` is `System`, or `Roots=sha256:<64 hex>`. In the second form, the handshake's
  `Roots(b)` must have exactly that SHA-256 over `b`'s bytes. A rule that pins `System`
  does not cover a `Roots` handshake, and a rule that pins roots does not cover a
  `System` handshake.

A rule covers a handshake when all of these hold: one of its patterns covers `name`, its
port range holds the socket's peer port, and its trust matches. The check reads the
socket's peer address from the runtime's socket record. Asking the provider is not
necessary.

`net.narrow` intersects `Tls` rules as it intersects the other rules. Name patterns
intersect as sets of names: an exact name with a pattern that covers it gives the exact
name, and two wildcards give the narrower one when one covers the other. Port ranges
intersect. Trusts intersect only when they are equal. An intersection with no names, no
ports or unequal trusts is dropped. Narrowing never widens a rule
([CAP-7](capabilities.md#cap-7-attenuation)).

The halves of a TLS stream keep the socket's issuing provider and its peer. Their
operations go to the provider that performed `connect`, and no operation can point them
at another peer. The runtime also keeps the verified `name` on the socket record. That
is how `tls.send_secret` checks it (TLS-5).

### TLS-4: Provider calls

Two operation tags are added. Everything else on a TLS stream uses the existing socket
operations.

| tag | operation | states |
|---|---|---|
| 22 | `TLS_CONNECT` | C, I, D |
| 23 | `SEND_SECRET` | C, I, D |
| 27 | `RECV_BOUNDED` on a TLS half | C, I, D, unchanged |
| 26 | `SEND_PAIR` on a TLS half | C, I, D, unchanged |

**Why the handshake is a new operation and plaintext I/O is not.** A handshake has
arguments and a result that no existing operation has. A receive or a send on a TLS
stream has the same arguments, the same bounds, the same committed-prefix rule and the
same deadline as a plain one. Only the bytes on the wire differ, and the provider
already chooses those for each handle. Separate TLS receive and send tags would make
two copies of the same contract. They would also give the HTTP reader and writer two
code paths, and a recording two frame shapes. `SEND_SECRET` is new because its arguments
cannot be the bytes it sends.

**`TLS_CONNECT`.** `Request.handle` is the socket key. `Request.path` points to the
runtime's `TlsRequest` extension. It holds the `NetworkRequest` fields the socket
operations already use, followed by these fields, which an external provider can read:

- `name`, a pointer and a length;
- `trust`, where 0 is System and 1 is Roots;
- `roots`, a borrowed buffer that is empty for System;
- `alpn`, a pointer to a run of length-prefixed entries, and its length;
- `protocol_out`, a caller buffer of capacity 255;
- `peer_out`, 32 bytes.

All of these stay pinned until delivery.

The callbacks work like the other socket operations:

- `issue` makes the first nonblocking attempt.
- When rustls needs the socket, the attempt returns `Pending` with the step's deadline,
  and registers readable or writable interest, whichever the session wants.
- Each readiness runs `resume`.
- `abort(Cancel)` gives `Aborted` and `abort(Deadline)` gives `TimedOut`. Both close the
  socket and free the session inside the callback.
- On success, `Outcome.value` is the TLS stream's key. Its two halves get the direction
  bits that `net.split` uses. `protocol_out.len` and `peer_out` are written.

`hirsel_core_host_tls_connect` is the C entry. It takes the context, the `Net`
designation, the socket key, the extension fields above and the `Until` triple, and
returns a `HirselCoreHostOutcome`.

**Plaintext I/O.** `RECV_BOUNDED` and `SEND_PAIR` use the handle of a TLS half. The
provider sees that the handle belongs to a TLS session and moves plaintext through that
session. Their request layouts, `Pending` use and abort meanings are unchanged.

**Errors.** `NetError` tag 9 is `Tls`. `IoError.code` carries the `TlsFailure`:

- the low byte is the case: Certificate 0, Name 1, Alpn 2, Roots 3, Protocol 4,
  Closed 5, Truncated 6;
- the second byte is the `CertFailure` case (UnknownIssuer 0, NameMismatch 1, Expired 2,
  NotYetValid 3, Invalid 4), or the alert code for `Protocol`.

**Providers.** The OS provider (`os.rs` and `os_net.rs`) implements both tags with rustls
and the `ring` crypto provider. It loads the host roots with `rustls-native-certs`. A
provider that does not implement TLS answers `TLS_CONNECT` with
`Tls(Protocol(0))`, and that is still a delivery. The evaluator calls the same
operations through its own call form, which by [CAP-5](capabilities.md#cap-5-provider-operations)
has no different behaviour. The scripted provider plays a peer script instead of doing
cryptography. A script states the handshake result (a `TlsFailure`, or a protocol and a
peer digest), then the plaintext the peer sends and the plaintext it expects. That is
enough for every law below that does not test rustls itself.

Server-side TLS is not a provider operation. A place's ingress belongs to its host,
which terminates inbound TLS in front of the program, so an accepted socket carries
plaintext. Client certificates are not in v1.
When they come, the client key is a `Secret` that only the provider reads.

### TLS-5: Secrets

```hirsel core
pub fn seal(take b: Bytes, to: String) -> Result[Secret, SecretError]
pub fn narrow(s: Secret, to: String) -> Result[Secret, SecretError]
pub fn render(s: Secret) -> View[String] in s
pub fn send_secret(mut w: WriteHalf, before: Bytes, s: Secret, after: Bytes, until: Until) -> Result[(), NetError] waits
pub type SecretError = Invalid | Capacity(Capacity)
```

**Ingestion.** A secret's bytes enter the run in one of two ways:

- **An entry grant.** `main` takes a `Secret` parameter, which the run configuration
  grants ([CAP-4](capabilities.md#cap-4-entry-grants)):

  ```sh
  hirsel run app --grant 'Secret:api_key=env:OPENAI_API_KEY to api.openai.com'
  hirsel run app --grant 'Secret:db=file:/run/secrets/db to *.db.internal,10.0.0.7'
  ```

  `Secret:<parameter>` names the `main` parameter. The launcher reads the environment
  variable or the file once, before the run starts. If the source is missing or empty,
  the run does not start (exit 103). The bytes are kept in run-owned host memory. They
  are zeroed when the run ends and are never charged to a limit. The program receives a
  designation, never the bytes. `to` is the list of name patterns the secret may be sent
  to (TLS-3 pattern syntax).
- **`secret.seal`.** This takes bytes the program already holds, such as a token a
  server minted during the run, together with a `to` pattern list. It moves them into
  run-owned storage that is charged to the caller's limit until the run ends, as
  `net.narrow` charges rule storage. A malformed pattern list gives `Invalid`. Once
  sealed, the program has no access to the bytes. Sealing does not remove copies the
  program made before sealing.

`secret.narrow` keeps the patterns of `s` that the new list also covers, using TLS-3's
intersection, and designates the same bytes. If the intersection is empty, the result
is `Invalid`.

**No cleartext accessor.** No operation returns a secret's bytes, their length, any
digest of them, or the result of comparing them. Because `Secret` is not `codec`, no
store or JSON codec encodes one. `secret.render` returns the fixed marker
`[secret]` for every secret. Every std function that renders, logs or describes a value
holding a secret writes this marker in the secret's place. This includes the HTTP
client's request and error renders. Run diagnostics, failure causes and the launcher's
grant summary print the parameter name and `[secret]`, never the source's contents.
Since program code never gets the bytes from a `Secret`, it can log or render copies
it had before sealing. Received plaintext is outside the TL10 guarantee because an
authorized peer can return any bytes, and a recording keeps its replies exactly
([recording](../recording.md#credentials)).

**The sink.** `tls.send_secret` sends `before ‖ secret ‖ after` on `w`. Every byte is
sent, or the operation fails. These checks run before issue, each with no effect:

1. If `w` is not a TLS half, the result is `Denied`.
2. If no pattern of `s` covers the verified name of `w`'s stream, the result is
   `Denied`.

The provider receives the bytes from run-owned memory. They never pass through program
memory or a buffer that the program can see.

Failure after a partial send is different from `send_pair`. On `TimedOut`, `Aborted` or
a transport error after some bytes were committed, the half is **spent**: every later
send on it gives `Reset`. The reason is that the caller cannot resume in the middle of
bytes it cannot read. A failure before any byte was committed leaves the half usable.

The HTTP client writes a header whose value is a `Secret` by calling `send_secret`. The
`before` argument carries the head up to the value, and `after` carries the rest of that
line, with any following head bytes up to the next secret value. On a plain-TCP
connection, such a request fails `Denied` before any byte is sent.

An HMAC or signature sink, which uses a secret as a key without sending it, is a later
operation. Its rule is set now: its output is a delivery that passes the replay point
([CAP-8](capabilities.md#cap-8-the-replay-point)), and its key never does.

## Laws

Each law names the bug it catches and the oracle its expected value comes from. Where a
harness is named, the cases go into it. Certificate fixtures are a test CA and two leaf
certificates, `alpha.test` and `beta.test`, generated with `openssl` and checked in
together with their SHA-256 leaf digests, which `openssl x509 -outform der | sha256sum`
computes.

| law | bug | oracle and setup |
|---|---|---|
| **TL1** wrong name | verification skipped, or SNI matched in place of the certificate | A loopback server presents `alpha.test`. `tls.connect` with name `beta.test` and roots holding the test CA gives `Tls(Certificate(NameMismatch))`. With name `alpha.test` it succeeds, and `Handshake.peer` equals the `openssl` digest of the alpha leaf split into four big-endian words. |
| **TL2** trust is exact | `Roots` falling back to the host store, or accepting any chain | The same server with `Roots` set to an unrelated CA, and with `System`: both give `Tls(Certificate(UnknownIssuer))`. Malformed or empty PEM gives `Tls(Roots)`, and the provider records no call. The oracle is the fixture's issuer. |
| **TL3** authority before effect | the handshake reaching the host without a covering rule | Using a counting provider, check that each of these gives `Denied` with zero provider calls past the socket: a name outside the rule's patterns, a peer port outside the range, trust `System` under a `Roots=` rule, a roots bundle whose digest differs from the pin, and an accepted socket. The expected verdicts are worked out by hand for a corpus of names against `*.a.test` and `x.a.test` (`a.test`, `x.a.test`, `y.x.a.test`, `X.A.TEST.`, `xa.test`). |
| **TL4** narrowing never widens | intersection returning the wider pattern | Over the TL3 name corpus, a pair of rules, and every order of two `narrow` calls: the set of names the result covers equals the intersection of the hand-computed sets. Put the cases in the existing NET-6 rule tests. |
| **TL5** committed prefix | counting plaintext as committed that the session later drops or sends twice | The peer reads slowly over a small socket buffer. Sends of a 1 MiB pattern with deadlines that expire part way through. The caller continues from `written` each time. The peer's received plaintext equals the pattern byte for byte, so nothing is lost or repeated. |
| **TL6** truncation | an unauthenticated EOF read as a clean end | The peer closes TCP after half of a body without `close_notify`, and the next receive gives `Tls(Truncated)`. With `close_notify`, the next receive gives `Ok(0)`. The oracle is RFC 8446 §6.1. |
| **TL7** release on every exit | an fd or session leaked on abort or failure | Check four cases: cancel during a handshake parked on a peer that never answers (`Aborted`), a deadline expiry (`TimedOut`), TL1's failure, and a refusal from TL3. After each one, the process's open descriptor count, read from `/proc/self/fd`, is back to its value before `connect`. |
| **TL8** ALPN | the wrong protocol reported, or a server choice outside the offer accepted | Offer `[h2, http/1.1]` to a server that lists `[http/1.1]`: the result is `Some(1)`. Against a server that lists only `[x]`: `Tls(Protocol(120))`. An entry of 0 bytes, an entry of 256 bytes, or a repeated entry: `Tls(Alpn)` before issue. The oracle is RFC 7301 §3.2. |
| **TL10** a secret never renders | the bytes reaching a render, log line or diagnostic | The grant `env:` source is 32 random bytes encoded as hex. The program makes an HTTPS request with it as `Authorization: Bearer`, renders the request, triggers and renders an HTTP error, and writes a `Log`. Search stdout, stderr and the run's diagnostics for the value, its base64 and its raw bytes: there are no hits. The loopback server's received header equals the environment value, and the server does not echo it. |
| **TL11** a secret reaches only its names | exfiltration to a host the grant did not name | Two cases: `send_secret` on a stream verified as `beta.test` with a secret `to alpha.test`, and `send_secret` on a plain-TCP half. Both give `Denied`, and the server receives zero bytes after the handshake. |
| **TL13** spent half | a resumed send that splits a secret | Make `send_secret` time out after it has committed a prefix (the scripted peer stops reading). The next `send_pair` on that half gives `Reset`. The scripted peer's received bytes are a prefix of `before ‖ secret ‖ after` and contain nothing after it. |

TL1 to TL8, TL10 and TL11 run against the OS provider over loopback. TL5, TL7 and TL13
also have scripted-provider cases, so that the evaluator is covered. The recording laws of
TLS sessions and secrets, RL3 to RL5, are with the tools ([recording](../recording.md#laws)).

## Build slices

Each slice lands finished. Sizes are S (under a day of one lane), M (one to three days)
and L (more than three).

| slice | size | depends on | contents | laws |
|---|---|---|---|---|
| **T1** OS TLS provider | L | — | core declarations (`tls.connect`, `Trust`, `Handshake`, `TlsFailure`, the `NetError.Tls` case); `TLS_CONNECT` and TLS halves in `os.rs` and `os_net.rs` over rustls with `ring` and `rustls-native-certs` (Buck third-party fixups); the `Tls` rule in grant parsing, `net.narrow` and the C rule struct; the C entry; the LLVM and Cranelift lowering of `tls.connect` | TL1–TL8 |
| **T2** recordings | M | T1 | in the recording build, `TLS_CONNECT` issue and delivery codecs, the TLS halves under the recorded `Net` grant, the format-4 header `Tls` rule record, credential-bearing file mode and export gate ([recording](../recording.md)) | RL3 |
| **T3** evaluator and scripted provider | M | T1 | the evaluator's call form for tag 22; peer scripts in the scripted provider (`model_net`) with handshake results and plaintext steps | scripted TL5, TL7 |
| **T4** Secret | M | T1, T2 | the `Secret` core type and grant kind 11, launcher sources `env:` and `file:`, `seal`, `narrow`, `render`, `SEND_SECRET` in the OS and scripted providers, its designation and grant record in a recording, the spent-half rule | TL10–TL13 |
| **T5** HTTPS client | S | T1, T4, http-client-1439 | std `tls` api page; `https` connect = `net.connect`, then `tls.connect` offering `http/1.1`, then the plain client's reader and writer over the halves; `Secret` header values written through `send_secret`; `[secret]` in request and error renders | TL10 (the client's case), and one HTTPS round trip in the existing http client harness |

T2 and T3 can run in parallel after T1. T4 waits for T2 because its designation in a
recording is part of its contract. T5 is last.
