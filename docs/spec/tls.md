# TLS and secrets

TLS is a provider operation. The provider performs the cryptography: the OS provider
uses rustls. A program never handles keys, records or certificates. A program asks for a
client handshake on a connected socket and gets back the two halves of an encrypted
stream. Those halves are the `ReadHalf` and `WriteHalf` of [Net](net.md). After the
handshake, `host.recv_bounded` and `host.send_pair` move plaintext, so code written over the
halves works over TCP and over TLS without change.

A `Secret` is a credential that program code cannot read. Its bytes are held by the run,
outside program memory. The one operation that sends them, `tls.send_secret`, writes them
on a TLS stream whose verified name the secret permits.

## Types

Module `tls` declares `connect` and `send_secret`. Module `secret` declares `seal`,
`narrow`, `share`, `render`, the MAC sink and the SCRAM sink (TLS-6).

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
| `Secret` | resource, mechanical | `send` | 8 | releases one holder (TLS-5) |

`Secret` is a capability type ([CAP-2](capabilities.md#cap-2-capabilities-are-opaque)).
It designates bytes that let a program act as an account. It has neither `key` nor
`codec`. So no program can compare two secrets, encode one, or store one in a type that
declares `codec` ([OWN-4](ownership.md#own-4-bounds)). Unlike the other capabilities it is
a resource ([OWN-3](ownership.md#own-3-kinds)) that holds run-owned storage, so `let` moves
it, dropping it releases that storage (TLS-5), and no `Rc` or `Arc` payload holds one. It
crosses to another unit as a handle ([CONC-15](tasks.md#conc-15-units)).

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
- **Resumption.** The OS provider may resume TLS 1.3 using an in-memory ticket from this
  run for the same verification name, trust configuration and ordered ALPN offer. It
  limits the retained configurations and tickets. A resumed server proves possession of
  the resumption secret. The provider uses the originally verified leaf for the peer
  digest and channel binding. A server that declines the ticket completes a full
  handshake with the usual certificate checks. TLS 1.2 always uses a full handshake.
  Early data is disabled.
- **Replay.** The recording captures the handshake outcome, negotiated ALPN, peer digest,
  channel binding and the results of subsequent plaintext operations. TLS wire records
  and tickets stay outside the recording. Replay delivers the captured results without
  a live handshake or access to the ticket cache ([CONC-15](tasks.md#conc-15-units)).
- **Channel binding.** The provider also computes the stream's **binding value**, RFC 5929
  §4.1's `tls-server-end-point`: the hash of the leaf's DER encoding with the hash function
  of the leaf's signature algorithm, where MD5 and SHA-1 are replaced by SHA-256. A leaf
  signed with SHA-256, SHA-384 or SHA-512 gives 32, 48 or 64 bytes. A leaf whose signature
  algorithm names no single hash, such as Ed25519, gives no binding value. The runtime keeps
  it on the socket record for TLS-6; no operation returns it.
- **STARTTLS.** `s` may be a socket that `net.join` rebuilt after a plaintext exchange
  ([NET-7](net.md#net-7-owned-connections)). The handshake treats it as it treats any
  socket from `host.connect`.

The following checks run in order, before issue. Each refuses with no host effect.

1. `s` was not made by `host.connect`: `Denied`. A socket from `accept` cannot be the
   client side of a handshake. A socket that `net.join` rebuilt keeps the origin of the
   socket it was split from.
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
| the host had already received bytes on the socket that nobody had read, when the handshake was issued | `Tls(Protocol(0))`, and no byte is sent. The provider checks with one nonblocking `MSG_PEEK` before it writes the ClientHello. A server never speaks first in a TLS handshake, so such bytes are plaintext injected before TLS started ([NET-7](net.md#net-7-owned-connections)). |
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

- **Receive.** `host.recv_bounded` appends decrypted plaintext only: at most
  `max_receive` bytes, and never more than the caller's spare capacity. If plaintext is
  already decrypted and buffered, the receive completes without socket I/O. Otherwise it
  waits until a whole record has arrived, so a readable socket can still leave it
  pending. A delivery of 0 bytes means the peer sent `close_notify`. If the transport
  ends without `close_notify`, the receive gives `Tls(Truncated)` (RFC 8446 §6.1). After
  a `Tls` failure, every later receive on that half gives the same failure.
- **Send.** `host.send_pair` returns the number of plaintext bytes the TLS stream
  committed. A byte is committed when the session has taken it into a record. The
  session sends the ciphertext of committed bytes before the ciphertext of any later
  send, and before `close_notify`. `Ok(n)` means `n` bytes are committed. If the deadline
  passes or cancellation arrives after some bytes were committed, the send returns
  `Ok(n)`. If it committed zero bytes, it returns `TimedOut` or `Aborted`. The session
  holds at most the connection's slot cap of committed ciphertext beyond what the
  socket has accepted. A send that commits its entire input waits for its final
  ciphertext to reach the socket, subject to the deadline and cancellation rules
  above. The next send or drop flushes any outstanding ciphertext first. Each byte
  reaches the peer exactly once when the caller continues from the returned count.
- **Both directions.** One TLS session serves both halves. The provider serialises the
  two halves' use of it, but only inside a callback and never across a wait. So a
  receive in one task and a send in another proceed together, as they do on a plain
  socket. Records that the session must send in reply, such as a key update response,
  are sent by the next callback that can write.
- **Drop.** Dropping a TLS write half queues `close_notify` and makes one nonblocking
  attempt to send it with any committed ciphertext, then performs `SHUT_WR`. After a
  send that ended in `Aborted` or `TimedOut`, with no later send, the close resets the
  connection ([NET-7](net.md#net-7-owned-connections)), so committed ciphertext the
  peer has not taken may be dropped. Like the
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

Two operation tags are added for the handshake and the secret sink, and two more for the
SCRAM sink (TLS-6), which takes the next two free codes in `hirsel-core-ids`'s `host_op`. Everything else on a TLS stream uses the existing socket
operations.

| tag | operation | states |
|---|---|---|
| 22 | `TLS_CONNECT` | C, I, D |
| 23 | `SEND_SECRET` | C, I, D |
| 68 | `SCRAM` (`secret.scram_sha256`) | C, I, D |
| 69 | `SCRAM_VERIFY` (`secret.scram_verify`) | C, D |
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
- `peer_out`, 32 bytes;
- `binding_out`, a caller buffer of capacity 64, whose length stays 0 when the leaf gives
  no binding value (TLS-1).

All of these stay pinned until delivery.

The callbacks work like the other socket operations:

- `issue` makes the first nonblocking attempt.
- When rustls needs the socket, the attempt returns `Pending` with the step's deadline,
  and registers readable or writable interest, whichever the session wants.
- Each readiness runs `resume`.
- `abort(Cancel)` gives `Aborted` and `abort(Deadline)` gives `TimedOut`. Both close the
  socket and free the session inside the callback.
- On success, `Outcome.value` is the TLS stream's key. Its two halves get the direction
  bits that `net.split` uses. `protocol_out.len`, `peer_out` and `binding_out` are written.

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
pub fn seal(take b: Bytes, uses: String) -> Result[Secret, (SecretError, Bytes)]
pub fn narrow(s: Secret, uses: String) -> Result[Secret, SecretError]
pub fn share(s: Secret) -> Secret
pub fn render(s: Secret) -> View[String] in s
pub fn send_secret(mut w: WriteHalf, wire: Bytes, secrets: List[Secret], marks: List[u64], until: Until) -> Result[(), NetError] waits
pub type SecretError = Invalid | Capacity(Capacity)
pub fn wrap(kek: Secret, s: Secret, mut out: Bytes) -> Result[u64, KeyError]
pub fn unwrap(kek: Secret, blob: Bytes, uses: String) -> Result[Secret, KeyError]
pub fn sign(key: Secret, alg: SignAlg, msg: Bytes, mut out: Bytes) -> Result[u64, KeyError]
pub type KeyError = Denied | Invalid | Tampered | Capacity(Capacity)
pub type SignAlg = Ed25519 | P256 | Rs256
```

**Uses.** A secret is opaque host material together with the typed set of uses it
permits, its **policy**. A policy's text is its uses separated by single spaces, each at
most once:

| Use | Permits |
| --- | --- |
| `tls_send(<patterns>)` | `tls.send_secret` and `secret.scram_sha256` on a stream whose verified name one of the comma-separated TLS-3 patterns covers |
| `mac` | `secret.hmac_sha256` and `secret.verify_hmac_sha256` (the MAC sink, below) |
| `verify` | `secret.verify_hmac_sha256` only |
| `kdf` | `secret.derive_hmac_sha256` |
| `sign(<alg>)` | `secret.sign` with `<alg>`, one of `ed25519`, `p256` and `rs256` (key operations, below) |
| `wrap` | `secret.wrap` and `secret.unwrap` with the secret as the key-encryption key (KEK) |
| `mtls_client_id` | use as a TLS client identity |

`mac` includes `verify`, so the two are not written together. A name is a destination
only inside `tls_send(...)` and a use word only outside it, so `tls_send(mac)` sends to
the host `mac` and permits no MAC. A policy with no use, a word given twice, an unknown
word or a malformed pattern list is `Invalid`. The canonical text, which recordings and
diagnostics print, writes `tls_send` first, then `mac` or `verify`, `kdf`, the `sign` uses in the
order above, `wrap` and `mtls_client_id`, with each pattern as TLS-3 prints it. No
operation yet reads `mtls_client_id`: a policy may hold it and `narrow` intersects it, so
the policy's shape is final.

**Ingestion.** A secret's bytes enter the run in one of two ways:

- **An entry grant.** `main` takes a `Secret` parameter, which the run configuration
  grants ([CAP-4](capabilities.md#cap-4-entry-grants)):

  ```sh
  hirsel run app --grant 'Secret:api_key=env:OPENAI_API_KEY to api.openai.com'
  hirsel run app --grant 'Secret:db=file:/run/secrets/db to *.db.internal,10.0.0.7'
  hirsel run app --grant 'Secret:hook=env:WEBHOOK_KEY verify'
  ```

  `Secret:<parameter>` names the `main` parameter. The launcher reads the environment
  variable or the file once, before the run starts. If the source is missing or empty,
  the run does not start (exit 103). The bytes are kept in run-owned host memory. They
  are zeroed when the run ends and are never charged to a limit, and releasing a holder
  of a granted secret never releases them. The program receives a
  designation, never the bytes. The text after the source is the grant's policy.
  `to <patterns>`, with no space in `<patterns>`, is the command line's spelling of
  `tls_send(<patterns>)`, and other uses may follow it, so the first grant above is
  `tls_send(api.openai.com)`. The policy is the longest space-separated suffix of the
  text that parses as a policy, so a file path may hold spaces.
- **`secret.seal`.** This takes bytes the program already holds, such as a token a
  server minted during the run, together with a policy in canonical spelling (no `to`
  sugar). It moves them into
  run-owned storage charged to the caller's current limit. Sealing admits
  `round_up_8(pattern_bytes + len(b) + 32)` using the length of `b` before copying.
  `pattern_bytes` is the sum, over the `tls_send` patterns, of 32 plus the bytes of the name for a
  DNS name or a wildcard's suffix, and 32 for an IP literal or `*`; other uses add nothing.
  A policy that is `Invalid` returns `Error((Invalid, b))`. If
  admission refuses, sealing returns `Error((Capacity(c), b))` with the requested
  charge and available space. Either refusal returns the original bytes with their
  original payer and charge. The call admits nothing and retains neither the bytes
  nor the policy. On success, sealing copies the bytes, then zeroes and refunds the
  owned source storage. A sealed secret that crosses to another unit
  ([CONC-15](tasks.md#conc-15-units)) designates the same bytes and keeps its policy. The program can still access any copies it made before sealing.

`secret.narrow` keeps the uses of `s` that the new policy also permits and designates the
same bytes. Its `tls_send` patterns are the TLS-3 intersection of the two lists, so
`tls_send` remains when that is not empty. `mac` against `verify` gives `verify`, and
every other use remains when both policies hold it. If no use remains, or `uses` is
`Invalid`, the result is `Invalid`. It admits `round_up_8(pattern_bytes + 32)` for the new designation, and a
refusal returns `Capacity(c)`.

**Holders and release.** Each `Secret` value is one **holder** of a **designation**: the
bytes it designates together with its policy. `seal`, `derive_hmac_sha256`, `narrow` and `provide.secret`
([core](core.md#provide)) each make a new designation with one holder.
`secret.share(s)` returns one more holder of the designation of `s`, as `share` adds a
holder of an `Rc` ([MEM-16](memory.md#mem-16-counting)): it admits nothing, never fails
and never waits. Dropping a `Secret` removes its holder. When the last holder of a
designation is dropped, the designation is **released**: the charge that `narrow` or
`provide.secret` admitted for it is refunded. The bytes of a sealed secret stay while
any designation of them is held, since `narrow` and `provide.secret` designate the same
bytes. When the last of them is released, the runtime zeroes the bytes and refunds the
charge that `seal` or `derive_hmac_sha256` admitted. So a program that seals a fresh token every hour and drops
the old one holds the storage of one token, not of every token it has sealed.

A release is not a provider operation: it passes no replay point, takes no step, cannot
fail and adds no frame to a recording. A designation is recorded by its origin (the
grant, seal ordinal or derivation delivery position), never by where the runtime keeps it, so a release
changes no recorded value ([recording](../recording.md#credentials)). Each charge is paid
by the limit that admitted it ([MEM-19](budgets.md#mem-19-limits)). A refund from
another unit reaches that limit as any cross-unit refund does
([MEM-20](budgets.md#mem-20-limit-records-and-hand-over)). When a helper unit ends while a
designation it made is still held, in it or in another unit, the designation's charges
stay with the helper's owner, as the charges of its open connections do
([CONC-15](tasks.md#conc-15-units)), until the designation is released.

**No cleartext accessor.** No operation returns a secret's bytes, their length, any
unkeyed digest of them, or the result of comparing them. The MAC sink's keyed tag and
verdict, the SCRAM sink's verdict, `wrap`'s blob (ciphertext under the KEK) and `sign`'s
signature are the only values computed from them that reach the program. A derivation
returns an opaque designation. The SCRAM proof goes only onto an authorized stream (TLS-6). Because `Secret` is not `codec`, no
store or JSON codec encodes one. `secret.render` returns the fixed marker
`[secret]` for every secret. Every std function that renders, logs or describes a value
holding a secret writes this marker in the secret's place. This includes the HTTP
client's request and error renders. Run diagnostics, failure causes and the launcher's
grant summary print the parameter name and `[secret]`, never the source's contents.
Since program code never gets the bytes from a `Secret`, it can log or render copies
it had before sealing. A child process started through `Exec` is outside this guarantee: it
runs with the OS user's authority and may read a granted secret's source
([EXEC-1](exec.md#exec-1-an-exec-grant-confers-what-its-programs-can-do-and-hirsel-does-not-confine-them)),
and may rewrite a recording of the run. What it is denied is the tracing-gated access to
the run's private memory and descriptors, which hold a sealed secret's bytes, and that only
while the run stays non-dumpable; it can still read any file or shared memory that holds
those bytes, and the run's public process metadata
([EXEC-7](exec.md#exec-7-a-run-granted-exec-keeps-its-children-out-of-its-private-state)).
Received plaintext is outside the TL10 guarantee because an
authorized peer can return any bytes, and a recording keeps its replies exactly
([recording](../recording.md#credentials)).

**The sink.** `tls.send_secret` sends one message built from `wire`, the plain bytes,
by inserting the secrets and the lengths that `marks` names. The runtime builds the message;
the program never sees it. `marks` is a sequence of groups of four words `[at, kind, a, b]`.
`at` is an offset into `wire`, and the message is `wire` with each group's insertion
placed before `wire[at]` (at `at = len(wire)`, after the last byte). Groups are in
nondecreasing `at` order, and insertions at the same offset are placed in list order.

| kind | inserts | `a` | `b` |
| --- | --- | --- | --- |
| 0 | the bytes of `secrets[a]` | index | 0 |
| 1 | `secrets[a]` form-urlencoded (below) | index | 0 |
| 2 | the decimal digits of the length of the range `a..b` | start | end |
| 3 | the lowercase hexadecimal digits of the length of the range `a..b` | start | end |

**Lengths.** The length of the range `a..b` (`a <= b <= len(wire)`) is the number of
message bytes made from `wire[a..b]` together with the secret insertions whose `at` lies
in `a..=b`, so a secret at either end of the range counts. Digits have no leading zero;
a length of zero is `0`. A length range may not hold the `at` of a length group. So an
HTTP head can carry `Content-Length` for a body that holds secrets: the group sits where
the value goes, and its range is the body. The program never learns the number.

**Form-urlencoding** is the `application/x-www-form-urlencoded` byte serializer (WHATWG
URL §5.2). The bytes `*`, `-`, `.`, `_`, `0`–`9`, `A`–`Z` and `a`–`z` stay as they are.
A space becomes `+`. Every other byte becomes `%` followed by two uppercase hexadecimal
digits. The runtime encodes the secret's bytes; they never pass through program memory.

`marks` is well formed when its length is a multiple of four, every `at` and range lies in
`wire`, the offsets are in order, every index names an element of `secrets`, `b` is 0 for
kinds 0 and 1, and no range holds a length group. Each kind is checked from the arguments
alone, never from a secret's bytes. A `marks` that is not well formed fails the calling
task with a `Bounds` cause, as an out-of-bounds index does; it is a fault of the program,
not a refusal by the host. A `secrets` element that no group names is still checked below.

These checks run before issue, each with no effect:

1. If `w` is not a TLS half, the result is `Denied`.
2. If, for any element of `secrets`, no `tls_send` pattern covers the verified name of
   `w`'s stream, the result is `Denied`. Every element is checked before any byte is sent,
   so a refusal sends none of the message.

Every byte of the message is sent, or the operation fails. The provider receives the
secrets' bytes from run-owned memory, and the runtime builds the encodings and the length
digits in run-owned memory, which it zeroes after the send and never charges to a limit:
a refusal for capacity would tell the program the length. They never pass through program
memory or a buffer that the program can see.

**Virtual streams.** A TLS half of a `Net` made by `provide.net` ([core](core.md#provide))
was verified by its server, not by a trust rule the run was granted, so it never satisfies
a granted secret: `send_secret` on it gives `Denied` before issue when any element of
`secrets` is opaque (from an entry grant, or derived or unwrapped from one; key operations,
below), whatever its patterns, and the server sees nothing. A `Secret` made by
`secret.seal` passes the two checks above as usual. Its server receives the operation as
`SendSecret` with `wire` and `marks` as payload fields 1 and 2 and each element of `secrets`
itself, narrowed to the stream's verified name, and never its bytes
(`provide.secret(c, i)`). A server that forwards the call to a real `tls.send_secret`, with
the same `wire`, the same `marks` and the narrowed secrets in order, passes that real
stream's checks again.

Failure after a partial send is different from `send_pair`. On `TimedOut`, `Aborted` or
a transport error after some bytes were committed, the half is **spent**: every later
send on it gives `Reset`. The reason is that the caller cannot resume in the middle of
bytes it cannot read. A failure before any byte was committed leaves the half usable.

The HTTP client sends a request that holds a `Secret`, in a header or in its body, with
one `send_secret` call: `wire` is the head and any body bytes given with it, kind 0 marks
place header values, kind 0 or 1 marks place body segments, and a kind 2 mark writes
`Content-Length` ([http](http.md#http-17-client-request-bodies)). On a plain-TCP
connection, such a request fails `Denied` before any byte is sent.

**The MAC sink.** `secret.hmac_sha256(s, data)` and `secret.verify_hmac_sha256(s, data,
tag)` use a secret as an HMAC-SHA256 key without sending it
([hmac](../../core/std/api/hmac.md#secret-keys)). The runtime computes from run-owned
memory. The tag, or the constant-time verdict for exactly 32 tag bytes, is a delivery
that passes the replay point ([CAP-8](capabilities.md#cap-8-the-replay-point)), and the
key never does. `hmac_sha256` needs the `mac` use, and `verify_hmac_sha256` needs `mac`
or `verify`. Without the use, the result is `Denied` before issue, with no effect. A tag over chosen data is a
signing oracle wherever it is sent, which is why the use must be granted. The key
operations below follow the same rule.

**Key operations.** `secret.wrap`, `secret.unwrap` and `secret.sign` use a secret as a key
without revealing it. A connector stores an OAuth refresh token as `wrap`'s blob in a
`store` row, so the token commits atomically with its account row, and opens it with
`unwrap` when it needs it. The runtime computes from run-owned memory with ring's
primitives; Hirsel has no cryptographic code of its own.

A secret is **opaque** when the program never held its bytes: one from an entry grant, one
derived (`derive_hmac_sha256`) from an opaque parent, or one unwrapped under an opaque KEK.
A sealed secret, and one derived from or unwrapped under one that is not opaque, is not. The program holds a sealed KEK's bytes and could open the blob itself, so
wrapping an opaque secret under a KEK that is not opaque is `Denied`.

`wrap(kek, s, out)` appends to the spare capacity of `out` the blob

```text
[1] [len(policy) u32 LE] [policy] [salt 32] [ciphertext len(s)] [tag 16]
```

and answers its length, `1 + 4 + len(policy) + 32 + len(s) + 16`. `policy` is the
canonical text of `s`'s policy. The construction is ChaCha20-Poly1305 (RFC 8439) under a
key derived per blob: HKDF-SHA256 (RFC 5869) with the 32 random salt bytes as salt, the KEK
as input keying material and `hirsel secret.wrap 1 chacha20-poly1305` as info. The nonce
is twelve zero bytes. The reason is the nonce: ring offers AES-GCM and ChaCha20-Poly1305,
neither with a nonce long enough to draw at random for every wrap under one long-lived
KEK, and neither XChaCha20 nor AES-GCM-SIV. A fresh 256-bit salt gives every blob its own
key, so each key seals one message and its fixed nonce is never reused; two salts
collide with negligible probability. ChaCha20-Poly1305 rather than AES-GCM, because it is
constant time without AES instructions. The associated data is the bytes
`hirsel secret.wrap 1` and a zero byte, then the header up to the ciphertext: the
version, the policy and the salt. So the policy is authenticated with the bytes and
cannot be swapped, and the version names the construction.

`unwrap(kek, blob, uses)` opens a blob under `kek` and answers a new designation with one
holder, policy `uses` and the opened bytes. `uses` must be within the policy the blob
binds: each use of `uses` is permitted by the bound uses, and each of its `tls_send`
patterns is covered by one bound pattern (TLS-3 subset), so an unwrap can narrow but never
widen. The check runs after the tag is verified, on the authenticated policy. Unwrapping
admits `round_up_8(pattern_bytes + len + 32)` before issue, as `seal` does, where `len` is
the plaintext length the blob's header implies (0 for a blob too short for one). A failure
refunds it. On success it is the new secret's seal charge: it is refunded when the last
designation of the unwrapped bytes is released, and the bytes are zeroed then.

`sign(key, alg, msg)` appends the signature of `msg` to the spare of `out` and answers its
length. The key's bytes are a PKCS#8 private key (RFC 5958), given by the grant or the
seal, and `sign(<alg>)` in its policy fixes the algorithm:

| `alg` | scheme | key | signature |
| --- | --- | --- | --- |
| `Ed25519` | Ed25519 (RFC 8032) | PKCS#8 v1 or v2 with the RFC 8410 OID | 64 bytes |
| `P256` | ECDSA over P-256 with SHA-256 (JWS `ES256`), with a fresh random nonce | PKCS#8 with an RFC 5915 EC key | 64 bytes, `r ‖ s` |
| `Rs256` | RSASSA-PKCS1-v1_5 with SHA-256 (RFC 8017 §8.2, JWS `RS256`) | PKCS#8 RSA key of 2048 to 4096 bits | the modulus length |

The results, decided in this order:

- `Denied` before issue, with no effect: the key lacks `wrap` (for `wrap` and `unwrap`) or
  the `sign(<alg>)` for `alg`, or `wrap` would put an opaque secret under a KEK that is
  not.
- `Invalid` before issue: `uses` is not a policy.
- `Capacity(c)` before issue: `unwrap`'s admission is refused, with the requested charge
  and the space available.
- `Invalid`: a KEK shorter than 32 bytes, or a signing key that is not PKCS#8 for `alg`.
- `Tampered`: the blob is not one this KEK wrapped, unaltered. A bad version, header or
  tag, a different KEK and any changed byte give the same answer, and nothing more.
- `Denied`: `unwrap`'s `uses` is not within the bound policy.
- `Capacity(c)`: the blob or signature does not fit the spare of `out`, with its length
  as `requested` and the spare as `available`; nothing is appended.

The last four are deliveries. The blob, the signature and the opened designation pass the
replay point ([CAP-8](capabilities.md#cap-8-the-replay-point)); the key's bytes and the
opened bytes never do. A recording keeps the blob and the signature as outputs and never
key material, and a replay delivers them from the tape
([recording](../recording.md#credentials)). An unwrapped secret is designated by the order
of its unwrap among the run's unwraps; on replay it has no bytes, like a grant, and every
use of it replays from the recording. Its charge follows CONC-15 as a seal's does: it is
paid by the limit current at the unwrap, and when that unit ends while the secret is
held, in it or in another unit, the charge stays with the unit's owner until the release.

**The HMAC derivation sink.**

```hirsel core
pub fn derive_hmac_sha256(s: Secret, key_prefix: Bytes, data: Bytes, uses: String) -> Result[Secret, SecretError]
```

The runtime computes HMAC-SHA256 over `data` with `key_prefix` followed by the bytes
of `s` as the key. It keeps the resulting 32 bytes as a new sealed secret. The
parent must permit `kdf`. The child policy must be nonempty and a subset of
`mac kdf`, including the `verify` use that `mac` permits. A refused or malformed
policy gives `Invalid` before issue. The runtime admits 64 bytes for the child
under the caller's current limit and returns `Capacity(c)` if admission refuses.

The call never waits. A successful result passes the replay point. A recording
holds the parent designation, the child policy, `key_prefix` and `data`, and a
success delivery. Replay makes the child designation without key material.
The runtime zeroes derived bytes when their last designation is released.

### TLS-6: SCRAM-SHA-256

```hirsel core
pub opaque resource type Scram
pub type ScramError = Denied | Transcript | Iterations | Binding | Net(NetError)
pub fn scram_sha256(mut w: WriteHalf, s: Secret, client_first: Bytes, server_first: Bytes, before: Bytes, after: Bytes, until: Until) -> Result[Scram, ScramError] waits
pub fn scram_final_len(w: WriteHalf, client_first: Bytes, server_first: Bytes) -> Result[u64, ScramError]
pub fn scram_verify(take c: Scram, server_final: Bytes) -> bool
```

These three are in module `secret`. They authenticate with a `Secret` password by
SCRAM-SHA-256 (RFC 5802 with RFC 7677's hash), as PostgreSQL's `scram-sha-256` method
needs, without the password or any key derived from it reaching program memory.

**What must stay inside.** RFC 5802 §3 derives, from the password and the server's salt and
iteration count:

- `SaltedPassword = Hi(Normalize(password), salt, i)`, which is PBKDF2-HMAC-SHA256 with a
  32-byte output;
- `ClientKey = HMAC(SaltedPassword, "Client Key")`, `StoredKey = H(ClientKey)` and
  `ServerKey = HMAC(SaltedPassword, "Server Key")`;
- `ClientProof = ClientKey XOR HMAC(StoredKey, AuthMessage)` and
  `ServerSignature = HMAC(ServerKey, AuthMessage)`.

Every one of these except the proof the server receives must stay with the run's secret
bytes. `SaltedPassword` reproduces both keys, so it is as good as the password against
that server. `ClientKey` authenticates as the client. `StoredKey` together with any captured
proof gives `ClientKey`, by computing the client signature from the transcript and XORing it
out. `ServerKey` impersonates the server. So Hirsel has no general PBKDF2 or SCRAM-key
operation over a `Secret`. An HMAC tag with `SaltedPassword` as key over `"Client Key"` would
be `ClientKey` itself. The one SCRAM operation takes the exact transcript bytes, emits the
proof only onto the authorized connection, and keeps the expected server signature in a
host-held `Scram` that answers only with a verdict.

**Flow.** The program builds `client-first-message` itself, taking the nonce from
`host.random` (18 random bytes in base64, as libpq and pgx use, is enough), and sends it.
It receives `server-first-message`. It then calls `scram_sha256`, which sends
`before ‖ client-final-message ‖ after` on `w`, and later passes the server's
`server-final-message` to `scram_verify`. `before` and `after` carry the framing the
protocol wraps around the message. For PostgreSQL, `before` is the `SASLResponse` header
`p` with its 4-byte length, and `after` is empty. The program learns the message's length
from `scram_final_len`, which runs the same checks as `scram_sha256` (below) except the
two checks on the secret, and returns the exact byte length of the `client-final-message`
that `scram_sha256` would send for these arguments.

**Checks.** These run in order before issue. Each refuses with no host effect and no step
beyond the call's own:

1. `w` is not a TLS half: `Denied`. SCRAM over plain TCP gives an attacker on the path
   material for offline password guessing, and channel binding needs TLS.
2. No `tls_send` pattern of `s` covers the verified name of `w`'s stream: `Denied`. On a TLS half of a
   `provide.net` `Net`, a `Secret` from an entry grant also gives `Denied`, as for
   `send_secret` ([TLS-5](#tls-5-secrets)). `tls_send` is the only use needed. The operation sends a
   value computed from the password only to a host the password itself may be sent to,
   and that value tells the host less than the password does.
3. The transcript is malformed: `Transcript`. `client_first` must be a GS2 header followed
   by `client-first-message-bare` (RFC 5802 §7):
   - The GS2 header is exactly `n,,`, `y,,` or `p=<cb-name>,,`. An authorization identity
     (`a=`) is refused.
   - The bare part is `n=<saslname>,r=<c-nonce>` and nothing after it. The `saslname` may be
     empty, as PostgreSQL sends it.
   - The nonce is one or more printable ASCII bytes other than `,` (0x21–0x7E without 0x2C).

   `server_first` must be `r=<nonce>,s=<salt>,i=<count>`, optionally followed by `,` and
   extensions:
   - It must not start with `m=`.
   - `<nonce>` must start with the exact `c-nonce` bytes and be strictly longer, with the
     same byte rule.
   - `<salt>` is padded standard base64 that decodes to at least one byte.
   - `<count>` is 1 to 10 decimal digits with no leading zero.
4. `<count>` is outside 4096 to 1,000,000 inclusive: `Iterations`. RFC 7677 §4 sets
   4096 as the minimum, and it is PostgreSQL's default. A lower count chosen by a server
   or by an attacker makes a captured exchange cheap to guess offline. The ceiling bounds
   the work an untrusted server can demand, which is about a second of one core at
   1,000,000. It is above the 600,000 that OWASP recommends for stored PBKDF2-HMAC-SHA256.
5. The GS2 header is `p=` with a `cb-name` other than `tls-server-end-point`, or `w`'s
   stream has no binding value (TLS-1): `Binding`. `tls-unique` is undefined for TLS 1.3,
   and PostgreSQL offers only `tls-server-end-point`.

**Charge.** When the checks pass, the call takes one more step for each admitted
iteration, `<count>` steps in all, at the call and before issue
([MEM-30](budgets.md#mem-30-step-bounds)). If the current bound has fewer left, it raises
`OutOfTime` there, having derived and sent nothing. With no bound current, the steps use
nothing, as every step does. Slice ends among them fall at the call, before the
derivation. The call then admits 32 bytes of run-owned storage for the `Scram` from the
current limit, and a refusal raises `Capacity` with nothing sent. The storage is refunded
when the `Scram` is consumed or dropped, or at once when the operation fails, since no
`Scram` then exists. Nothing else is admitted.

**Issue.** From the run-owned bytes of `s`, the runtime computes:

- **Normalize.** The password is prepared with SASLprep (RFC 4013) as PostgreSQL's
  `pg_saslprep` does. If the bytes are not UTF-8, or they contain a code point SASLprep
  prohibits, the raw bytes are used unchanged, so a password set through PostgreSQL works
  either way.
- **Message.** `client-final-message-without-proof` is
  `c=<base64(gs2-header ‖ cbind-data)>,r=<nonce>`. `gs2-header` is the exact prefix of
  `client_first`. `cbind-data` is the stream's binding value for `p=`, and empty for `n` and
  `y`. `AuthMessage` is `client-first-message-bare ‖ "," ‖ server_first ‖ "," ‖
  client-final-message-without-proof`.
- **Proof and signature.** The runtime computes `ClientProof` and `ServerSignature`, keeps
  `ServerSignature` in the `Scram`'s storage, and drops everything else. Hirsel makes no
  promise that the memory is wiped. It then sends `before ‖ client-final-message-without-proof
  ‖ ",p=" ‖ base64(ClientProof) ‖ after` on `w`.
- **Sending.** The send works as for `send_secret`: every byte is sent or the operation
  fails with `Net(e)`. If it fails after committing some bytes, the half is spent. The
  derivation runs at issue, before the first send attempt, and is not a cancellation point.

`client-final-message` is
`2 + len(base64(gs2-header ‖ cbind-data)) + 3 + len(nonce) + 3 + 44` bytes long, and that is
what `scram_final_len` returns.

**Verdict.** `scram_verify` consumes `c` and returns `true` exactly when `server_final` is
`v=` followed by 44 base64 characters that decode to the kept `ServerSignature`, then either
the end or `,` and extensions. The comparison runs in constant time. `e=<error>`, a wrong
signature, a wrong length or anything else gives `false`. It never waits or fails, and it
refunds the `Scram`'s storage. A client accepts the server, as RFC 5802 §5 requires, only
after `true`. Dropping a `Scram` unverified refunds it and reveals nothing.

**Virtual streams.** A SCRAM exchange on a TLS half of `provide.net` with a sealed `Secret`
runs the checks and the derivation above in the runtime, which holds the sealed bytes. The
bytes go to the server as ordinary `Send` calls on the half: the first one offers the whole
message as `prefix` with an empty `tail`, and each later one offers the bytes after the
committed count, until all are committed or a reply fails ([core](core.md#provide)). A
virtual stream has no binding value, so `p=` gives `Binding`. The program already held
the sealed bytes, so the server's view of the proof adds nothing. The scripted provider
plays the client-final message as plaintext the peer expects, and its handshake result may
state a binding value.

**Replay.** On a host TLS half, `scram_sha256` and `scram_verify` are provider operations
([CAP-5](capabilities.md#cap-5-provider-operations)) under the run-owned secret sink, and
they pass the replay point at issue and at delivery
([CAP-8](capabilities.md#cap-8-the-replay-point)):

- The issue of `scram_sha256` holds the half's ordinal, the secret's designation, the
  canonical bytes of `client_first`, `server_first`, `before` and `after`, and `until`. Its
  delivery holds the outcome, and the committed count on failure.
- The issue of `scram_verify` holds the `Scram`'s ordinal and the canonical bytes of
  `server_final`. Its delivery holds the verdict.
- `scram_final_len` is computed from its arguments and the stream's record, and adds no
  frame.

On a TLS half of `provide.net`, both are computed by the run, as every virtual provider's
results are ([core](core.md#provide)). They pass no replay point. A replay recomputes the
proof from the sealed bytes, which it recomputes from the deliveries they came from
([recording](../recording.md#credentials)), and sends the same `Send` calls to the server.
It recomputes the verdict of a `Scram` made there too.

A recording never holds the password, its normalized bytes, `SaltedPassword`, `ClientKey`,
`StoredKey`, `ServerKey`, the proof or the expected signature. A replay delivers the
recorded outcome and verdict from a `Scram` with no bytes behind it
([recording](../recording.md#credentials)).

The server's `v=` is also received plaintext in program memory, since the program reads it
before passing it to `scram_verify`. So guessing material is in the program as it is on the
wire; the sink keeps out only what would let the program authenticate without the server.
What a tape does hold is the received plaintext, which includes the salt, the iteration
count, the server's nonce and the server's `v=`. With the client-first message that the
same tape's sends describe, that is enough to test password guesses offline at `<count>`
PBKDF2 iterations per guess. A proof with its transcript would be the same. That is why
the proof is never stored and the tape is credential-bearing: it is readable only by its
owner, created at a fresh path, and exported only with `--include-credentials`.

### TLS-7: Credential grants

```hirsel core
pub type CredError = Room(key_id: u64, token: u64) | Net(NetError) | Token(IoError) | Status(u16) | Malformed
pub fn fetch(c: Cred, mut key_id: String, mut token: String, until: Until) -> Result[(Secret, u64), CredError] waits
```

These are in module `cred`. A `Cred` is a plain capability that designates one
**credential source**: a host endpoint that issues short-lived AWS-style credentials.
`cred.fetch` obtains a fresh set from it. The secret access key arrives as a `Secret`
that the host sealed itself, so no byte of it is ever in program memory. The access key
id, the session token and the expiry are public, and the program receives them as
ordinary values. A session token authorizes nothing without a signature made with the
secret key, and SigV4 hashes it into the canonical request, so it needs no sealing.

**Why a grant.** Authority to obtain credentials enters by grant (CAP-4), not by a `Net`
rule. A `Net` that reaches the metadata endpoint or STS would also let the program fetch the
same response itself and read the key in plaintext, so keeping the key out of program
memory would be a library convention, not a property of the run. With a `Cred` grant, only
the host provider reaches the endpoint, and the run needs no `Net` rule for it. A static key
is already a `Secret` grant (TLS-5) with a public key id, so it is not a credential source:
a second way to grant the same bytes would make two rules for one thing.

**Grants.** `main` takes a `Cred` parameter, which the run configuration grants:

```sh
hirsel run app --grant 'Cred:aws=imds mac'
hirsel run app --grant 'Cred:aws=imds:127.0.0.1:8123 mac'
hirsel run app --grant 'Cred:aws=sts:arn:aws:iam::123456789012:role/app file:/var/run/secrets/token mac'
```

`Cred:<parameter>` names the `main` parameter. The text after `=` is the source, then the
policy of every secret key fetched from it, in TLS-5's command-line spelling. The policy is
the longest space-separated suffix that parses as one, as for a `Secret` grant. The
sources are:

- `imds`: the instance metadata service, IMDSv2, at `169.254.169.254` port 80.
  `imds:<ipv4>:<port>` names another address, so a test can run its own server.
- `sts:<role-arn> [at:<name>:<port>] [roots:<pem-path>] file:<token-path>`: STS
  `AssumeRoleWithWebIdentity` with the web identity token in `<token-path>`. The endpoint is
  `sts.amazonaws.com` port 443 with the host's roots (TLS-3's `System`) unless `at:` and
  `roots:` name others. `<token-path>` comes last and may hold spaces.

Any other source, `static` included, is a misspelt grant: like a misspelt `Secret` or `Net`
grant, the command reports the grant syntax and exits 2 before it reads the program. The launcher reads nothing from a source before the run starts.
The token file is read by the provider at each fetch, since the identity provider rotates
it, and its bytes never enter program memory either.

**The fetch.** `cred.fetch` waits, and is a cancellation point at its issue
([CONC-7](tasks.md#conc-7-cancellation-points)). At issue, the provider starts the exchange
on a host helper and the task parks. The provider:

- For `imds`, sends three HTTP/1.1 requests, each on its own connection with
  `Connection: close`. The first is `PUT /latest/api/token` with
  `X-aws-ec2-metadata-token-ttl-seconds: 21600`, whose body is the session token for the next
  two. The second is `GET /latest/meta-data/iam/security-credentials/` with
  `X-aws-ec2-metadata-token`, whose first line is the role name. The third is the same
  path followed by that role name, whose body is a JSON object.
- For `sts`, sends one `POST /` with an `application/x-www-form-urlencoded` body that holds
  `Action=AssumeRoleWithWebIdentity`, `Version=2011-06-15`, `RoleArn`,
  `RoleSessionName=hirsel` and `WebIdentityToken`, over TLS 1.3 verified as the endpoint's
  name. rustls runs the handshake; the provider opens the records after it in place in its
  own buffer, since rustls' record layer keeps plaintext in memory it never zeroes. A server
  that offers only TLS 1.2 gives `Net(Tls(Protocol(_)))`.
  Each value is sent with every byte outside RFC 3986's unreserved set as `%XX`. The reply
  is XML, and its fields are read inside its `Credentials` element.

The provider receives each reply into run-owned host memory of at most 64 KiB, and zeroes
each buffer that held key bytes before it frees it. It reads these fields from the last
reply:

| meaning | IMDS JSON member | STS XML element |
|---|---|---|
| access key id | `AccessKeyId` | `AccessKeyId` |
| secret access key | `SecretAccessKey` | `SecretAccessKey` |
| session token | `Token` | `SessionToken` |
| expiry | `Expiration` | `Expiration` |

An IMDS reply must also have `"Code": "Success"`. Each value is a string with no escape
sequence, and the expiry has the form `YYYY-MM-DDTHH:MM:SSZ`. These are the forms both
services send.

On success, the access key id is appended to `key_id` and the session token to `token`,
and the result is the secret key together with the expiry, in nanoseconds since the Unix
epoch. So `2026-11-23T12:00:00Z` gives `1795435200000000000`, which a program compares with
`host.wall_ms(c) * 1000000`. The failures are:

- `Room(key_id, token)`: the spare capacity of `key_id` or `token` is less than the byte
  length of that value. It holds both lengths, and the call writes neither string and makes
  no designation. Like every core operation it never grows a buffer.
- `Net(e)`: a connection, TLS or transport failure. A `roots:` file that cannot be read or
  holds no certificate gives `Net(Tls(Roots))`. A deadline gives `Net(TimedOut)` and a cancel gives `Net(Aborted)`. Each
  ends the helper's exchange; a reply that comes later is zeroed and discarded.
- `Token(e)`: the `sts` token file could not be read, or is empty (`Token(NotFound)`).
- `Status(code)`: a reply whose HTTP status is not 200.
- `Malformed`: a reply over 64 KiB, a missing member or element, a value with an escape, an
  expiry in another form, or an IMDS `Code` other than `Success`.

Each call makes a fresh exchange. The provider caches nothing, and when to refresh is the
program's choice (std credentials, below).

**The secret key.** A successful fetch makes a new designation with one holder, with the
grant's policy. It admits `round_up_8(pattern_bytes + 32)` from the current limit, as
`narrow` does (TLS-5), and a refused admission raises
([MEM-21](budgets.md#mem-21-admission)), so the result type does not mention it. The key bytes are run-owned host memory and are never charged, as a
granted secret's are, so the key's length never reaches the program, even through a
refusal. Holders and release follow TLS-5. When the last designation of the key is
released, the runtime zeroes the bytes. A fetched secret is treated as a granted one wherever
this chapter tells the two apart: it never satisfies a TLS half of `provide.net`
(`send_secret` and `scram_sha256` give `Denied`).

**No virtual form.** There is no `provide.cred`, and the model world grants no `Cred`. A
test of a program that fetches credentials runs natively against a server of its own,
named by the grant's `imds:` address or `sts` endpoint and roots.

**Replay.** `cred.fetch` is a provider operation
([CAP-5](capabilities.md#cap-5-provider-operations)) and passes the replay point at issue
and at delivery ([CAP-8](capabilities.md#cap-8-the-replay-point)):

- The issue holds the `Cred`'s grant ordinal, the spare capacities of `key_id` and `token`,
  and `until`.
- The delivery holds the outcome. On success it also holds the expiry and the bytes of the
  access key id and the session token, which are received plaintext. It never holds the
  secret key.

A recording's grant record for a `Cred` holds the parameter name, the source text and the
canonical policy, as a `Secret` grant record does. A replay contacts no endpoint and
reads no token file. It delivers the recorded outcome, and a recorded success makes a
designation with no bytes behind it, as a replayed grant has
([recording](../recording.md#credentials)). No operation reads a secret's bytes, so the
program computes the same values. A MAC tag computed with the key is a delivery of its own
and comes from the recording.

**Std credentials.** Std's `cred` module wraps the fetch in a record:

```hirsel unbuilt
pub type Credentials: send { access_key_id: String, secret: Secret, session_token: String, expires_ns: u64 }
pub fn credentials(c: Cred, until: Until) -> Result[Credentials, CredError] waits
pub fn from_secret(take access_key_id: String, take secret: Secret, take session_token: String) -> Credentials
```

`credentials` reserves 128 bytes for the key id and 4096 for the token, and on `Room` it
reserves the reported lengths and fetches once more. `from_secret` builds the record of a
static key from a `Secret` grant, with `expires_ns` equal to `u64` max, so a client takes
one record type whatever the key's source. The refresh policy belongs with the SigV4
`Signer`. It fetches when the expiry is less than 300 s away, retries after 100 ms
doubling, and validates new credentials with a signed `HEAD` of the bucket before it
replaces the old set, which stays in use until it expires.

## Laws

Each law names the bug it catches and the oracle its expected value comes from. Where a
harness is named, the cases go into it. Certificate fixtures are a test CA and two leaf
certificates, `alpha.test` and `beta.test`, generated with `openssl` and checked in
together with their SHA-256 leaf digests, which `openssl x509 -outform der | sha256sum`
computes. CR4 uses a second test CA and a `localhost` leaf, since its source dials the
endpoint by name.

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
| **TL9** TLS 1.3 resumption | no ticket reuse, or skipping verification on a full handshake after a ticket offer | Connect twice to the same loopback server with the same name, trust and ALPN. The server reports a full then a resumed handshake and the client sees the same peer digest and ALPN. At the same endpoint, replace the server with one that has no prior session and presents a leaf for a different name. The client gives `Tls(Certificate(NameMismatch))`. |
| **TL10** a secret never renders | the bytes reaching a render, log line or diagnostic | The grant `env:` source is 32 random bytes encoded as hex. The program makes an HTTPS request with it as `Authorization: Bearer`, renders the request, triggers and renders an HTTP error, and writes a `Log`. Search stdout, stderr and the run's diagnostics for the value, its base64 and its raw bytes: there are no hits. The loopback server's received header equals the environment value, and the server does not echo it. |
| **TL11** a secret reaches only its names | exfiltration to a host the grant did not name | Three cases: `send_secret` on a stream verified as `beta.test` with a secret granted `tls_send(alpha.test)`, the same with that secret as the second of two secrets after one granted `tls_send(beta.test)`, and `send_secret` on a plain-TCP half. Each gives `Denied`, and the server receives zero bytes after the handshake. |
| **TL17** body segments | a wrong form encoding, a length that misses a secret at a range end, or digits the program could see | An HTTPS POST whose `Content-Length` is a kind 2 mark over a form body `grant_type=refresh_token&refresh_token=<s1>&client_secret=<s2>`, where `s2` ends the body and holds a space, `+`, `&`, `=`, `%` and byte `0xFF`. The server receives the bytes and the length worked out by hand from WHATWG URL §5.2. |
| **TL13** spent half | a resumed send that splits a secret | Make `send_secret` time out after it has committed a prefix (the scripted peer stops reading). The next `send_pair` on that half gives `Reset`. The scripted peer's received bytes are a prefix of the message and contain nothing after it. |
| **TL14** STARTTLS in place | `join` shutting down, rebuffering or losing the connection, or `tls.connect` refusing a joined socket | A loopback fixture server reads 8 bytes and checks they equal `00 00 00 08 04 D2 16 2F` (PostgreSQL's `SSLRequest`, from the protocol documentation's code 80877103). It answers `S`, then runs TLS as `alpha.test`. NET-7's `pg_starttls` succeeds, `Handshake.peer` equals the `openssl` digest of the alpha leaf, and `hello` sent on the TLS half reaches the server as plaintext. When the server answers `N`, the result is `Declined` with byte `0x4E`, and `hello` sent on the returned half reaches the server unencrypted. |
| **TL15** injected plaintext before TLS | bytes after the go-ahead read as the server's (CVE-2021-23222) | The fixture server writes `S` followed by `R\x00\x00\x00\x08\x00\x00\x00\x00` (a fake `AuthenticationOk`) in one write, then waits. The client reads one byte, joins, and `tls.connect` gives `Tls(Protocol(0))`. The server receives no byte after the 8-byte request, and the open descriptor count is back to its value before `connect` (TL7's check). The oracle is TLS-1's peek rule and RFC 8446 §4.1.2, under which the client speaks first. |
| **TL16** join refusals | halves of different sockets merged, or a refusal that loses a half | Using a counting provider: halves of two different loopback sockets, the halves of a TLS stream, and the halves of an accepted socket each give `Unjoined` with zero provider calls. Afterwards, one byte sent on each returned write half reaches that half's own peer, and no other. |
| **SC1** RFC 7677 vector | a wrong derivation, AuthMessage or base64 | Use secret `pencil` granted `tls_send(alpha.test)`, over a loopback TLS stream verified as `alpha.test`. `client_first` is `n,,n=user,r=rOprNGfwEbeRWgbNEkqO`. `server_first` is `r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096`. `before` and `after` are empty. The server receives exactly `c=biws,r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,p=dHzbZapWIk4jUhN+Ute9ytag9zjfMHgsqmmiz7AndVQ=`, and `scram_final_len` gives 106. `scram_verify` with `v=6rriTRBi23WpRR/wtup+mMhUZUn/dB5nLTJRsjl95G4=` gives `true`. With the first signature character changed to `7`, with `e=invalid-proof`, and with the right `v=` on a `Scram` made from a different server nonce, it gives `false`. The oracle is RFC 7677 §3. |
| **SC2** normalization and raw fallback | skipping SASLprep, or failing where PostgreSQL falls back | Use SC1's transcript with three passwords. `pen\u{AD}cil` (U+00AD, which RFC 4013 table B.1 maps to nothing) sends SC1's proof. The bytes `70 65 6E FF 63 69 6C` are not UTF-8 and are used raw, so the proof is `p=udNjQvoDM/dE+e/v63Vs504VHmXCmsAHQp5FC38JZcU=`. `pen\u{E000}cil` holds a private-use code point, which RFC 4013 §5 prohibits, so its raw UTF-8 bytes `70 65 6E EE 80 80 63 69 6C` are used and the proof is `p=S0CJLeozN1oaedLBR16sXAg8qzxE/6wMKY16FfthmYo=`. The last two proofs come from Python's `hashlib.pbkdf2_hmac` and `hmac` over those raw bytes. |
| **SC3** iteration range before work | a check after the derivation, or an off-by-one at either end | With SC1's transcript, `i=4096` succeeds (SC1), and `i=1000000` succeeds, with the server accepting the proof by its own reference computation. `i=4095`, `i=1000001` and `i=0` give `Iterations`. `i=04096` and a missing `i=` give `Transcript`. In each refused case the server receives no byte after the handshake, and the call takes exactly 1 step: `bound steps 1 { let mut h = take w; secret.scram_sha256(mut h, ...) }` returns `Ok` holding the refusal. The half is moved into the scope, since MEM-30 refuses a body that lends `mut` a place declared outside it, and a `let` with a move takes no step. |
| **SC4** per-iteration charge | the derivation counted as one step | With the other arguments in locals bound outside the scope, `bound steps 4097 { let mut h = take w; secret.scram_sha256(mut h, ...) }` returns `Ok`, and the same body under `bound steps 4096` returns `Error(OutOfTime(steps: 4096))`, with no byte reaching the server. By hand, that is 1 step for the call and 4096 for its iterations, and the `let` takes none (MEM-30). |
| **SC5** transcript checks | accepting a server nonce that does not extend the client's, or a mandatory extension | Each of these gives `Transcript` with no byte after the handshake: a server nonce that does not start with the client nonce, a server nonce equal to the client nonce, a `server_first` starting with `m=`, a `client_first` of `n,a=admin,n=,r=abc`, and a salt that is not base64. The oracle is RFC 5802 §5.1 and §7. |
| **SC6** authority | a proof emitted to a host the secret may not reach | Each of these gives `Denied`, and the server receives zero bytes after the handshake: a secret `to beta.test` on an `alpha.test` stream, any secret on a plain-TCP half, and a granted secret on a TLS half of `provide.net`, where the server's `provide.next` sees no `Send`. |
| **SC7** channel binding | the identity digest used where the signature's hash is required, or binding over a missing value | `client_first` `p=tls-server-end-point,,n=,r=abc` over the `alpha.test` leaf, signed with SHA-256, sends `c=` the base64 of `p=tls-server-end-point,,` followed by the leaf's `openssl x509 -outform der \| sha256sum` digest. Over a leaf signed with SHA-384 (an added fixture), it sends the base64 of the header followed by the leaf's `sha384sum` digest, and `scram_final_len` counts its 48 bytes. An Ed25519 leaf, and `p=tls-unique,,`, give `Binding` before issue. The oracle is RFC 5929 §4.1. |
| **SC8** no key material in a recording | key material or the proof reaching a frame | Record SC1's run with the password from an `env:` grant. The tape holds none of these: `pencil`, `SaltedPassword` `c4a49510323ab4f952cac1fa99441939e78ea74d6be81ddf7096e87513dc615d`, `ClientKey` `a60fc923d67e8644a92d16b96eda5ef4656b0c725c484374be25535576996e8b`, `StoredKey` `586e5df283e6dceb5c3e791d8b8528ec191e664045ce971792e2e6b5bb13e2a6`, `ServerKey` `c1f3cbc1c13a9d35a14c0990eed97629ea225863e566a4314ab99f3f00e5d9d5`, the proof's 32 raw bytes, and their base64. Each is searched raw and as hex. The values follow by hand from RFC 7677's inputs through RFC 5802 §3. Replayed with the variable unset, the run gives the same verdict and ends `Sealed`. |
| **SC9** virtual SCRAM | the virtual path sending different bytes or exposing a granted secret | Against a `provide.net` server whose handshake reply is `Secured`, seal the owned bytes of `"pencil"` with `"tls_send(alpha.test)"`, unwrap the result, and use SC1's transcript. The server's `Send` calls carry exactly SC1's client-final bytes. When the server commits 10 bytes and then the rest, the second `Send` offers bytes 10 to 106. Under `hirsel test --replay`, the replay ends `Sealed` and its server sees the same calls. |
| **CR1** no key byte in program memory | the key copied into a program buffer, or a host buffer that held it left unzeroed | A loopback IMDS server hands out the 40-byte key `wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY` granted `Cred:aws=imds:127.0.0.1:<port> mac`. The program fetches, prints `secret.hmac_sha256` of `x` under the key, and parks on a receive from the test. The tag equals Python's `hmac.new(key, b"x", "sha256")`. While it is parked, the test reads every readable mapping of the run's process through `/proc/<pid>/mem` and finds the key exactly once: the sealed store. The control serves the same key as `Token` too, which puts it in a program `String`, and the same scan finds it at least twice. Both runs repeat against CR4's STS server over TLS, which catches plaintext left in a TLS library's buffers. |
| **CR2** replay without the endpoint | replay contacting the source, or the key reaching a frame | Record CR1's run. The tape holds the key id and token bytes, and holds none of the key's raw bytes, hex or base64. Replayed with the server gone, the run prints the same tag and ends `Sealed`. |
| **CR3** the IMDS exchange | a missing IMDSv2 token, a wrong expiry, or a failure that still makes a designation | The server answers only a `PUT` with the TTL header and only `GET`s that carry its token. `Expiration` `2026-11-23T12:00:00Z` gives `1795435200000000000` (`date -ud`). A 404 on the role gives `Status(404)`. A reply without `SecretAccessKey`, with an escaped value, or with `Code` `Expired` gives `Malformed`. A `token` with 3 bytes of room against a 7-byte token gives `Room(20, 7)` with both strings unchanged. A server that never answers, under a 50 ms deadline, gives `Net(TimedOut)`. A grant with source `static` is refused at its spelling. |
| **CR4** STS web identity | the token sent from a stale read, or the wrong reply element taken as the key | A loopback TLS server as `localhost` with its CA's roots, granted `sts:<arn> at:localhost:<port> roots:<ca> file:<token>`. The server receives `WebIdentityToken` equal to the file's bytes, which it rewrites before each reply, and sees each of three versions once. The reply puts `SessionToken` before `SecretAccessKey`. Its `AssumeRoleWithWebIdentityResponse` gives the fixture's key id and expiry, and the key's MAC matches CR1's oracle. A 403 `AccessDenied` gives `Status(403)`. |

TL1 to TL11, TL14 to TL16 and SC1 to SC8 run against the OS provider over
loopback. TL5, TL7, TL13, TL15, SC1 and SC5 also have scripted-provider cases, so that the
evaluator is covered. SC9 runs on both engines. CR1 to CR4 run natively against loopback servers. The recording laws of
TLS sessions and secrets, RL3 to RL5, are with the tools ([recording](../recording.md#laws)).

## Build slices

Each slice lands finished. Sizes are S (under a day of one lane), M (one to three days)
and L (more than three).

| slice | size | depends on | contents | laws |
|---|---|---|---|---|
| **T1** OS TLS provider | L | — | core declarations (`tls.connect`, `Trust`, `Handshake`, `TlsFailure`, the `NetError.Tls` case); `TLS_CONNECT` and TLS halves in `os.rs` and `os_net.rs` over rustls with `ring` and `rustls-native-certs` (Buck third-party fixups); the `Tls` rule in grant parsing, `net.narrow` and the C rule struct; the C entry; the LLVM and Cranelift lowering of `tls.connect` | TL1–TL8 |
| **T2** recordings | M | T1 | in the recording build, `TLS_CONNECT` issue and delivery codecs, the TLS halves under the recorded `Net` grant, the format-5 header `Tls` rule record, credential-bearing file mode and export gate ([recording](../recording.md)) | RL3 |
| **T3** evaluator and scripted provider | M | T1 | the evaluator's call form for tag 22; served handshake results and plaintext steps | scripted TL5, TL7 |
| **T4** Secret | M | T1, T2 | the `Secret` core type and grant kind 11, launcher sources `env:` and `file:`, `seal`, `narrow`, `render`, `SEND_SECRET` in the OS and scripted providers, its designation and grant record in a recording, the spent-half rule | TL10–TL13 |
| **T5** HTTPS client | S | T1, T4, http-client-1439 | std `tls` api page; `https` connect = `host.connect`, then `tls.connect` offering `http/1.1`, then the plain client's reader and writer over the halves; `Secret` header values written through `send_secret`; `[secret]` in request and error renders | TL10 (the client's case), and one HTTPS round trip in the existing http client harness |
| **T6** STARTTLS | S | T1, T3 | `net.join` and `Unjoined` (NET-7) on both engines and in the evaluator's virtual sockets; TLS-1's unread-bytes peek in the OS provider, and a pre-handshake bytes step in peer scripts | TL14–TL16 |
| **T7** SCRAM | M | T4, T6 | TLS-1's binding value (`binding_out`, the recording's `TLS_CONNECT` delivery); `Scram`, `ScramError`, `scram_sha256`, `scram_final_len`, `scram_verify`; SASLprep with raw fallback; the per-iteration charge; `SCRAM` and `SCRAM_VERIFY` in the OS, scripted and virtual paths and in a recording | SC1–SC9 |
| **T8** credential grants | M | T4, T7 | the `Cred` type and grant kind, the `imds` and `sts` sources, `cred.fetch` in the OS provider on a host helper, its frames and grant record in a recording, std `cred` | CR1–CR4 |

T2 and T3 can run in parallel after T1. T4 waits for T2 because its designation in a
recording is part of its contract. T5, T6 and T7 run in any order once their dependencies land.
