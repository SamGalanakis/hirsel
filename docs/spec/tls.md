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
| `Secret` | resource, mechanical | `cross` | 8 | releases one holder (TLS-5) |

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
  A bare `*` covers every name ([NET-18](net.md#net-18-public-only-connects)). It is
  not a `tls_send` pattern (TLS-5).
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
name, `*` with any pattern gives that pattern, and two wildcards give the narrower one
when one covers the other. Port ranges
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

*Version note (2026-11-24, net-public-only-amend-2441): review 2431's F4 applied. A bare
`*` is not a `tls_send` pattern.*

```hirsel core
pub fn seal(take b: Bytes, uses: String) -> Result[Secret, (SecretError, Bytes)]
pub fn narrow(s: Secret, uses: String) -> Result[Secret, SecretError]
pub fn share(s: Secret) -> Secret
pub fn render(s: Secret) -> View[String] in s
pub fn send_secret(mut w: WriteHalf, wire: Bytes, secrets: List[Secret], marks: List[u64], until: Until) -> Result[(), NetError] waits
pub type SecretError = Invalid | Capacity(Capacity)
pub fn wrap(kek: Secret, s: Secret, mut out: Bytes) -> Result[u64, KeyError]
pub fn unwrap(kek: Secret, blob: Bytes, uses: String) -> Result[Secret, KeyError]
pub fn sign(key: Secret, alg: SignAlg, r: Random, msg: Bytes, mut out: Bytes) -> Result[u64, KeyError]
pub type KeyError = Denied | Invalid | Tampered | Capacity(Capacity)
pub type SignAlg = Ed25519 | P256 | Rs256
```

**Uses.** A secret is opaque host material together with the typed set of uses it
permits, its **policy**. A policy's text is its uses separated by single spaces, each at
most once:

| Use | Permits |
| --- | --- |
| `tls_send(<patterns>)` | `tls.send_secret` and `secret.scram_sha256` on a stream whose verified name one of the comma-separated TLS-3 patterns covers |
| `mac` | the MAC signing and verification sinks below |
| `verify` | the MAC verification sinks below |
| `kdf` | `secret.derive_hmac_sha256`, `secret.derive_hkdf_sha256` and `secret.derive_argon2id` with the secret as parent (the derivation sinks, below) |
| `digest` | `secret.derive_digest` (unbuilt, [turn.md TURN-5](turn.md#turn-5-long-term-credentials-and-secrets)) with the secret as parent. It also reads what `kdf` seals from the same bytes under a key over 64 bytes: the digest is that child's HMAC key (the long-key rules, below) |
| `sign(<alg>)` | `secret.sign` with `<alg>`, one of `ed25519`, `p256` and `rs256` (key operations, below) |
| `aead(<alg>)` | `secret.aead_seal` and `secret.aead_open` with `<alg>`, one of `chacha20-poly1305`, `aes128-gcm` and `aes256-gcm` (the AEAD sink, below) |
| `open(<alg>)` | `secret.aead_open` only, with `<alg>` |
| `wrap` | `secret.wrap` and `secret.unwrap` with the secret as the key-encryption key (KEK) |
| `srtp_import(<dir>)` | `schedule.import_srtp`, with the secret as SRTP master key and salt, making a schedule of direction `<dir>`, `send` or `receive` ([TLS-8](#tls-8-packet-ciphers-and-key-agreement)) |
| `traffic_import(<dir>)` | `schedule.import_traffic`, with the secret as a TLS 1.3 or QUIC traffic secret, making a schedule of direction `<dir>` (TLS-8) |
| `mtls_client_id` | use as a TLS client identity |

A bare `*` is not a `tls_send` pattern: `hirsel run` refuses a grant that has it, and a
`seal` or `unwrap` policy that has it is `Invalid`.

`mac` includes `verify`, so the two are not written together, and `aead(<alg>)` includes
`open(<alg>)` for the same `<alg>`, so neither are those. A holder's authority over a secret's bytes is every use it holds on
them, in one policy or over several designations. `mac` with `kdf` reads the child of
`derive_hmac_sha256(s, "", data, …)` as the tag `hmac_sha256(s, data)`, and `digest` with
`kdf` reads long-key HMAC children (the long-key rules, below). A derivation's caller picks
`uses` and may derive the same child again, so over a child it holds the whole child set.
What a grant withholds, it withholds from the granted bytes only. So a `kdf` grant confers
MAC authority over that secret's children: a holder of a SigV4 secret key as `kdf` derives
`kDate` as `mac`, reads `kRegion` as its tag, and can export a signing key for any day it
chooses. An import use stands alone: a
policy holding `srtp_import(<dir>)` or `traffic_import(<dir>)` holds no other use (TLS-8). A name is a destination
only inside `tls_send(...)` and a use word only outside it, so `tls_send(mac)` sends to
the host `mac` and permits no MAC. A policy with no use, a word given twice, an unknown
word or a malformed pattern list is `Invalid`. The canonical text, which recordings and
diagnostics print, writes `tls_send` first, then `mac` or `verify`, `kdf`, `digest`, the `sign` uses in the
order above, the `aead` and `open` uses with their algorithms in the order above, `wrap`, `srtp_import(<dir>)`,
`traffic_import(<dir>)` and `mtls_client_id`, with each pattern as TLS-3 prints it. No
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
`tls_send` remains when that is not empty. `mac` against `verify` gives `verify`,
`aead(<alg>)` against `open(<alg>)` gives `open(<alg>)`, and every other use remains when
both policies hold it. If no use remains, or `uses` is
`Invalid`, the result is `Invalid`. It admits `round_up_8(pattern_bytes + 32)` for the new designation, and a
refusal returns `Capacity(c)`.

**Holders and release.** Each `Secret` value is one **holder** of a **designation**: the
bytes it designates together with its policy. `seal`, the three derivations, `narrow` and `provide.secret`
([core](core.md#provide)) each make a new designation with one holder.
`secret.share(s)` returns one more holder of the designation of `s`, as `share` adds a
holder of an `Rc` ([MEM-16](memory.md#mem-16-counting)): it admits nothing, never fails
and never waits. Dropping a `Secret` removes its holder. When the last holder of a
designation is dropped, the designation is **released**: the charge that `narrow` or
`provide.secret` admitted for it is refunded. The bytes of a sealed secret stay while
any designation of them is held, since `narrow` and `provide.secret` designate the same
bytes. When the last of them is released, the runtime zeroes the bytes and refunds the
charge that `seal` or the derivation admitted. So a program that seals a fresh token every hour and drops
the old one holds the storage of one token, not of every token it has sealed.

A release is not a provider operation: it passes no replay point, takes no step and cannot
fail. Its refund is a `refund` take in the stream of the unit that pays the charge, at the
drop when that unit drops the last holder and at a drain of its mailbox otherwise, because
whether a drop is the last depends on drops in every unit
([unit recordings](../recording.md#unit-recordings)). A designation is recorded by its origin (the
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
verdict or JWS key-size refusal, the SCRAM sink's verdict, `wrap`'s blob (ciphertext under the KEK), `sign`'s
signature and the AEAD sink's sealed and opened bytes are the only values computed from
them that reach the program. A derivation
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

**The MAC sink.** `secret.hmac_sha1`, `secret.hmac_sha256`, `secret.hmac_sha384`
and `secret.hmac_sha512` use a secret as an HMAC key without sending it. The runtime computes from run-owned
memory and returns a 20-, 32-, 48- or 64-byte inline tag. The corresponding
`secret.verify_hmac_sha1`, `secret.verify_hmac_sha256`, `secret.verify_hmac_sha384`
and `secret.verify_hmac_sha512` verify full-length tags in constant time. They return `Ok(false)` for a tag of any
other length. Signing requires `mac` and verification requires `mac` or `verify`.
Without the use, the result is `Denied` before issue. A tag over chosen data is a signing
oracle wherever it is sent, which is why the use must be granted.

`secret.jws_hmac_sign` and `secret.jws_hmac_verify` have the same use requirements.
They enforce RFC 7518 §3.2 in the provider. A key shorter than its algorithm's tag
gives `ShortKey` without computing a MAC. The JWS signing sink returns a 64-byte
inline result padded with zeros after the tag.

An authorized call takes one step for the call and one per byte of `data`, charged
before issue. No operation waits or admits bytes. The tag, verdict or JWS key-size
refusal is a delivery at the replay point. The recording contains the designation,
policy and public arguments, plus that delivery.
[HMAC API](../../core/std/api/hmac.md#secret-keys).
[CAP-8](capabilities.md#cap-8-the-replay-point).

**Key operations.** `secret.wrap`, `secret.unwrap` and `secret.sign` use a secret as a key
without revealing it. A connector stores an OAuth refresh token as `wrap`'s blob in a
`store` row, so the token commits atomically with its account row, and opens it with
`unwrap` when it needs it. The runtime computes from run-owned memory with ring's
primitives; Hirsel has no cryptographic code of its own.

A secret is **opaque** when the program never held its bytes: one from an entry grant, one
made by a derivation sink from an opaque parent, or one unwrapped under an opaque KEK.
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
| `P256` | ECDSA over P-256 with SHA-256 (JWS `ES256`). Host `Random` supplies fresh randomness. Virtual `Random` selects RFC 6979 | PKCS#8 with an RFC 5915 EC key | 64 bytes, `r ‖ s` |
| `Rs256` | RSASSA-PKCS1-v1_5 with SHA-256 (RFC 8017 §8.2, JWS `RS256`) | PKCS#8 RSA key of 2048 to 4096 bits | the modulus length |

The results, decided in this order:

- `Denied` before issue, with no effect: the key lacks `wrap` (for `wrap` and `unwrap`) or
  the `sign(<alg>)` for `alg`, or `wrap` would put an opaque secret under a KEK that is
  not, or `wrap`'s `s` holds an import use (TLS-8: an unwrap would make fresh bytes that a
  second import could use).
- `Invalid` before issue: `uses` is not a policy, or `unwrap`'s `uses` holds an import use
  (TLS-8). A program that holds a KEK's bytes can write a blob binding any policy, so this
  is checked on `uses`, not left to `wrap`'s refusal.
- `Capacity(c)` before issue: `unwrap`'s admission is refused, with the requested charge
  and the space available.
- `Invalid`: a KEK shorter than 32 bytes, or a signing key that is not PKCS#8 for `alg`.
- `Tampered`: the blob is not one this KEK wrapped, unaltered. A bad version, header or
  tag, a different KEK and any changed byte give the same answer, and nothing more.
- `Denied`: `unwrap`'s `uses` is not within the bound policy.
- `Capacity(c)`: the blob or signature does not fit the spare of `out`, with its length
  as `requested` and the spare as `available`; nothing is appended.

The last four are deliveries. Wrapping and unwrapping pass the replay point
([CAP-8](capabilities.md#cap-8-the-replay-point)). Signing follows TLS-8's rule
for the key and `Random`. A recording keeps provider outputs and designation origins.
A replay delivers the recorded blob, signature or designation from the tape
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
parent and child policies follow the derivation rule below. The runtime admits 64 bytes
for the child, `round_up_8(32 + 32)` by that rule.

The call never waits. A successful result passes the replay point. A recording
holds the parent designation, the child policy, `key_prefix` and `data`, and a
success delivery. Replay makes the child designation without key material.
The runtime zeroes derived bytes when their last designation is released.

**The digest derivation sink** (unbuilt; [turn.md TURN-5](turn.md#turn-5-long-term-credentials-and-secrets),
slice U1). `derive_digest(s, alg, prefix, inner, suffix, uses)` keeps
`H(prefix ‖ enc(bytes of s) ‖ suffix)` as a new sealed secret, `H` being MD5 or SHA-256 and
`enc` the identity (`Raw`) or padded base64 (`Base64`). It is under the derivation rule
below, with `digest` on the parent, and its charges, frames and replay are
`derive_hmac_sha256`'s.

**The long-key rules.** HMAC-SHA256 ([RFC 2104](https://www.rfc-editor.org/rfc/rfc2104)
§2) replaces a key longer than its 64-byte block with the key's SHA-256, so `SHA-256(P ‖ s)`
is the key `derive_hmac_sha256(s, P, …)` uses when `P ‖ s` is over 64 bytes (RFC 4231 test
case 6, 131 bytes of 0xaa, gives the published tag `60e43159…0ee37f54` either way). Two rules
keep that digest from a holder not granted it: `derive_digest` with `Raw` and `Sha256` and
both `prefix` and `suffix` empty is `Invalid` before issue; and `derive_digest` needs
`digest`, so a secret granted `kdf` alone, of any length, never becomes a MAC key and its
children stay sealed. A digest's child never holds `digest`: a digest of a digest under the
two HMAC pads is HMAC-SHA256 with the secret as its message, which is HKDF's PRK.
`derive_hmac_sha256` takes a key of any length, so SigV4 secret keys of any length derive.

**The HKDF and Argon2id derivation sinks.**

```hirsel core
pub fn derive_hkdf_sha256(s: Secret, salt: Bytes, info: Bytes, len: u64, uses: String) -> Result[Secret, SecretError]
pub fn derive_argon2id(s: Secret, salt: Bytes, cost: Argon2Cost, len: u64, uses: String) -> Result[Secret, SecretError]
```

`derive_hkdf_sha256` is HKDF with SHA-256 ([RFC 5869](https://www.rfc-editor.org/rfc/rfc5869)):
Extract with `salt` and the bytes of `s` as input keying material, then Expand with `info`
to `len` bytes. An empty `salt` is the 32 zero bytes of §2.2, which gives the same PRK.
`derive_argon2id` is `argon2.id` ([core](core.md#argon2id)) with the bytes of `s` as the
password, `salt`, an empty secret value and associated data, `cost` and `len`. Each keeps
the `len` result bytes as a new sealed secret. HKDF turns a high-entropy key into keys for
separate purposes, such as one AEAD key per tenant or per file; Argon2id turns a granted
passphrase into a key. The runtime computes from run-owned memory. HKDF is ring 0.17.14's
`hkdf::Salt::new(HKDF_SHA256, salt).extract(ikm)`, then `Prk::expand(&[info], len)` and
`Okm::fill`, with `len` given through a wrapper type implementing `hkdf::KeyType`. ring's
`expand` fails exactly when `len` exceeds `255 × 32`, the bound the rule below checks
first, and an empty `salt` is an HMAC key of no bytes, which HMAC pads to the same block as
32 zero bytes. Argon2id is `argon2.id`'s wrapper.

**The derivation rule.** It governs `derive_hmac_sha256`, `derive_digest`,
`derive_hkdf_sha256` and `derive_argon2id` alike. The results, in this order:

- `Invalid` before issue, with no effect: the parent does not permit `kdf` (`digest` for
  `derive_digest`); `uses` is not a policy, or holds a use outside `mac`, `verify`, `kdf`,
  `digest`, `aead(<alg>)` and `open(<alg>)`;
  `len` is outside `1` to `8160` (255 SHA-256 blocks, RFC 5869 §2.3) for HKDF; or, for
  Argon2id, any of `argon2.id`'s `Params` checks fails, `key` and `data` being empty; or,
  for `derive_digest`, `Raw` with `Sha256` and both `prefix` and `suffix` empty, or `uses`
  holding `digest`.
- For Argon2id only, its steps: one for each block compression, taken at the call before
  issue exactly as `argon2.id` takes them ([MEM-30](budgets.md#mem-30-step-bounds)). If
  the current bound has fewer left, the call raises `OutOfTime` there, having admitted and
  derived nothing.
- `Capacity(c)` before issue: the caller's current limit refuses the child's storage,
  `round_up_8(len + 32)` bytes, or for Argon2id the `1024 × m′` bytes of working memory,
  admitted together. Nothing is derived.
- For Argon2id only, `Invalid` as a delivery when the parent's bytes are longer than
  `2^32 − 1`, the longest password RFC 9106 allows.
- The new designation, with one holder and policy `uses`.

The working memory holds blocks computed from the password; the runtime zeroes it before
it refunds it, at the delivery. A child is opaque exactly when its parent is (key
operations, above). The child set leaves out `tls_send`, `sign`, `wrap`, the import uses and
`mtls_client_id`: a derived key is a fresh symmetric key, which no peer knows as a
credential and which is not a PKCS#8 private key, and a KEK comes from a grant or a seal so
its custody is decided where it is made; an import key comes only from outside the run
(TLS-8). A derived key's length is the caller's `len`, so
`aead(chacha20-poly1305)` and `aead(aes256-gcm)` children need `len = 32` and
`aead(aes128-gcm)` children `len = 16`; any other length is refused at use (the AEAD sink).

Charges are the rule's: one step per call, plus Argon2id's block steps, and the admissions
above. HKDF's work is bounded by `len`, which the rule bounds, and by the lengths of `salt`
and `info`, which a limit bounds, so it takes one step whatever they are, as
`sha256.blocks` does. Neither call waits. A successful result passes the replay point
([CAP-8](capabilities.md#cap-8-the-replay-point)). A recording holds the parent
designation, the child policy, `salt`, `info` or `cost`, `len` and the success delivery,
and never the parent's or the child's bytes. Replay makes the child designation without
key material, and a later use of it replays from the recording, as for
`derive_hmac_sha256`.

HKDF, HMAC and Argon2id over the parent's bytes run with no branch or memory index that
depends on them, except Argon2id's data-dependent addressing after its first half pass,
which RFC 9106 §3.4 designs in.

**The AEAD sink.**

```hirsel core
pub type AeadAlg = ChaCha20Poly1305 | Aes128Gcm | Aes256Gcm
pub fn aead_seal(key: Secret, alg: AeadAlg, nonce: Bytes, aad: Bytes, plain: Bytes, mut out: Bytes) -> Result[u64, KeyError]
pub fn aead_open(key: Secret, alg: AeadAlg, nonce: Bytes, aad: Bytes, sealed: Bytes, mut out: Bytes) -> Result[u64, KeyError]
```

`aead_seal` encrypts `plain` and authenticates it with `aad` under the secret as key, and
appends to the spare of `out` the ciphertext followed by the 16-byte tag, answering
`len(plain) + 16`. `aead_open` takes `sealed` in that layout, checks the tag over `aad`
and the ciphertext, and only when it holds appends the plaintext, answering
`len(sealed) − 16`. That layout is what RFC 8439, JWE (`A128GCM`, `A256GCM`, split at the
tag) and most file formats store.

| `alg` | construction | key | nonce | tag | longest `plain` |
| --- | --- | --- | --- | --- | --- |
| `ChaCha20Poly1305` | [RFC 8439 §2.8](https://www.rfc-editor.org/rfc/rfc8439#section-2.8) | 32 bytes | 12 bytes | 16 bytes | `2^38 − 64` bytes |
| `Aes128Gcm` | GCM ([NIST SP 800-38D](https://csrc.nist.gov/pubs/sp/800/38/d/final)) with AES-128 | 16 bytes | 12 bytes | 16 bytes | `2^36 − 32` bytes |
| `Aes256Gcm` | GCM with AES-256 | 32 bytes | 12 bytes | 16 bytes | `2^36 − 32` bytes |

The runtime is ring 0.17.14's `aead`: `UnboundKey::new` with `CHACHA20_POLY1305`,
`AES_128_GCM` or `AES_256_GCM`, which refuses a key of any other length (the delivered
`Invalid` below), wrapped in a `LessSafeKey`, with `Nonce::try_assume_unique_for_key`,
which takes exactly 12 bytes. The longest `plain` column is ring's per-nonce limit for each
algorithm (its `MAX_IN_OUT_LEN`), which the sink checks before issue. A seal copies `plain`
into the spare of `out`, encrypts it there with `seal_in_place_separate_tag` and appends
the tag after it. An open copies the ciphertext into the spare of `out`, decrypts it there
with `open_in_place_separate_tag` against the tag taken from `sealed`'s last 16 bytes
(`Tag::try_from`), and extends `out`'s length only when that succeeds, so neither copies
through any other buffer and nothing is admitted. ring offers no AES-192, no nonce of
another length and no truncated tag, and none is offered here: GCM
with a nonce other than 96 bits derives its counter through GHASH, which SP 800-38D
§8.2 and the attacks on it make the weaker choice, and a short tag weakens every message.

**Nonces are the caller's.** The sink keeps no state, so it cannot see a nonce used twice
under one key, and reuse is catastrophic: it reveals the XOR of the two plaintexts, and
under GCM the authentication key, after which an attacker forges at will. A caller either
counts (a 96-bit counter stored with the key) or draws the 12 bytes with `host.random` and
seals at most `2^32` messages under one key (SP 800-38D §8.3). Beyond that it derives a key
per message or per batch with `derive_hkdf_sha256` and a fresh salt, as `wrap` does. The
nonce is an argument because the formats a program must read and write fix it: JWE,
age-style files and record protocols each say how theirs is made.

The results, decided in this order:

- `Denied` before issue, with no effect: `aead_seal`'s key lacks `aead(<alg>)`, or
  `aead_open`'s key lacks both `aead(<alg>)` and `open(<alg>)`, for `<alg>` the spelling of
  `alg`. The policy fixes the algorithm, as `sign(<alg>)` does, so one key is never used
  with two constructions.
- `Invalid` before issue: `nonce` is not 12 bytes; `plain` is longer than `alg`'s longest,
  or `sealed` is longer than that plus 16.
- `Capacity(c)` before issue: the result does not fit the spare of `out`, with its length
  as `requested` (`len(plain) + 16` for a seal; `len(sealed) − 16` for an open, or 0 when
  `sealed` is shorter than 16) and the spare
  as `available`; nothing is appended.
- `Invalid`: the key's length is not `alg`'s. Like `wrap`'s KEK check, this tells the
  program only whether the length matches, which the grant's author chose.
- `Tampered`, `aead_open` only: `sealed` is shorter than 16 bytes, or the tag does not
  verify. A wrong key, nonce, `aad`, ciphertext byte or tag byte gives the same answer, and
  nothing more: not which byte, and no plaintext. Nothing is appended. ring decrypts as
  it authenticates, then compares the tags in constant time and zeroes every decrypted
  byte when they differ, before the call returns, so no unverified plaintext becomes part
  of `out` or reaches the program.

The last two are deliveries. Each operation is a provider operation
([CAP-5](capabilities.md#cap-5-provider-operations)), as the MAC sink is: its issue and its
delivery pass the replay point. The issue holds the key's designation, `alg`, `nonce`,
`aad` and `plain` or `sealed`; the delivery holds the appended bytes or the refusal. A
recording never holds the key. A replay, whose key has no bytes behind it, delivers the
recorded result. A recording of `aead_open` holds the plaintext it opened, so it is as
sensitive as that plaintext, as a recording of received TLS plaintext is. To keep opened
bytes sealed, store them with `wrap` and open them with `unwrap`, which give a `Secret`.

**Charge.** Nothing is admitted: the result lands in capacity the caller holds, and the key
schedule and working state are run-owned and never charged, as
`send_secret`'s encodings are. Each call takes one step whatever the lengths, the rule of
`sha256.blocks` and the signature kernels ([core](core.md#signature-verification)): the
work is bounded by `len(plain)` or `len(sealed)`, which a limit bounds. Neither operation
waits.

**Constant time.** Nothing the sink computes from the key or the plaintext varies in time
or memory address with their values. ChaCha20 and Poly1305 are fixed sequences of
additions, rotations, XORs and multiplications. AES runs on the CPU's AES instructions when
it has them (AES-NI, ARMv8 AES), otherwise on ring's vector-permute AES (`vpaes`, SSSE3 or
NEON) or its bitsliced software (`aes_nohw`), none of which looks up a table by secret
index. GHASH runs on carry-less multiply instructions (PCLMULQDQ, VPCLMULQDQ, ARMv8 PMULL)
or on ring's constant-time software multiply (its `gcm` fallback, after BearSSL's). The
tag comparison is constant time (`verify_slices_are_equal`). The time may depend
on the lengths of `nonce`, `aad` and `plain`, which the program already knows. An engine
may choose among these paths by CPU; every path gives the same bytes, so by
[SEM-8](semantics.md#sem-8-determinism) a run behaves the same on every CPU.

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
- `Token(e)`: the `sts` token file could not be read (`NotFound` for a missing file,
  `Denied` for a permissions failure), or is empty (`Token(NotFound)`).
- `Status(code)`: a reply whose HTTP status is not 200.
- `Malformed`: a reply or web identity token over 64 KiB, a missing member or element,
  a value with an escape, an expiry in another form, or an IMDS `Code` other than `Success`.

The provider only reads regular files for `file:` and `roots:`. A named pipe gives
`Token(Other(EINVAL))` or `Net(Tls(Roots))` respectively. File refusals include no file
contents in the error or logs.

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

### TLS-8: Packet ciphers and key agreement

*Version note: amended by media-crypto-amend-2227 after the security review media-crypto-attack-2222,
which found the first text (0eb6a5c7) unsafe. Key schedules replace the generic derivation sinks, and
senders are owned.*

Datagram security protocols seal every packet under a long-lived key. QUIC
([RFC 9001](https://www.rfc-editor.org/rfc/rfc9001)), DTLS 1.2
([RFC 6347](https://www.rfc-editor.org/rfc/rfc6347)) and 1.3
([RFC 9147](https://www.rfc-editor.org/rfc/rfc9147)), and SRTP and SRTCP
([RFC 3711](https://www.rfc-editor.org/rfc/rfc3711), [RFC 7714](https://www.rfc-editor.org/rfc/rfc7714))
do this thousands of times a second. They also agree a fresh key with each peer and mask
packet numbers. TLS-5's AEAD sink suits file formats: it caches each expanded key with
the owned secret bytes, and the caller picks the nonce. TLS-8 adds what packet protocols need:

- an ephemeral **key share** for X25519 or P-256 agreement;
- a **key schedule** per protocol, which takes the share and the peer's public key and
  derives every protocol key under the protocol's own labels;
- **sealers** and **openers**, issued by a schedule, which hold an expanded key for their
  whole life and derive each nonce from a packet counter;
- **header masks** for QUIC header protection and DTLS 1.3 record-number encryption;
- the Finished values of TLS 1.3 and TLS 1.2;
- a P-256 **signing key** generated in the run, for a self-signed DTLS certificate.

The protocols themselves (handshake messages, transcripts, retransmission, replay windows,
key-update timing) are library code over these operations. A program never holds a protocol
key, and no protocol key is ever a `Secret`: an agreed secret, a master secret, a traffic
secret or a key block lives only inside a schedule, which uses each value for the one
purpose its protocol gives it.

**Why schedules and not derivation sinks.** A generic derivation lets a caller pick the
label, the output slice and the child's uses. Then a short slice of a key block, given a
use that answers observable queries, can be found by trying every value, and slice by slice
the whole block falls. The same parent derived twice with different uses yields the same
bytes under two purposes, so a payload key could serve as a header-mask key. A schedule
fixes the labels, the output boundaries and the purpose of every output, and it computes
each output once.

```hirsel unbuilt
pub opaque resource type KeyShare
pub opaque resource type Schedule
pub opaque resource type Sealer
pub opaque resource type Opener
pub opaque resource type HeaderMask
pub type Group = X25519 | P256
pub type Side = Client | Server
pub type Suite13 = Aes128GcmSha256 | Aes256GcmSha384 | ChaCha20Poly1305Sha256
pub type Suite12 = Aes128GcmSha256 | Aes256GcmSha384 | ChaCha20Poly1305Sha256
pub type Profile13 = Quic(Suite13) | Dtls13(Suite13) | Tls13(Suite13) | QuicMultipath(Suite13)
pub type SrtpProfile = Aes128CmHmacSha1_80 | AeadAes128Gcm | AeadAes256Gcm
pub type Space = Initial | Handshake | Application | Rtp | Rtcp
pub type ScheduleError = Denied | Invalid | Issued | Spent | Capacity(Capacity)
pub type PacketError = Reused | Range | TooLong | Tampered | Capacity(Capacity)
pub type PacketCipher: key = Aes128CmHmacSha1_80 | Aes128Gcm | Aes256Gcm | ChaCha20Poly1305
pub type SpaceInfo { cipher: PacketCipher, send: bool, receive: bool }

// module secret
pub fn key_share(r: Random, g: Group, mut public: Bytes) -> Result[KeyShare, KeyError]
pub fn p256_key(r: Random, uses: String, mut public: Bytes) -> Result[Secret, KeyError]
pub fn sign(key: Secret, alg: SignAlg, r: Random, msg: Bytes, mut out: Bytes) -> Result[u64, KeyError]

// module schedule
pub fn tls13(p: Profile13, side: Side, take k: KeyShare, peer: Bytes, hello_hash: Bytes) -> Result[Schedule, (ScheduleError, Option[KeyShare])]
pub fn quic_initial(side: Side, dcid: Bytes) -> Result[Schedule, ScheduleError]
pub fn dtls12(suite: Suite12, side: Side, epoch: u16, take k: KeyShare, peer: Bytes, client_random: Bytes, server_random: Bytes, session_hash: Option[Bytes]) -> Result[Schedule, (ScheduleError, Option[KeyShare])]
pub fn import_traffic(p: Profile13, take s: Secret) -> Result[Schedule, (ScheduleError, Option[Secret])]
pub fn import_srtp(p: SrtpProfile, take s: Secret) -> Result[Schedule, (ScheduleError, Option[Secret])]
pub fn application(mut s: Schedule, finished_hash: Bytes) -> Result[(), ScheduleError]
pub fn key_update(mut s: Schedule, local: bool) -> Result[(), ScheduleError]
pub fn retire(mut s: Schedule, space: Space) -> Result[(), ScheduleError]
pub fn srtp(mut s: Schedule, p: SrtpProfile) -> Result[Schedule, ScheduleError]
pub fn finished(s: Schedule, of: Side, transcript_hash: Bytes, mut out: Bytes) -> Result[u64, ScheduleError]
pub fn verify_finished(s: Schedule, of: Side, transcript_hash: Bytes, received: Bytes) -> Result[bool, ScheduleError]
pub fn sealer(mut s: Schedule, space: Space, domain: u64, floor: u64) -> Result[Sealer, ScheduleError]
pub fn sealer_pair(mut s: Schedule, first: Space, second: Space, domain: u64, first_floor: u64, second_floor: u64) -> Result[(Sealer, Sealer), ScheduleError]
pub fn inspect(s: Schedule, space: Space) -> Result[SpaceInfo, ScheduleError]
pub fn opener(s: Schedule, space: Space) -> Result[Opener, ScheduleError]
pub fn header_mask(s: Schedule, space: Space, local: bool) -> Result[HeaderMask, ScheduleError]

// module packet
pub fn seal(mut c: Sealer, seq: u64, mut buf: Bytes, aad_at: u64, at: u64, extra: Bytes) -> Result[u64, PacketError]
pub fn seal_into(mut c: Sealer, seq: u64, src: Bytes, from: u64, mut dst: Bytes, aad_at: u64, extra: Bytes) -> Result[u64, PacketError]
pub fn open(c: Opener, domain: u64, seq: u64, mut buf: Bytes, aad_at: u64, at: u64, extra: Bytes) -> Result[u64, PacketError]
pub fn open_into(c: Opener, domain: u64, seq: u64, src: Bytes, aad_at: u64, at: u64, extra: Bytes, mut dst: Bytes) -> Result[u64, PacketError]
pub fn mask(m: HeaderMask, sample: Bytes, at: u64) -> Inline[u8, 5]
```

The `sign` operation takes the `Random` used by ECDSA (randomness, below).

`inspect` reads a space's packet cipher and permitted directions without issuing a key.
It answers `Invalid` for an absent space and `Spent` for a retired space.
`sealer_pair` issues two distinct spaces for the same domain, with a floor per space.
It validates both spaces and admits both cipher and ledger charges together. On refusal
neither domain is marked issued. A recorded pair binds each sealer to its position in the issue.

| type | kind | bounds | logical size | cleanup |
|---|---|---|---|---|
| `KeyShare` | resource, mechanical | `cross` | 8 | erases what Hirsel owns of its state (zeroing, below) and refunds 64 bytes |
| `Schedule` | resource, mechanical | `cross` | 8 | zeroes its secrets, retires its sender ledger and refunds its charges |
| `Sealer`, `Opener` | resource, mechanical | `cross` | 8 | zeroes the state Hirsel owns and refunds 1024 bytes |
| `HeaderMask` | resource, mechanical | `cross` | 8 | zeroes the state Hirsel owns and refunds 512 bytes |

Each type is a capability type in the sense of `Secret`
([CAP-2](capabilities.md#cap-2-capabilities-are-opaque)). It has neither `key` nor `codec`, so
a program cannot compare, encode or store one. Each is move-only: `let` moves it, no `Rc` or
`Arc` payload holds one, and no operation shares one. Each crosses to another unit as a
handle ([CONC-15](tasks.md#conc-15-units)). Its state stays where the runtime put it, and
its charges stay with the limits that admitted them.

**Key agreement.** `secret.key_share(r, g, mut public)` generates an ephemeral private key
for `g` and appends its public key to the spare of `public`. For X25519 that is 32 bytes
([RFC 7748 §6.1](https://www.rfc-editor.org/rfc/rfc7748#section-6.1)). For P-256 it is the
65-byte uncompressed SEC 1 point. The private key stays in run-owned memory. A share is used
by exactly one schedule constructor, `tls13` or `dtls12`, which consumes it. No operation
returns the agreed value or makes a `Secret` of it. The results, in this order:

- `Capacity(c)` before issue when `public`'s spare is shorter than the public key, with its
  length as `requested`.
- `Capacity(c)` before issue when the current limit refuses the share's 64 bytes.
- The share.

The constructor computes the shared value. For X25519 that is the 32-byte output. For P-256
it is the 32-byte X coordinate of the product (SEC 1 §3.3.1), which TLS uses as the premaster
secret. It refuses a peer key that is not valid, as a delivered `Invalid`, with the share
consumed. For X25519 that is any length but 32, or an all-zero output, which comes from a
small-order point (ring's `x25519.rs:158-163`). For P-256 it is anything but an uncompressed
point on the curve and not at infinity (ring's `ecdh.rs:99-120`).

The other participant can compute the shared value too, and the program chooses the peer key
it agrees with. So an agreement authenticates nobody. The handshake's signature and Finished
values do that, and they are the library's to check. Opacity (randomness, below) records
where key material came from and who has held it. It never means that no one else knows the
value.

**Signing key and certificate.** `secret.p256_key(r, uses, mut public)` generates a P-256
key. It makes a new designation, with policy `uses`, of the key's PKCS#8 v1 document: the
138 bytes that ring's `EcdsaKeyPair::generate_pkcs8` writes for `ECDSA_P256_SHA256_FIXED_SIGNING`.
That document is a fixed template around the 32-byte scalar and the 65-byte public point.
The results, in this order:

- `Invalid` before issue: `uses` is not exactly `sign(p256)`.
- `Capacity(c)` before issue: `public`'s spare is shorter than 65.
- `Capacity(c)` before issue: the current limit refuses `round_up_8(138 + 32) = 176` bytes
  for the document, as `seal` admits.
- The draws, then the designation and the 65 public bytes appended to `public`.

`sign` with the key signs the certificate. The self-signed certificate whose DER WebRTC
fingerprints ([RFC 8122 §5](https://www.rfc-editor.org/rfc/rfc8122#section-5)) is library
code, in [`std.x509_build`](../../core/std/api/x509_build.md) over `secret.sign`.
`sign(P256)` answers the fixed-width 64-byte
`r ‖ s`. An X.509 signature value is the DER `ECDSA-Sig-Value` SEQUENCE of two INTEGERs
([RFC 3279 §2.2.3](https://www.rfc-editor.org/rfc/rfc3279#section-2.2.3)), so the builder
re-encodes `r ‖ s` into that form before writing the certificate, and the fingerprint hashes
that DER, signature included.

**Randomness.** `key_share`, `p256_key` and `sign` take a `Random`. Every draw happens
inside the operation, and only its results leave it. No draw returns a failure to the
program. A host draw comes from `host.random`'s source, and a virtual stream never fails.

| operation | host `Random` | virtual `Random` ([provide](core.md#provide)) |
| --- | --- | --- |
| `key_share(X25519)` | ring 0.17.14 `EphemeralPrivateKey::generate(&X25519, &SystemRandom)` | one 32-byte draw: the private key, as x25519-dalek's `StaticSecret::from` takes it |
| `key_share(P256)`, `p256_key` | ring's `EphemeralPrivateKey::generate(&ECDH_P256, ..)` or `EcdsaKeyPair::generate_pkcs8`, with `SystemRandom` | 32-byte draws, each a big-endian candidate scalar, until a candidate `k` satisfies `1 <= k < n`, the group order. That candidate is the key, in RustCrypto `p256` 0.14.0's `SecretKey::from_bytes`. The provider loops with no cap; the stream is a pure function of the seed, so the count is too |
| `sign(Ed25519)`, `sign(Rs256)` | no draw: both are deterministic (RFC 8032; ring's PKCS#1 v1.5 padding draws nothing) | no draw |
| `sign(P256)` | ring's `EcdsaKeyPair::sign` with `SystemRandom` | no draw: deterministic ECDSA ([RFC 6979](https://www.rfc-editor.org/rfc/rfc6979)) by `p256` 0.14.0's `SigningKey`, which gives the same `r ‖ s` encoding |

**A host source that fails ends the run.** Core promises that `host.random` fills exactly
the spare it is given and never fails, so it has no result for a source that cannot. TLS-8
makes the consequence explicit, for `host.random` and for every host draw here. Each of the
following is a **terminal provider failure**:

- a `getrandom` error, or a read that returns fewer bytes than asked and then an error;
- ring's P-256 generation rejecting 100 candidates in a row (its `generate` and
  `generate_pkcs8` return `Unspecified`);
- ring's `EcdsaKeyPair::from_pkcs8` failing on its `SystemRandom` while initializing the
  nonce key, after validating the document;
- ring's `EcdsaKeyPair::sign` failing on its `SystemRandom`.

On a terminal provider failure the runtime stops the run with a diagnostic naming the
operation and the source error. No task observes a result: neither a short buffer, nor a
refusal, nor `Invalid`. A recording ends at the last frame written. The native adapter must
not deliver a `Random` request with fewer bytes than its spare, and the signing wrapper must
not map a ring failure on a host `Random` to `Invalid`; `Invalid` stays for a key that is not
PKCS#8 for its algorithm. A virtual tape that runs out is a different thing: it is the
drawing task's `Bounds` fault (`provide.random_tape`, below). Build law
`host_random_failure_ends_run`.

A draw of `n` bytes takes `n / 8` steps of the cell's SplitMix64 stream, eight
little-endian bytes per step, in the order the table gives. So the stream after a TLS-8
operation is exactly where `host.random` calls of those lengths would have left it.

- **Host draws are opaque.** A key share or signing key drawn from a host `Random` is
  **opaque** (TLS-5's key operations), and so is every schedule made from such a share. The
  drawn bytes never pass the replay point.
- **Virtual draws are not opaque.** A key drawn from a virtual stream is computable from
  the seed, as a sealed key is from its bytes. A simulation exists to be reproduced, and this
  keeps a bit-identical simulated trace reachable through a DTLS or QUIC handshake. Such a
  key never meets the host backend for a draw.
- **No mixing.** `sign(P256)` with an opaque key and a virtual `Random` is `Denied` before
  issue. An opaque key is signed only with host randomness. A key drawn virtually may be
  signed with a host `Random`, through ring. The PKCS#8 template is ring's, so ring parses it.

**The simulation backend.** The virtual rows run on RustCrypto `p256` 0.14.0 (features
`arithmetic`, `ecdh`, `ecdsa` and `pkcs8`) and `x25519-dalek` 3.0.0 (`static_secrets` and
`zeroize`), with default features off. A virtual `sign(P256)` takes the same key bytes a
host one does: any PKCS#8 that ring's `EcdsaKeyPair::from_pkcs8` accepts for
`ECDSA_P256_SHA256_FIXED_SIGNING`, not only `p256_key`'s 138-byte template. It decodes them
with `p256`'s `SecretKey::from_pkcs8_der` and applies ring's checks too: PKCS#8 v1, the
`id-ecPublicKey` algorithm with the P-256 curve OID, an RFC 5915 `ECPrivateKey` of version 1
with a 32-byte private key, and the embedded public key present and equal to the scalar's
point. A document that fails any of these is `Invalid`, as on the host path. The X25519
adapter rejects an all-zero agreement with dalek's `SharedSecret::was_contributory`, and
`Invalid` follows exactly as on ring. Neither consumes ring's `test::rand`: a virtual draw is computed by
the provider and handed to the backend as bytes. The host rows stay on ring. Both backends
compute the same functions, so a simulated peer agrees with a host peer (PC8). The
`ring::test::rand` route of the first text is withdrawn. Its fixtures repeat one slice or
panic on an unexpected call, and neither streams draws in order.

**Schedules.** A schedule holds a protocol's secrets in its run-owned storage and derives
everything the protocol needs from them, with the protocol's exact labels. The program
supplies public values: the peer's key, randoms, connection ids and transcript hashes. Each
hash is exactly HashLen bytes (32 for the SHA-256 suites, 48 for SHA-384), each random is
exactly 32 bytes, and a QUIC connection id is 0 to 20 bytes. Every constructor and
transition decides its result in this order, each refusal before issue and with no effect
(a constructor's share is returned): `Invalid` for a hash, random or connection id of the
wrong length, a profile or suite the operation does not take, or a schedule of a kind or at
a stage the operation does not take (the kind table, below); `Spent` for a retired space;
`Issued` for a repeated issue; `Capacity(c)` for a refused admission. The imports check
their designation first and keep their own order (imports, below). Once these checks pass,
`tls13` and `dtls12` **consume the share**: every later result is a delivery, the share is
gone, and its 64 bytes are refunded. The peer key is checked next. A peer key that is not
valid, whatever its length, is `Invalid` as a delivery (key agreement, above); a peer key's
length is never a refusal before issue. Then the agreement registry (sender ownership,
below) can give `Issued` as a delivery. Each delivered refusal refunds the schedule's and
the registry entry's admissions.

| operation | takes | anything else |
| --- | --- | --- |
| `application` | a `tls13` schedule at `Handshake` | `Invalid` |
| `key_update` | a `tls13` schedule at `Application`; a `traffic_import` schedule whose direction is `local`'s (`send` for `true`) | `Invalid`, for `dtls12`, SRTP and `quic_initial` schedules too: they have no TLS 1.3 secret to update |
| `retire` | the `Handshake` space of a `tls13` schedule | `Invalid`; dropping a schedule ends its other spaces |
| `srtp` | a `dtls12` schedule | `Invalid` |
| `finished`, `verify_finished` | `tls13` and `dtls12` schedules | `Invalid` |
| `sealer`, `opener`, `header_mask` | a space the schedule holds, in a direction it holds | `Invalid` |

The constructors and transitions:

- **`tls13(p, side, take k, peer, hello_hash)`** runs the
  [RFC 8446 §7.1](https://www.rfc-editor.org/rfc/rfc8446#section-7.1) schedule with no PSK.
  `hello_hash` is the transcript hash through ServerHello. The early secret is
  `HKDF-Extract(0, 0^HashLen)`. The handshake secret is
  `HKDF-Extract(Derive-Secret(early, "derived", ""), shared)`. The client and server handshake
  traffic secrets are `HKDF-Expand-Label(hs, "c hs traffic" or "s hs traffic", hello_hash,
  HashLen)`. `hello_hash` is already a transcript hash, so it is the context exactly as
  supplied. RFC 8446's `Derive-Secret(s, l, m)` is `HKDF-Expand-Label(s, l,
  Transcript-Hash(m), HashLen)`, so passing the hash where `m` goes would hash it a second
  time and give a key no peer uses (build law `tls13_prehashed_context`). The two
  `"derived"` steps keep `Derive-Secret(s, "derived", "")`, whose context is the hash of no
  messages. The schedule is then at `Handshake`.
- **`application(mut s, finished_hash)`** derives the master secret,
  `HKDF-Extract(Derive-Secret(hs, "derived", ""), 0^HashLen)`, and the application traffic
  secrets, `HKDF-Expand-Label(master, "c ap traffic" or "s ap traffic", finished_hash,
  HashLen)`, with `finished_hash` as the context as supplied. Here `finished_hash` is the
  transcript hash through the server's Finished. It is `Invalid` unless the schedule is a
  `tls13` one at `Handshake`. The handshake traffic secrets stay:
  `finished`, `verify_finished` and the `Handshake` space keep working after `application`,
  because a server sends its first application data before it has the client's Finished,
  and QUIC and DTLS 1.3 keep the handshake keys for retransmission. `retire` ends them.
- **`key_update(mut s, local)`** replaces the sending (`local`) or receiving application
  traffic secret with `HKDF-Expand-Label(secret, "traffic upd", "", HashLen)`. Both QUIC profiles use
  `"quic ku"` ([RFC 9001 §6.1](https://www.rfc-editor.org/rfc/rfc9001#section-6.1)). A QUIC
  header-protection key is not updated (§6). The DTLS 1.3 record-number key is, because
  each key update starts a new epoch with its own `sn` key (RFC 9147 §4.2.3, §8). The
  superseded secret and its keys are zeroed, sealers and openers already issued keep their
  own copies, and its sender ledger retires (sender ownership, below). The kind table gives
  the schedules it takes.
- **`retire(mut s, space)`** erases the secrets and keys of `space`, both directions, and
  retires its ledgers. Sealers, openers and masks already issued keep working until dropped.
  Afterwards `sealer`, `opener`, `header_mask`, `finished` and `verify_finished` for that space
  are `Spent`. A QUIC endpoint retires `Handshake` when the handshake is confirmed
  ([RFC 9001 §4.9.2](https://www.rfc-editor.org/rfc/rfc9001#section-4.9.2)). Only that
  space is retired this way (the kind table); dropping the schedule ends the others. It
  refunds the retired ledgers' entries at the call, live and on replay alike.
- **`quic_initial(side, dcid)`** is [RFC 9001 §5.2](https://www.rfc-editor.org/rfc/rfc9001#section-5.2):
  `HKDF-Extract(initial_salt, dcid)`, with salt `0x38762cf7f55934b34d179ae6a4c80cadccbb7f0a`,
  then `"client in"` and `"server in"`, over SHA-256 with AES-128-GCM. Its keys are computable
  from public values, so the schedule is never opaque. It gives the `Initial` space only. Two
  `quic_initial` schedules for one `dcid` hold the same keys, and no ledger spans them; a
  repeated nonce under them loses nothing, because anyone can compute those keys.
- **`dtls12(suite, side, epoch, take k, peer, client_random, server_random, session_hash)`**
  is the TLS 1.2 schedule ([RFC 5246 §§5, 6.3, 8.1](https://www.rfc-editor.org/rfc/rfc5246#section-5)),
  where `PRF` is `P_SHA256` or `P_SHA384` by suite. The master secret is
  `PRF(shared, "master secret", client_random ‖ server_random)[0..48]`. When `session_hash` is
  `Some`, it is instead `PRF(shared, "extended master secret", session_hash)[0..48]`
  ([RFC 7627 §4](https://www.rfc-editor.org/rfc/rfc7627#section-4)). The key block is
  `PRF(master, "key expansion", server_random ‖ client_random)`, cut in order into
  client_write_key, server_write_key, client_write_IV and server_write_IV. The keys are 16, 32
  and 32 bytes by suite. The IVs are 4 bytes for AES-GCM ([RFC 5288 §3](https://www.rfc-editor.org/rfc/rfc5288#section-3))
  and 12 for ChaCha20-Poly1305 ([RFC 7905 §2](https://www.rfc-editor.org/rfc/rfc7905#section-2)).
  The MAC keys are empty for these AEAD suites. `epoch` is the DTLS epoch the keys protect,
  normally 1. The schedule is at `Application`, and `side`'s write key and IV are its sending
  keys.
- **`srtp(mut s, p)`** derives the DTLS-SRTP keying material from a `dtls12` schedule, once
  ([RFC 5764 §4.2](https://www.rfc-editor.org/rfc/rfc5764#section-4.2)). It is the
  [RFC 5705](https://www.rfc-editor.org/rfc/rfc5705#section-4) exporter
  `PRF(master, "EXTRACTOR-dtls_srtp", client_random ‖ server_random)` in its no-context
  encoding, with no context-length suffix. Its `2 × (key + salt)` bytes are cut into client
  master key, server master key, client master salt and server master salt, in that order.
  It makes a new SRTP schedule with the `dtls12` schedule's opacity. `side`'s master key and
  salt are its sending keys and the other side's are its receiving keys. A second call is
  `Issued`.
- **`import_srtp(p, take s)`** and **`import_traffic(p, take s)`** take one direction's
  keys made outside the run, in the direction the secret's policy names (imports, below).

The TLS 1.2 PRF inputs never alias. Every label is fixed ASCII without a NUL, and every seed
is fixed in length. Under `shared` the schedule runs one PRF only, the master's. Under the
master, `P_hash` feeds HMAC three kinds of input: `label ‖ seed` (key block 77 bytes,
exporter 83, Finished 15 plus HashLen); `A(i)`, which is HashLen bytes of HMAC output that no
caller chooses; and `A(i) ‖ label ‖ seed`, HashLen more than the first. With HashLen 32 the
caller-influenced lengths are 77, 109, 83, 115, 47 and 79; with 48 they are 77, 125, 83, 131,
63 and 111. All differ, so no output of one derivation is an HMAC another computes. Under
[RFC 8446 §7.1](https://www.rfc-editor.org/rfc/rfc8446#section-7.1)'s `HKDF-Expand-Label`, the
label is the prefix followed by the label: `"tls13 "` for TLS 1.3 and QUIC, and `"dtls13"`, with
no trailing space, for DTLS 1.3 ([RFC 9147 §5.9](https://www.rfc-editor.org/rfc/rfc9147#section-5.9)).
So the DTLS 1.3 key label is `"dtls13key"`.

**Each space's keys.** A schedule computes the keys of a space once, at the constructor or
transition that makes the space's secret, and keeps them until `retire`, the `key_update`
that supersedes them, or the drop. The QUIC `Application` header keys are computed at
`application` from the first application secrets and kept across key updates. Every output
has a label of its own:

| profile | payload key | IV or salt | authentication key | header key |
| --- | --- | --- | --- | --- |
| `Quic(s)`, `QuicMultipath(s)` | `"quic key"`, the suite's key length | `"quic iv"`, 12 | — | `"quic hp"`, the key length ([RFC 9001 §5.1](https://www.rfc-editor.org/rfc/rfc9001#section-5.1)) |
| `Dtls13(s)` | `"key"` | `"iv"`, 12 | — | `"sn"`, the key length ([RFC 9147 §4.2.3](https://www.rfc-editor.org/rfc/rfc9147#section-4.2.3)) |
| `Tls13(s)` | `"key"` | `"iv"`, 12 | — | — |
| DTLS 1.2 | key block | key block, 4 or 12 | — | — |
| SRTP `Rtp` / `Rtcp` | KDF label 0 / 3 | label 2 / 5: 14 bytes for CM, 12 for AEAD | label 1 / 4, 20 bytes, CM only | — |

The SRTP KDF is RFC 3711 §4.3.1's AES-CM PRF with key derivation rate 0, as
[RFC 5764 §4.1.2](https://www.rfc-editor.org/rfc/rfc5764#section-4.1.2) requires. A 16-byte
master key gives AES-128 and a 32-byte one gives AES-256 ([RFC 6188 §7](https://www.rfc-editor.org/rfc/rfc6188#section-7)).
The master salt is 14 bytes for `Aes128CmHmacSha1_80` (RFC 3711's 112 bits; a 12-byte salt
is `Invalid` for it). It is 12 bytes for the AEAD profiles, and the KDF first appends two zero
bytes to make 14
([RFC 7714 §11](https://www.rfc-editor.org/rfc/rfc7714#section-11), as corrected by verified
[erratum 4938](https://www.rfc-editor.org/errata/eid4938)). An imported 14-byte salt is kept
as supplied. The KDF places the 14 bytes at the start of a 16-byte zero block, XORs the
label into byte 7 (counting from zero), and generates AES-CM blocks from counter zero under
the master key. The output is the first `len` bytes. The AEAD session salt is 12 bytes. As a
check, libSRTP's `srtp_validate_gcm` (`test/srtp_driver.c` at `fd08747f`) uses master key `00..0f` and master salt `a0..ab`
with `AeadAes128Gcm`. Its labels 0 and 2 give `077c6143cb221bc355ff23d5f984a16e` and
`9af3e95364ebac9c99c5a7c4`.

**Imports.** A key from outside the run enters only through an import:

- an SRTP master key and salt from SDES, or from a pre-shared configuration;
- a TLS 1.3, DTLS 1.3 or QUIC traffic secret from a test vector or an interop harness.

`import_traffic` accepts `Tls13`, `Dtls13`, `Quic` and `QuicMultipath` with their
suite's hash length and labels. A `Dtls13` import holds the `Application` space
and uses the full 64-bit protected-record sequence number.

Two uses join TLS-5's table:

| Use | Permits |
| --- | --- |
| `srtp_import(<dir>)` | `schedule.import_srtp`, with the secret as master key followed by master salt, for direction `<dir>` |
| `traffic_import(<dir>)` | `schedule.import_traffic`, with the secret as one traffic secret, for direction `<dir>` |

`<dir>` is `send` or `receive`. The direction is part of the authority: whoever issues the
secret decides whether its holder may seal, and the holder cannot change it. A policy
naming both directions, or two import uses, is `Invalid`. `narrow` of one direction by the
other leaves no use and is `Invalid`.

An import use is **exclusive**. A policy holding an import use holds no
other use, so `seal`, a grant or `unwrap` with such a policy and any other use is `Invalid`.
`narrow` can only remove uses, so it cannot add one. Neither use is in the derivation child
set, so no derivation makes an import key. `wrap` of a secret holding an import use is
`Denied` before issue, because an unwrap would make fresh bytes that a second import could
use. The canonical text writes the two uses after `wrap` and before `mtls_client_id`.

An import **spends the bytes**. It consumes the designation it is given. On success, and on
an `Invalid` length, it marks the run-owned bytes behind that designation as spent. The
mark covers every designation of those bytes, the ones `share` and `narrow` made included,
in every unit. A later import of spent bytes is `Spent`. The mark is run-wide state that
another unit can change, so an import issues before it reads the mark, and `Spent` is a
delivery ([unit recordings](../recording.md#unit-recordings)). Checking the mark
and setting it are one transition of the run, atomic across units and across every
designation of the bytes, so two imports racing in two units through two aliases give one
schedule and one `Spent`. The import results, in this order, which is the imports' own and
takes precedence over the general order above:

- `Denied` before issue: the secret lacks this import's use (`srtp_import(..)` for
  `import_srtp`, `traffic_import(..)` for `import_traffic`). A spent SRTP designation given
  to `import_traffic` is therefore `Denied`. Then the import issues.
- As a delivery, in this order: `Spent`: its bytes are spent. `Invalid`: the profile does
  not fit the import kind. `Capacity(c)`: the schedule's 512 bytes are refused. Each of
  these returns the secret, with its holder unchanged, and marks nothing. A spent
  designation with an unsupported profile is `Spent`. This is the one admission TLS-8 makes
  after issue: it waits on the mark, which no unit-local state decides.
- `Invalid` as a delivery, with the bytes spent and the secret consumed. For `import_srtp`
  the length is not the profile's master key length (16, 16 or 32) plus 14, or plus 12 for
  the AEAD profiles. For `import_traffic` it is not the suite's HashLen.
- The schedule, which holds the one direction the policy names. A `send` import is the
  sending secret, of the `Rtp` and `Rtcp` spaces or of `Application`, and issues sealers and
  sending masks only. A `receive` import issues openers and receiving masks only. The other
  direction's issue is `Invalid`. SDES gives each endpoint its own sending key, so the
  issuer grants an endpoint its own key as `srtp_import(send)` and the peer's as
  `srtp_import(receive)`. A program granted only the peer's key can therefore never seal in
  the peer's nonce space, whatever it passes (build law `import_direction_from_policy`).

The mark covers the bytes of one grant, seal or unwrap within one run. It cannot see equal
bytes that came in by another route: two grants of one source, the same source in a later
run, a peer that imported the same master, or a program that sealed bytes it also uses
otherwise. Keeping such bytes to one purpose and one sender is the job of whoever issues
them. A program that resumes sending under an imported key must pass a `floor` above every
counter it sent before (sender ownership, below), or obtain a fresh key.

**Sender ownership.** A nonce must never repeat under one key, across the whole key and not
only within one cipher. A schedule enforces this for every key it holds:

- **Send epochs.** Each sending secret is a **send epoch**: an `Initial`, `Handshake` or
  `Application` traffic secret, one key-update generation of an `Application` secret, or the
  `Rtp` or `Rtcp` session keys of an SRTP schedule. The epoch keeps a **ledger** of the
  domains it has issued a sealer for.
- **Issuing.** `sealer(mut s, space, domain, floor)` issues a sealer for the current send
  epoch of `space` and the given domain. It is `Issued` before issue when the ledger already
  holds that domain, whether or not that sealer has since been dropped. A domain is never
  issued twice: the multipath draft forbids reusing a path id for the same reason
  ([§2.4](https://www.ietf.org/archive/id/draft-ietf-quic-multipath-15.html#section-2.4)), and
  RFC 3711 §4.1.1 makes the SSRC part of the counter block for it.
- **Domains.** A domain is a field of the nonce with a fixed width and position (the profile
  table, below). It is the SSRC for SRTP and SRTCP, or the path ID for the `QuicMultipath`
  `Application` space. `Quic`, `quic_initial` and QUIC `Handshake` always use domain 0. A domain wider than its field, or nonzero where there is none, is
  `Invalid` before issue. Domain and counter fields never overlap, so distinct domains give
  distinct nonces under one key.
- **Sealing.** A sealer moves to another unit by moving its handle. It refuses any counter
  that is below `floor` or not above every counter it has sealed. It may skip counters, as
  QUIC skips packet numbers. Past its profile's widest counter it is spent.
- **Retiring.** A ledger retires when its epoch can issue no more: at the `key_update` that
  supersedes it, and when the schedule is dropped. Sealers already issued keep working.
- **Agreements.** Two schedules can hold one key without sharing any object: the two ends
  of an exchange made inside the run, or two shares against peers the program scaled so the
  products meet (share `a` against `c·B` and share `b` against `c·A` both agree on
  `c·ab·G`, and neither peer key is a share's public key). A ledger cannot see either, so
  the run keeps one **agreement registry**. Once `tls13` or `dtls12` holds a valid agreed
  value, it computes the value's **fingerprint**, `SHA-256("hirsel agreement 1" ‖ 0x00 ‖
  shared)`, and looks up the pair (fingerprint, `side`). When the registry holds the pair,
  the result is `Issued` as a delivery, the share consumed and every admission refunded.
  Otherwise the pair is added and the schedule is made. The look-up and the addition are one
  transition of the run, atomic across units. The two ends of a loopback exchange take
  different sides and both pass. Two schedules that would send under one key cannot both be
  made, at the same time or one after the other.
- **Registry lifetime.** An entry records the run's **share count** at its construction:
  how many shares `key_share` has issued so far. The entry retires, its fingerprint erased
  and its 48 bytes refunded to the limit that admitted them, after every counted share is
  dropped or its consuming constructor has decided its verdict. A constructor holds that
  share's protection from issue through validation and the atomic registry transition.
  No later host share can reach the same agreed value. For
  an earlier share `x` against the chosen peer `P`, a host share `y` drawn afterwards would
  need the peer `(x / y)·P`, which only someone who knows `y`'s scalar, and `x`'s or `P`'s,
  can compute. That is the Diffie–Hellman assumption the agreement already rests on. A
  later virtual share can reach it when the program knows the agreed value, which is the
  next case. The registry holds
  fingerprints only. It never passes the replay point itself (replay, below).
- **Keys the program can compute.** An agreed value the program can compute is outside
  this guarantee. That covers one made from a virtual share, from `provide.random` or
  `provide.random_tape`. It also covers a host share against a peer key whose scalar the
  program knows: a virtual share's public key, or any `d·G` it made. Its schedules enter the
  registry like any other and are refused while their entries live. After the entries
  retire, two things can reach the same value again. A new cell with the same seed draws the
  same share. A later virtual share can be aimed at it: host `a` against `d·G`, then a tape
  share `y` against `(d / y)·A`. Such a key protects nothing from the program, as
  `quic_initial`'s protects nothing from anyone with the `dcid`, and a repeated nonce under
  it shows nothing the program could not already compute.
- **Openers.** `opener(s, space)` is open-only authority for the peer's current receive
  secret of `space`. After a receiving `key_update`, an opener issued before it keeps the
  old generation's keys, which a receiver needs for packets reordered across the update. It holds no counter and no per-domain state, and any number may be
  issued.

A key that a schedule derived is never held outside it, every key is computed at most once
per schedule, and the agreement registry keeps two schedules of the run from sending under
one agreed key. So each nonce is used at most once per key across every schedule of the run.
That holds for every key not computable from public values (`quic_initial`) or by the
program (agreements, above). For imported bytes it holds within the mark's reach
(imports, above). Across runs it rests on fresh shares: every exchange draws new ones. Protocol lifetime limits are the library's to keep:

- QUIC counts the packets sealed under each key, and failed authentications across the
  connection ([RFC 9001 §6.6](https://www.rfc-editor.org/rfc/rfc9001#section-6.6));
- SRTP and SRTCP rekey before `2^48` and `2^31` packets
  ([RFC 7714 §§8.4, 9.4](https://www.rfc-editor.org/rfc/rfc7714#section-8.4));
- SRTP's replay window is at least 64 packets and advances only after authentication
  ([RFC 3711 §3.3.2](https://www.rfc-editor.org/rfc/rfc3711#section-3.3.2)).

A protocol that resends a packet under a number it already used resends the bytes it
already protected. It never seals them again.

**Header masks.** `header_mask(s, space, local)` gives the mask key of the sending (`local`)
or receiving side of `space`, for `Quic`, `QuicMultipath` and `Dtls13` schedules (`Invalid` otherwise).
The algorithm is the suite's: AES-128-ECB, AES-256-ECB or ChaCha20. `packet.mask(m, sample,
at)` returns 5 bytes computed from the 16 bytes `sample[at..at + 16]`. When those bytes are
not there, the call faults with `Bounds`, as an out-of-range index does.

- For AES, the first 5 bytes of the AES encryption of the sample
  ([RFC 9001 §5.4.3](https://www.rfc-editor.org/rfc/rfc9001#section-5.4.3)).
- For ChaCha20, `ChaCha20(hp_key, counter, nonce, {0,0,0,0,0})`, with the counter the
  sample's first 4 bytes read little-endian and the nonce its last 12 bytes (§5.4.4).

QUIC applies all 5 bytes (§5.4.1). DTLS 1.3 applies the first 1 or 2 bytes to the record
number (RFC 9147 §4.2.3). A header key is derived under a label of its own and serves only
masks, so its output says nothing about a payload key. ring's public mask API also returns 5
bytes.

**Finished.** `finished(s, of, transcript_hash, mut out)` appends the Finished value of side
`of` and answers its length. For a `tls13` schedule that is `HMAC(finished_key, transcript_hash)`,
with `finished_key = HKDF-Expand-Label(of's handshake traffic secret, "finished", "", HashLen)`,
HashLen bytes ([RFC 8446 §4.4.4](https://www.rfc-editor.org/rfc/rfc8446#section-4.4.4)). For
`dtls12` it is `PRF(master, "client finished" or "server finished", transcript_hash)[0..12]`
([RFC 5246 §7.4.9](https://www.rfc-editor.org/rfc/rfc5246#section-7.4.9)).
`verify_finished` computes the same value and compares it with `received` in constant time.
Other schedules are `Invalid` before issue, and a short `out` gives `Capacity(c)` before issue.
A Finished value is a MAC under a key that serves nothing else, so any number of calls is
safe.

**The profile table.** A sealer or opener holds the payload key, the base, and for CM the
authentication key. The base is the IV or salt placed at the start of `N` zero bytes. `N` is
12 for the AEAD algorithms and 16 for CM. The nonce is the base XORed with the domain and the
counter, each written big-endian into its field. `seq` is the counter. Its width bounds the
sealer and the opener (`Range` when exceeded).

| profile / space | algorithm | domain field | counter field and width | AAD in `buf[aad_at..at]`, then `extra` | tag |
| --- | --- | --- | --- | --- | --- |
| `Quic`, QUIC `Initial` and `Handshake` | the suite's AEAD | none | bytes 4..11, `pn < 2^62` | the header from its first byte; `extra` empty | 16 |
| `QuicMultipath`, `Application` | the suite's AEAD | path ID at bytes 0..3 (32 bits) | bytes 4..11, `pn < 2^62` | the header from its first byte; `extra` empty | 16 |
| DTLS 1.3 | the suite's AEAD | none | bytes 4..11, the full 64-bit sequence number of the epoch's protected records, any `u64`. The 48-bit limit belongs to the `DTLSPlaintext` wire field and does not apply here; the suite's usage limits are the library's ([RFC 9147 §4.5.3](https://www.rfc-editor.org/rfc/rfc9147#section-4.5.3)) | the unified header before record-number encryption ([RFC 9147 §4](https://www.rfc-editor.org/rfc/rfc9147#section-4)); `extra` empty | 16 |
| TLS 1.3 | the suite's AEAD | none | bytes 4..11, 64 bits | the 5-byte record header; `extra` empty | 16 |
| DTLS 1.2 AES-GCM | AES-GCM | none | bytes 4..11: the explicit nonce. A sealer's `seq` is the 48-bit sequence number (`< 2^48`), and the sealer writes the schedule's `epoch` at bytes 4..5 and `seq` at 6..11; an opener's `seq` is the 8 transmitted bytes read big-endian, any `u64` | none (`aad_at = at`); `extra` the 13 bytes `epoch ‖ seq ‖ type ‖ version ‖ length` from the record ([RFC 6347 §4.1.2.1](https://www.rfc-editor.org/rfc/rfc6347#section-4.1.2.1)) | 16 |
| DTLS 1.2 ChaCha20 | ChaCha20-Poly1305 | none | bytes 4..11: `epoch ‖ seq`. A sealer's `seq` is the 48-bit sequence number, with the schedule's `epoch` prepended as for AES-GCM; an opener's `seq` is the 8 bytes `epoch ‖ seq` from the record header | as AES-GCM | 16 |
| SRTP AEAD `Rtp` | AES-GCM | SSRC at bytes 2..5 | bytes 6..11, `ROC ‖ SEQ < 2^48` ([RFC 7714 §8.1](https://www.rfc-editor.org/rfc/rfc7714#section-8.1)) | the RTP header; `extra` empty | 16 |
| SRTCP AEAD `Rtcp` | AES-GCM | SSRC at bytes 2..5 | bytes 8..11, the 31-bit index (`< 2^31`) (§9.1) | `E = 1`: the first 8 bytes; `E = 0`: the whole compound packet, with an empty plaintext. `extra` the 4 bytes `E ‖ index` (§9.2, §9.3) | 16 |
| SRTP CM `Rtp` | AES-128-CM, then HMAC-SHA1-80 | SSRC at bytes 4..7 | bytes 8..13, index `< 2^48` ([RFC 3711 §4.1.1](https://www.rfc-editor.org/rfc/rfc3711#section-4.1.1)) | the RTP header; `extra` the 4-byte ROC (§4.2) | 10 |
| SRTCP CM `Rtcp` | as SRTP CM | SSRC at bytes 4..7 | bytes 10..13, index `< 2^31` | the RTCP header; `extra` `E ‖ index` (§3.4) | 10 |

`QuicMultipath` uses the same labels as `Quic`. For 1-RTT packets it XORs the IV
with the big-endian 32-bit path ID followed by the packet number in 64 bits.
A `Quic` opener refuses a nonzero domain as `Range` before issue.

The DTLS 1.2 GCM sealer's counter is the record number, which the caller also writes as the
8-byte explicit nonce. A peer may choose any explicit nonce, and it is independent of the
record number in the AAD, so an opener takes the transmitted bytes. The four-byte IV belongs
to the DTLS 1.2 AES-GCM rows alone.

SRTCP's byte order differs by algorithm. Under AEAD the tag comes before `E ‖ index`: the
caller seals, then appends the 4 bytes. On receipt it reads the 4 bytes, truncates them, and
passes them as `extra` to `open`. Under CM `E ‖ index` comes before the tag: the sealer puts
the tag after the ciphertext, and the caller moves it 4 bytes on. On receipt it moves the tag
back before calling `open`. That is a 14-byte copy each way on a low-rate stream.

With `E = 0` under CM, nothing is encrypted and the whole RTCP compound packet is
authenticated and stays clear (RFC 3711 §3.4). The caller passes `at = len(buf)`, so the
plaintext is empty and `buf[aad_at..]` is all authenticated data.

**Sealing.** `packet.seal(mut c, seq, mut buf, aad_at, at, extra)` seals in place. The bytes
`buf[aad_at..at]` and then `extra` are authenticated as one byte string, with no padding at
the boundary. The bytes `buf[at..]` are encrypted where they lie. The tag is appended after
them, and the result is the tag's length. For CM the tag is the HMAC-SHA1 of
`buf[aad_at..at] ‖ ciphertext ‖ extra`, truncated to 10 bytes (RFC 3711 §4.2). Bytes before
`aad_at` are not read or changed, so a datagram that coalesces packets seals each one at its
own offset. `packet.seal_into(mut c, seq, src, from, mut dst, aad_at, extra)` authenticates
`dst[aad_at..] ‖ extra` and appends the ciphertext of `src[from..]` and the tag to `dst`. It
answers the number of bytes appended. A forwarding server writes each subscriber's header
into that subscriber's buffer and seals the one shared payload behind it, without copying the
plaintext first.

Each seal checks, in order, before issue and with no effect. A refusal consumes no counter.

- `Bounds`, a fault of the program: `aad_at <= at <= len(buf)` fails, or
  `aad_at <= len(dst)` or `from <= len(src)` fails.
- `Range`: `seq` exceeds the profile's counter width.
- `Reused`: `seq` is below the sealer's `floor`, or not above a counter it has sealed.
- `TooLong`: the plaintext `p` or the authenticated length `a` exceeds the algorithm's
  per-message bound, computed with checked arithmetic. AES-GCM allows `p <= 2^36 − 32` and
  `a <= 2^61 − 1` bytes ([SP 800-38D §5.2.1.1](https://nvlpubs.nist.gov/nistpubs/Legacy/SP/nistspecialpublication800-38d.pdf)).
  ChaCha20-Poly1305 allows `p <= 2^38 − 64` and `a <= 2^64 − 1`
  ([RFC 8439 §2.8](https://www.rfc-editor.org/rfc/rfc8439#section-2.8)). AES-CM allows
  `p <= 2^20` bytes, 65,536 blocks: RFC 3711 §4.1.1 reserves exactly the counter block's low
  16 bits within a packet, and these are zero at its start by the layout above.
- `Capacity(c)`: the spare of `buf` or `dst` is shorter than the appended bytes, with their
  length as `requested`. A failed `len + tag` computation counts as short.
- `Capacity(c)`: the AAD scratch, below, is refused.

A seal then always succeeds, and it never waits.

**Opening.** `packet.open(c, domain, seq, mut buf, aad_at, at, extra)` opens in place. The
last tag-length bytes of `buf` are the tag, and `buf[at..len − tag]` is the ciphertext. When
the tag verifies, the ciphertext becomes the plaintext in place, the tag is truncated away,
and the result is the plaintext's length. `packet.open_into(c, domain, seq, src, aad_at, at,
extra, mut dst)` reads the same layout from `src` and appends the plaintext to the spare of
`dst`. An open takes `c` as a view. Each open checks before issue, with no effect:

- `Bounds`: `aad_at <= at <= len(buf)` fails, or the same for `src`.
- `Range`: `domain` or `seq` exceeds its field.
- `TooLong`: as for a seal.
- `Capacity(c)`, `open_into` only: the plaintext, `len − tag − at` bytes, or 0 when the
  input is shorter than `at + tag`, does not fit the spare of `dst`.
- `Capacity(c)`: the AAD scratch is refused.

As a delivery it gives `Tampered` when the bytes after `at` are shorter than the tag, or the
tag does not verify. A wrong key, nonce, AAD byte, ciphertext byte or tag byte gives the same
answer, and nothing more. After `Tampered` from a short input, `buf` is unchanged, because
there is no ciphertext/tag partition to clear. After `Tampered` from a failed tag, `open`
leaves `buf` at its length, with `buf[at..len − tag]` zeroed and all other bytes unchanged.
`open_into` never changes `dst` on a refusal. No unverified plaintext reaches the program.
AES-GCM and ChaCha20-Poly1305 compare tags in constant time and zero what they decrypted when
the tags differ (ring's `algorithm.rs`, and `os_tls/gcm.rs`'s `open`). CM verifies its HMAC
in constant time before it decrypts anything.

**The AAD scratch.** The runtime feeds the two AAD spans to GHASH, Poly1305 or HMAC in
order, as one string. GCM on `os_tls/gcm.rs` and HMAC-SHA1 stream them through a carry block
of at most 64 bytes. A backend that takes the AAD as one slice cannot stream: ring's `aead`,
which serves ChaCha20-Poly1305 and AES-GCM without VAES. For such a backend the runtime copies
the two spans into run-owned scratch. A run must behave the same on every CPU, so the charge
does not depend on the backend. Whenever both spans are non-empty, every AEAD seal and open
admits `len(buf span) + len(extra)` logical bytes before issue, from the current limit, and
refunds them at return. Only SRTCP's AEAD rows have two non-empty spans. The QUIC, DTLS and
SRTP RTP paths admit nothing. `gcm.rs`'s existing optional trailing byte belongs to the
plaintext, never the AAD, so the build must not pass `extra` through it.

**State, lifetime and erasure.** Hirsel erases storage that it owns and still holds, with
volatile writes of zeros before it releases or reuses that storage. That covers:

- each schedule's secrets, held as byte arrays in its run-owned slot: the agreed value,
  master and traffic secrets, the key block, the exporter output and Finished keys. A secret
  is erased when a transition supersedes it, and the rest when the schedule is dropped. The
  agreed value is erased once the handshake secret or master secret is made from it.
- each sealer's and opener's IV and base;
- on the VAES path, the AES key schedule and GHASH table that `os_tls/gcm.rs` builds. T11
  changes `GcmKey::new` to build them in place in the sealer's or opener's run-owned slot,
  through a pointer to that slot, never into a local that is then returned.

Erasure happens at drop, after a construction refused as a delivery (the share was
consumed), and when a schedule consumes a share. A construction refused before issue
returns the share untouched, with its private key intact, and erases nothing.
PC11 reads the still-allocated slot after erasure and before release.

The rest is not erased. That covers the state of ring values: ChaCha20-Poly1305 and non-VAES
AES-GCM keys, HMAC-SHA1 keys, `EphemeralPrivateKey` and `EcdsaKeyPair`. These hold no heap
buffer, but they are constructed by value and moved. Erasing their final slot says nothing
of earlier copies. Overwriting a live typed value, or reading a dropped one, is not sound.
Such a value is dropped in place in its slot exactly once, and the slot is not overwritten
(the crypto-ring-2115 decision of 2026-11-24: no unsafe overwrites). The simulation backend's values erase themselves
where their crates do (`zeroize`). Registers and stack spills are never covered. core.md's
open item 7 stays open for all of this.

**Charge and steps.** Each charge is admitted before issue from the current limit and paid by
that limit ([MEM-19](budgets.md#mem-19-limits)). A refund after a unit move reaches the
admitting limit ([MEM-20](budgets.md#mem-20-limit-records-and-hand-over)). A failed
construction refunds everything it admitted before it returns.

| item | admits | refunded |
| --- | --- | --- |
| `KeyShare` | 64 | at drop, or when a constructor consumes it: after the schedule's 512 are admitted, at the same point. A constructor refused before issue returns the share with its 64 still held |
| `Schedule` (each constructor and `srtp`) | 512 | at drop |
| an agreement registry entry (`tls13`, `dtls12`) | 48, admitted together with the schedule's 512 | when the entry retires (agreements, above), which may be after the schedule's drop; a refused construction refunds it at once |
| a ledger entry (each issued sealer) | 16 | when its ledger retires |
| `Sealer`, `Opener` | 1024 | at drop |
| `HeaderMask` | 512 | at drop |
| `p256_key`'s document | 176 | when its last designation is released (TLS-5) |
| the AAD scratch | `len(buf span) + len(extra)` | at the call's return |

Every operation in this section takes one step, whatever its lengths. The work of a seal,
open or mask is bounded by bytes that a limit already bounds, which is the AEAD sink's rule.
Charges are logical. No measured per-byte step charge is added. No operation here waits.

**Replay and recording.** Whether key material can be recomputed on replay is a property of
its own, separate from opacity. A `Secret` **has bytes on replay** when the operation that
made its bytes is recomputed on replay: `seal`, and `p256_key` with a virtual `Random`
(`share`, `narrow` and `provide.secret` designate the same bytes). A grant, a derivation,
an `unwrap` and a host `p256_key` make bytes that a replay does not have, whatever their
opacity (TLS-5 replays every use of them from the recording). An unwrap under a
program-sealed KEK is not opaque and still has no bytes on replay. A share, schedule,
sealer, opener or mask has bytes on replay exactly when it is not opaque. Every import reads
a secret with bytes on replay or an opaque one. No unwrap or derivation can make an import
use: `unwrap` refuses `uses` holding one, even for a blob the program wrote itself under a
KEK it sealed, and the derivation child set has none. So every share and schedule, and
every object made from one, has bytes on replay exactly when it is not opaque.

An operation **passes the replay point**
([CAP-8](capabilities.md#cap-8-the-replay-point)) exactly when it draws from a host `Random`,
reads key material without bytes on replay (any opaque object, or a `Secret` as above),
observes an agreement registry verdict at a `tls13` or `dtls12` constructor, or is an
`import_srtp` or `import_traffic`, which observes the run-wide spent mark. A virtual `Random`
counts as a host one in a unit where its operations pass the replay point
([unit recordings](../recording.md#unit-recordings): one made in another unit, or one
whose home has sent it out of its unit), and a `Secret` or key object that arrived from another unit
has no bytes on replay there.
The registry is a run-wide input to the unit. The root records each verdict, including
a verdict for a wholly virtual agreement. Replay reads the verdict from the tape and
never recomputes it. An entry's retirement depends on shares in every unit, so its refund
is an input of the unit that paid it, supplied from that unit's stream; replay never
retires an entry from its own share count. The same holds for a `Secret` designation and
the sealed bytes behind it, whose holders are counted run-wide.
Helper units record the same input in their own streams once
[unit recordings](../recording.md#unit-recordings) are built. Today the run tape records
the root alone. Every other TLS-8 operation is **wholly virtual**. It is recomputed on replay as it
was computed live, with no frame, and its virtual draws advance the stream exactly once
either way. This is core's rule for virtual providers. An operation that does both, such as
`sign(P256)` of a virtually drawn key with a host `Random`, passes the replay point as a
whole: its virtual inputs are in the issue frame, and it draws nothing from the virtual
stream. So does `sign` of any algorithm with a key that has no bytes on replay and a
virtual `Random`, such as a key unwrapped under a sealed KEK: live, it signs from the
run-owned bytes. On replay the signature comes from the delivery, and the virtual stream
advances as it did live, which is not at all, since RFC 6979, Ed25519 and RS256 draw
nothing (build law `dark_key_sign_replays`). `sign(P256)` with an opaque key and a virtual
`Random` stays `Denied`, because an opaque key is signed only with host randomness
(randomness, above). A schedule from a wholly virtual agreement uses the recorded registry verdict.
The peer keys, hashes and received packets still suffice to compute its keys.

When an operation passes the replay point, it writes one issue frame and one delivery frame,
and a packet sealed and masked passes it twice. An issue frame holds every argument that can
change the result, by the canonical-arguments rule ([recording](../recording.md)), and the
spare of every `mut` buffer the call appends to. A delivery holds the origin of every object
the call makes. Admissions happen before issue, except an import's (above), live and on replay alike, so a replay makes
the same charges. On replay, the delivery comes from the tape and the program state changes
as follows. A ledger changes only in the rows that say so.

| operation | issue holds | delivery holds | on replay |
| --- | --- | --- | --- |
| `key_share`, host | `Random`'s identity, group, `public`'s spare | the public key and the share's origin, or a refusal | `public` gains the delivered bytes; the share has no key |
| `p256_key`, host | `Random`, policy, spare | public point and designation origin | as above; the designation has no bytes |
| `sign` | key designation, algorithm, `Random` identity, `msg` by the canonical-arguments rule, `out`'s spare | the signature or a refusal | `out` gains the signature |
| `tls13`, `dtls12` | share origin, profile, suite, side, epoch, `peer`, randoms and hashes | the schedule's origin, or `Invalid` or `Issued` | the share is consumed and its 64 refunded. On success the registry gains an entry with the live share count, and the schedule has no secrets when opaque; a virtual one computes its secrets as live, after the recorded verdict. A refusal refunds as live |
| `import_srtp`, `import_traffic` | designation, profile | schedule origin, or `Spent`, `Invalid` or `Capacity` | the mark is never read. After a delivery other than `Spent` or a profile `Invalid`, the 512 bytes are admitted against the replay's own limit, as live, and a delivered `Capacity` is that admission's result; the bytes are marked spent where live marked them; the schedule's direction comes from the designation's policy |
| `application` | schedule origin, `finished_hash` | done or a refusal | the stage changes as live |
| `key_update` | schedule origin, `local` | done or a refusal | the generation of that direction changes and its old ledger retires, refunding its entries, as live |
| `retire` | schedule origin, space | done or a refusal | the space is spent and its ledgers retire, refunding their entries, as live |
| `srtp` | schedule origin, profile | the child schedule's origin, or a refusal | the parent's `srtp` is used; the child has no secrets |
| `finished` | schedule origin, side, hash, `out`'s spare | the value or a refusal | `out` gains the value |
| `verify_finished` | schedule origin, side, hash, `received` | the verdict or a refusal | none |
| `sealer` | schedule origin, space, domain, floor | the sealer's origin or a refusal | on success the current send epoch's ledger gains the domain |
| `opener` | schedule origin, space | the opener's origin or a refusal | no ledger changes |
| `header_mask` | schedule origin, space, `local` | the mask's origin or a refusal | no ledger changes |
| `seal`, `seal_into` | sealer origin, `seq`, offsets, the plaintext and AAD spans and `extra` by the canonical-arguments rule | every byte written (ciphertext and tag), or a refusal | `buf` or `dst` gets the delivered bytes at the same range and length; the high-water mark becomes `seq` |
| `open`, `open_into` | opener origin, `domain`, `seq`, offsets, the input spans and `extra` by the canonical-arguments rule | the plaintext, or `Tampered` with a reason: short or tag | success: the plaintext replaces the ciphertext and the length drops by the tag, or `dst` gains it; tag failure: `buf[at..len − tag]` is zeroed; short: `buf` unchanged; `dst` unchanged on any refusal |
| `mask` | mask origin, the 16 sample bytes | the 5 mask bytes | the 5 bytes |

A successful open on an opaque path records its plaintext in full, including through
`open_into`. That plaintext is whatever the peer sent: voice and video, or credentials the
peer sent in it. A recording is opt-in, owner-readable and credential-bearing
([recording](../recording.md#credentials)), and that is the boundary this relies on. A build
without recording machinery does nothing at these points beyond the operation (CAP-8), so the
cost target is a non-recording build's.

**Constant time.** As for the AEAD sink: AES runs on AES-NI or VAES where present, otherwise
on ring's `vpaes` or bitsliced `aes_nohw`. GHASH runs on PCLMULQDQ or VPCLMULQDQ, otherwise on
ring's constant-time multiply. ChaCha20, Poly1305, HMAC-SHA1 and the PRFs are fixed
instruction sequences. Tag and Finished comparison is constant time. X25519 and P-256 run
constant-time scalar multiplication in both backends. Every CPU path gives the same bytes.

**Cost.** On x86-64 with VAES and VPCLMULQDQ, sealing or opening a 1,200-byte QUIC packet
under `Aes128GcmSha256`, in place or into a buffer, adds at most 30 ns per packet above
ring's corresponding fused AES-CTR and GHASH call and makes zero allocations. Measure both
in the same locked run on one pinned CPU, with expanded keys, the same payload length and
a release build without recording. The fused baseline starts with the AAD accumulator
ready. The packet operation includes its own AAD, tag and nonce work. The sealer runs
`os_tls/gcm.rs` with its held key schedule and GHASH table. Other algorithms and CPUs
have no target.

The disjoint-buffer native kernel, before language packet handling, measured as follows.
Each median uses 60 interleaved pairs on CPU 16. The fused baseline omits tag generation
and checking. The overhead is the median of the paired differences.

| AES-128-GCM, 1,200 bytes | Fused ring (ns) | Before AAD packing (ns) | Before overhead (ns) | After AAD packing (ns) | After overhead (ns) | Allocations per packet |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Seal | 131.91 | 167.53 | 34.68 | 158.19 | 25.94 | 0 |
| Open | 115.78 | 150.22 | 34.09 | 143.54 | 27.79 | 0 |

Both disjoint-buffer kernel measurements meet the 30 ns bound. After packing the padded
AAD and length blocks into full-width stores, the seal and open cycle profiles attribute
77.75% and 75.89% to fused AES-CTR/GHASH, 9.60% and 11.89% to Rust GCM start and framing,
and 6.49% and 8.33% to one-block GHASH. That GHASH uses the cached H table.
Packet-rate callers use a sealer.

The `quic_path_id_requires_multipath` law checks the RFC 9001 A.2 unipath nonce
and refuses a nonzero domain. It uses [multipath draft §2.4](https://www.ietf.org/archive/id/draft-ietf-quic-multipath-15.html#section-2.4)'s
IV and packet number on paths 3 and 4 and checks both 32-bit path bounds and
62-bit packet-number bounds.

**Checksums.** SCTP's CRC-32C is `crc32c.update` ([crc32c](../../core/std/api/crc32c.md)). No
second spelling is added.

**Laws.**

| law | bug | oracle and setup |
| --- | --- | --- |
| **PC1** QUIC and TLS 1.3 vectors | a wrong label, prefix, nonce placement, AAD or mask | RFC 9001 A.2: `quic_initial(Client, 8394c8f03e515708)`, then seal the listed client Initial payload at packet number 2. That gives the listed protected payload, and `mask` of the listed sample gives `437b9aec36`. RFC 9001 A.5: `import_traffic(Quic(ChaCha20Poly1305Sha256), ..)` of its secret sealed as `traffic_import(send)`, seal at packet number 654360564: the listed packet, and the mask `aefefe7d03`. RFC 8448 §3: a `provide.random_tape` (below) holding the client's X25519 private key, so `key_share` appends the listed client public key. Then `tls13(Tls13(Aes128GcmSha256), Client, .., server public, hello_hash)` opens the server's listed encrypted handshake record with the server handshake key. The client Finished record uses the client handshake key. `rfc8448_handshake_records_use_the_sending_side` checks both directions against the complete RFC records. `finished(Server, ..)` equals its verify_data. After `application`, the client's application record seals to the listed bytes. |
| **PC2** SRTP vectors | a wrong KDF alignment, salt padding, counter block, ROC or index placement | libSRTP `test/srtp_driver.c` at `fd08747f`. `srtp_validate`: `import_srtp(Aes128CmHmacSha1_80, ..)` of the first 30 bytes of `test_key` sealed as `srtp_import(send)`; sealing `srtp_plaintext_ref` and `rtcp_plaintext_ref` gives the file's protected packets. An import of the same bytes, sealed separately as `srtp_import(receive)`, opens them back. `srtp_validate_gcm`: the same with `test_key_gcm` (28 bytes, a 12-byte salt) under `AeadAes128Gcm`, over the RTP and RTCP reference packets. RFC 3711 B.3 (14-byte salt, AES-128) and RFC 6188 §7.2 (14-byte salt, AES-256 KDF, imported under `AeadAes256Gcm`): one sealed packet each equals Python `cryptography` over the appendix's derived session keys. That is CTR plus `hmac` for B.3, and AES-GCM with the first 12 bytes of the derived salt for §7.2. A 12-byte and a 14-byte salt `a0..ab ‖ 00 00` import to equal output. |
| **PC3** one sender per nonce | two sealers on one domain, a counter reused or wrapped into a neighbour, or a refusal that consumes a counter | One `Application` epoch. A second `sealer` for domain 0 gives `Issued`, after the first sealer is dropped too. Designations made by `share` and `narrow` before an import give `Spent` when imported afterwards. `sealer` on an `srtp_import(receive)` import gives `Invalid`. After `retire(Handshake)`, `sealer`, `opener` and `finished` for `Handshake` give `Spent`, and a sealer issued before it still seals its next counter. Two SRTP sealers for SSRCs 1 and 2 each seal index 7 under one key; Python AES-GCM with the hand-built IVs (§8.1) matches both, and the IVs differ. A sealer at index `2^48 − 1` seals, then gives `Range`. Index `2^48` gives `Range` before issue. Seq 7, 6, 0 after 7, and seq below `floor`, give `Reused`. After each refusal `buf` and its length are unchanged, the counting provider saw no call, and the next admissible counter still succeeds. After `key_update(local = true)` a new sealer for domain 0 is issued, and its packet differs from the old generation's. |
| **PC4** in place, out of place, and lengths | `seal_into` authenticating the wrong `dst` range, an AAD split padded at the boundary, or an off-by-one at a size bound | A schedule from `import_traffic(Quic(Aes128GcmSha256), ..)` of a fixed secret, sealed twice, once as `traffic_import(send)` and once as `traffic_import(receive)`, with Python `cryptography`'s `HKDF` (RFC 8446's `HKDF-Expand-Label`) and `AESGCM` as the oracle. Plaintext lengths 0, 1, 15, 16, 17, 1200 and 65,535 seal in place and out of place to Python's bytes, and open back. The SRTCP rows split the AAD over `buf` and `extra` at every offset 0..12 and match Python over the concatenation. CM: `2^20` plaintext bytes seal; `2^20 + 1` give `TooLong` before issue with no counter consumed. |
| **PC5** a failed open leaks nothing | unverified plaintext left behind, or a refusal that tells one failure from another | Over PC4's packets, one flipped tag bit, AAD byte or ciphertext byte: `Tampered`. A wrong domain that is in range: PC2's `srtp_validate_gcm` RTP packet opened with SSRC `0xcafebabf` in place of its `0xcafebabe` gives `Tampered` (a nonzero domain on PC4's unipath QUIC packet is `Range` before issue, which is a different case). Each `Tampered` leaves `buf`'s length unchanged, its ciphertext range zero, the rest unchanged. An input shorter than `at + tag`: `Tampered`, and `buf` unchanged byte for byte. Through `open_into` with a non-empty `dst`: `Tampered`, and `dst` unchanged. |
| **PC6** purpose separation | a key, IV or header key used for another purpose, or an import retyped | Each gives a refusal before issue, and the counting provider sees no call. `header_mask` on a `Dtls12`, `Tls13` or SRTP schedule (`Invalid`). `seal` with an `Opener` and `open` with a `Sealer` are type errors (PC6's refused fence). `seal` of policies `srtp_import(send) mac`, `traffic_import(send) kdf`, `srtp_import(send) traffic_import(send)` and `srtp_import(send) srtp_import(receive)` (`Invalid`). `derive_hkdf_sha256` with child `uses` `srtp_import(send)` (`Invalid`). `wrap` of an import secret (`Denied`). `narrow` of `srtp_import(send)` by `mac`, and by `srtp_import(receive)` (`Invalid`). `aead_seal` with an import secret (`Denied`). With RFC 9001 A.2's Initial secret, the mask key's AES output differs from the payload key's on the same sample. |
| **PC7** key agreement | a missing small-order check, an off-curve peer accepted, or a scalar decoded wrongly | Wycheproof `x25519_test.json` and `ecdh_secp256r1_ecpoint_test.json` at the pinned commit. Each case's private key goes through `provide.random_tape`: X25519's 32 bytes, and P-256's unsigned big-endian scalar decoded to exactly 32 bytes first (215 valid cases have a 33-byte encoding with a leading zero). A `valid` case: `tls13(Tls13(Aes128GcmSha256), ..)` with the case's public key and a fixed `hello_hash`, then seal one record. It equals Python `cryptography` computing the same schedule from the case's `shared`. An `invalid` case, and an `acceptable` X25519 case flagged `ZeroSharedSecret`, gives `Invalid`, with the share consumed and its 64 bytes refunded. Every other `acceptable` X25519 case (flagged `Twist`, `NonCanonicalPublic`, `EdgeCase*`, `SpecialPublicKey` or `SmallPublicKey` with a nonzero `shared`; RFC 7748 §5 masks the top bit and accepts twist points) succeeds as a `valid` one does. The one `acceptable` P-256 case, tcId 2, a compressed point, gives `Invalid`, because TLS-8 takes only the 65-byte uncompressed form; the adapter checks the length and leading `04` before the library's parser, which would accept it. A second use of a moved share is refused by OWN rules. |
| **PC8** simulation determinism and backends | a virtual key read from host entropy, a draw skipped or doubled, a rejected candidate mishandled, or the two backends disagreeing | `provide.random_tape` holding the 32 bytes `ff..ff` (out of range), then a valid scalar, then 8 bytes: `p256_key` gives the valid scalar's public point, which Python `cryptography` derives independently. A following `host.random` of 8 bytes reads exactly the last 8. Two runs with `provide.random(7)` doing `key_share(X25519)`, `p256_key` and `sign(P256)` give byte-identical public keys and signatures, and the signature verifies with `p256.verify`. Separately, `p256_key` from a `random_tape` holding RFC 6979 A.2.5's private key, then `sign(P256)` of `sample`, gives A.2.5's SHA-256 `r ‖ s`. The same program on the evaluator, Cranelift and LLVM gives the same bytes. Recorded and replayed, a `host.random` draw on the same virtual cell after these operations reads the same bytes on both, so the replay advanced the stream exactly as live did. A host `sign(P256)` signature need not equal the virtual one, but it verifies with `p256.verify`. A simulated `key_share` and a host `key_share`, on opposite sides, agree through two `tls13` schedules for each group: the host side's sealed record opens on the virtual side and the virtual side's on the host side. `sign(P256)` with a granted key and `provide.random(7)` gives `Denied`. |
| **PC9** no key on a tape; replay effects | key material in a frame, or replay state that differs from live | A run with a host `Random` makes an X25519 share and runs `tls13` against a fixed peer. It issues a sealer and mask, seals and masks one packet, opens one valid and one tampered packet into a non-empty `buf`, and seals again. The key share comes from a host-provider fixture whose private key is a known sentinel. The tape holds the public key and the packets, and holds none of the sentinel, the shared value, the traffic secrets, the key, IV or header key, raw, hex or base64. Those values come from Python over the sentinel. On replay, the same bytes are written, the tampered `buf` has the same zeroed range, the same charges are admitted, the second seal's high-water mark refuses a repeat, a `provide.random(7)` draw after it reads the same bytes as live, and the run ends `Sealed`. |
| **PC10** DTLS 1.2 schedule | a PRF split off by one, randoms in the wrong order, EMS ignored, or the exporter cut wrongly | The oracle is a Python TLS 1.2 PRF, checked first against the 100-byte SHA-256 and 148-byte SHA-384 PRF fixtures of rustls `rustls-ring/src/tls12.rs` at `a40acf9d`: its first 32 bytes, the bytes at 32..100, and the whole output. With a tape X25519 share and a fixed peer, `dtls12` with and without `session_hash`, for each suite, seals one record whose bytes equal Python AES-GCM or ChaCha20-Poly1305 under the Python key block. For each GCM suite, a peer-direction record produced by Python alone, with explicit nonce `0x0000000000001234` under record number 1 in its AAD and its own tag, opens to its plaintext through the opener: a local sealer always writes the record number as the explicit nonce, so it cannot make this case. ChaCha20 has no explicit nonce and no such case. `srtp` then `sealer(Rtp, ssrc)` seals one RTP packet equal to Python over the exporter's cut. Interop: against `openssl s_server -dtls1_2 -use_srtp SRTP_AEAD_AES_128_GCM -keymatexport EXTRACTOR-dtls_srtp -keymatexportlen 56`, the printed exporter bytes give, through Python, the SRTP packet the Hirsel client seals. |
| **PC11** charge and erasure | state not admitted, admitted twice, not refunded on failure, a ledger that grows without charge, or a slot left with key bytes | Under the reference count: `bound bytes 1023 { sealer(..) }` gives `Capacity` with `requested` 1040 (1024 plus the 16-byte entry), and 1040 succeeds. `bound bytes 63 { key_share(..) }` gives `Capacity` with 64 requested. A `tls13` refused before issue returns the share with the limit unchanged. One refused for an invalid peer leaves the limit where it was before `key_share`. After `key_update`, the old ledger's entries are refunded. `bound bytes 511 { header_mask(..) }` gives `Capacity` with 512 requested. `bound bytes 175 { p256_key(..) }` gives `Capacity` with 176 requested and `public` unchanged, and 176 succeeds. `bound bytes 559 { tls13(..) }` gives `Capacity` with 560 (the schedule's 512 and the registry entry's 48) and returns the share. A sealer moved to another unit and dropped there refunds its 1024 to the limit that admitted it, after the first unit has ended. `p256_key` from a `random_tape` holding `ff..ff` then 16 bytes faults the task with `Bounds`; the limit is back where it was before the call, `public` is unchanged, and a surviving holder of the same `Random` draws next from the 16 bytes. After every drop the limit is back where it was. In the native runtime's unit tests, the live raw storage of a sealer's owned fields (IV, base, and on VAES the key schedule and GHASH table), and of a schedule's secret arrays, reads all zeros after erasure and before release; ring and dalek values are outside the check (erasure, above). Those tests run on the VAES path, and on a successful consume, a refused construction and an ordinary drop. |

`provide.random_tape(take b: Bytes) -> Random` is a virtual `Random`, like `provide.random`,
that answers each draw with the next bytes of `b` in order. A draw longer than what is left
faults the drawing task with `Bounds`, as a read past an array does. It is public `provide`
surface, for vectors that fix a private key. It joins core's `provide` list in T13.

The cell admits 32 bytes plus `len(b)` and keeps the tape and its charge until run teardown,
as for other [virtual cells](core.md#provide). `Random` is a plain, copyable capability, so
dropping a copy does not release its storage or charge
([declaration](../../core/core/core.hirsel#L111), [runtime](../../crates/hirsel-core-rt/src/provide.rs#L10-L11)).
A cursor advances only by whole draws. A draw that does not fit consumes nothing.

When a TLS-8 operation faults this way, nothing it started survives. Its owned provisional
bytes (rejected and accepted candidates, a half-built PKCS#8 document) are zeroed. Everything
it admitted, such as `p256_key`'s 176 or `key_share`'s 64, is refunded to the payer. `public`
is unchanged. A holder of the same `Random` in a task that survives finds the cursor after
the last whole draw, which includes rejected candidates.

How many candidates a draw may reject is bounded differently by source. A tape bounds it by
its own length, `len(b) / 32`, so the bytes its limit admitted bound the work. A seeded
stream has no length bound. A P-256 candidate is rejected with probability below `2^-32`,
and the count for a seed is fixed, so the work is finite. It is not charged per candidate:
under MEM-30 a core call takes one step. That is a different argument from the seal and open
one (charge and steps, above), where bytes a limit already admitted bound the work.

PC1 to PC6 and PC10 belong in `core/core/tests/packet_vectors.hirsel`<!-- doc-ref: planned -->,
with the vector files next to AE's. PC7 and PC8 belong in
`core/std/tests/secret_agree.hirsel`<!-- doc-ref: planned -->. PC9 belongs in the runtime tape
laws. PC11 belongs in the reference evaluator's limit laws and the runtime's unit tests. PC6's
type errors go in a `refused` fence when the module is built.

**Build laws.** These come from the media-crypto-recheck-2252 review, one for each blocker
and high finding. Each is required. The build must first make it fail on the design before
media-crypto-amend2-2257, then make it pass. Each is a case in the harness named, alongside
that harness's PC laws, and none of them adds a harness. No expected value comes from the
build's own output.

| law | finding | scenario | oracle | harness |
| --- | --- | --- | --- | --- |
| `same_agreement_same_side_refused` | B1 | Two host shares `a` and `b`, with public keys `A` and `B`. **(a)** `tls13(Tls13(Aes128GcmSha256), Client, a, B, h)`, then `tls13(.., Client, b, A, h)` with the same `h`. **(b)** The first schedule seals counter 0 and is dropped, its sealer too, and only then is `b` constructed as `Client` against `A`. **(c)** As control, `b` against `A` as `Server`. **(e)** Scaled peers: two X25519 shares from `random_tape`s holding fixed scalars `a` and `b`, constructed as `Client` against `X25519(c, B)` and `X25519(c, A)` for a fixed `c`, the peers precomputed in Python, so neither peer is a share's public key. **(d)** In the tape harness, `a` from a host `Random` and `b` from `provide.random_tape` holding RFC 8448 §3's client private key, recorded and replayed. | (a) and (b): the second construction gives `Issued` as a delivery, `b` is consumed, and the limit is where it was before `key_share` made `b` (its 64, the 512 and the 48 all refunded). Before this amend, both schedules seal counter 0 under one key, and the two records' ciphertexts XOR to their plaintexts' XOR. (c): the server schedule opens the client's record. (e): the second gives `Issued`, as in (a), although no peer key equals a share's public key, which a build matching peer bytes would miss. Once `a` and `b` are both gone, the 48 bytes of the entry are refunded. (d): replay gives the same `Issued` and ends `Sealed`. These follow from the invariant one sending key per side per agreed value (sender ownership), and the charges are worked out by hand from the charge table | `core/core/tests/packet_vectors.hirsel`<!-- doc-ref: planned --> (PC3's cases); (d) in the runtime tape laws (PC9's) |
| `root_virtual_agreement_verdicts_are_recorded_and_replayed` | T14 | Two root tasks hold different virtual X25519 shares and construct as Client against each other's public key. | One is accepted and one is `Issued`. The two verdicts are recorded and replayed without consulting the agreement engine. The 48-byte registry charges return to their respective payers. | the runtime tape laws |
| `helper_virtual_agreement_verdicts_are_recorded_and_replayed` | T14 ([unit recordings](../recording.md#unit-recordings), slice U5; not built) | Two helper units hold different virtual X25519 shares and construct as Client against each other's public key. | One is accepted and one is `Issued`. Each verdict is a frame of its own helper's stream, and each helper replays alone (`hirsel replay --unit`<!-- doc-ref: planned -->) without consulting the agreement engine. The 48-byte registry charges return to their respective payers. | the runtime tape laws |
| `tls13_prehashed_context` | B2 | RFC 8448 §3's handshake secret `1dc826e9…` and its transcript hash through ServerHello, `860c06ed…`, as `hello_hash`, through `tls13` with the tape share of PC1. | The server's handshake record opens with server traffic secret `b67b7d690cc16c4e75e54213cb2d37b4e9c912bcded9105d42befd59d391ad38`. The client Finished record opens with client traffic secret `b3eddb126e067f35a780b3abf45e2d8f3b1a950738f52e9600746a0e27a55a21` (RFC 8448 §3). Hashing `hello_hash` again gives server traffic secret `5491f576b791c18774e3f4398f9de8eca47846e828ac43a00efc324fa5cd57af`, under which the server record fails authentication (Python HMAC-SHA256). The application law also checks the prehashed context against independent record bytes. | [`secret_agree.hirsel`](../../core/std/tests/secret_agree.hirsel) (PC1) |
| `dark_key_sign_replays` | B3 | **(a)** `p256_key` from a `random_tape` holding RFC 6979 A.2.5's private key, policy `sign(p256)`. It is wrapped under a KEK sealed as `wrap` and unwrapped with `sign(p256)`. Then `sign(P256)` of `sample` with `provide.random(7)`. **(b)** RFC 8032 §7.1 TEST 1's secret key as RFC 8410 PKCS#8 v1, sealed as `sign(ed25519)`, wrapped and unwrapped the same way, then `sign(Ed25519)` of the empty message with `provide.random(7)`. Each run is recorded and replayed. | (a) gives A.2.5's SHA-256 `r ‖ s` (`efd48b2a…3716 ‖ f7cb1c94…cda8`), and (b) gives TEST 1's signature `e5564300…7a100b`, live and on replay. Both replays end `Sealed`, and a `host.random` draw on the same cell afterwards reads the same bytes live and on replay. Before this amend the replay has no key bytes and cannot sign | the runtime tape laws (PC9's) |
| `import_direction_from_policy` | H1 | The first 30 bytes of libSRTP `srtp_validate`'s `test_key`, sealed as `srtp_import(receive)`, through `import_srtp(Aes128CmHmacSha1_80, ..)`. Also: `narrow` of that secret by `srtp_import(send)`, and `seal` with policy `srtp_import(send) srtp_import(receive)`. | `sealer(Rtp, 0xcafebabe, 0)` gives `Invalid` before issue, with no ledger entry admitted. An `opener` opens the file's protected RTP packet to `srtp_plaintext_ref`. The `narrow` and the `seal` give `Invalid`. These follow from the libSRTP fixture and the policy rule (imports) | `core/core/tests/packet_vectors.hirsel`<!-- doc-ref: planned --> (PC2 and PC6) |
| `replay_issue_binds_arguments` | H2 | Opaque schedules from a host share against a fixed peer. **(a)** On a `Quic(Aes128GcmSha256)` `tls13` schedule after `application`, `header_mask(Application, true)`, recorded. The source is changed to `false` and replayed. **(b)** On an opaque `dtls12` schedule, `srtp(s, AeadAes128Gcm)`, recorded, then replayed with `AeadAes256Gcm`. **(c)** `opener(Application)`, then `sealer(Application, 0, 0)`. **(d)** `retire(Handshake)`, then `sealer(Handshake, 0, 0)`. **(e)** A host `p256_key`, then `sign(P256)` with `out` spare 63, recorded, then replayed with spare 64. | (a), (b) and (e): `Diverged(Arg)` at that issue (recording's verdict table). (c): live and replay both issue the sealer and admit the same 1024 and 1040, with no `Issued`. (d): live and replay both give `Spent` with the same charges, and the handshake ledger's entries are refunded at `retire` on both. Worked out by hand from the replay table | the runtime tape laws (PC9's) |
| `dtls13_seq_beyond_48_bits` | H3 | A fixed 32-byte secret sealed as `traffic_import(send)`, through `import_traffic(Dtls13(Aes128GcmSha256), ..)`, then `sealer(Application, 0, 0)`. Seal at sequence `2^48`, then at `2^64 − 1`, then at `2^64 − 1` again. A copy of the secret sealed as `traffic_import(receive)` gives an opener, which opens the `2^48` record at sequence `2^48`. | The opener authenticates and returns the plaintext, where the design before this amend gave `Range` before authentication. The first two seal, and each equals Python `cryptography` `AESGCM` under key `HKDF-Expand-Label(secret, "key", "", 16)` and IV `HKDF-Expand-Label(secret, "iv", "", 12)` with prefix `"dtls13"`, the nonce being the IV XOR the 64-bit sequence number (RFC 9147 §§4, 5.9). The third gives `Reused`, because the sealer is spent. Neither of the first two gives `Range` | `core/core/tests/packet_vectors.hirsel`<!-- doc-ref: planned --> (PC4) |
| `host_random_failure_ends_run` | H4 | In the native runtime's unit tests, the `getrandom` call of `os/random.rs` is replaced by a test seam. A `host.random` request with a spare of 32 is issued through the OS provider's issue entry and its `deliver`, the path a native program's call takes. **(a)** The seam fails with `EIO` on the first call. **(b)** It returns 8 of 32 bytes, then fails. | Before this amend, `request_outcome` turns the error into an `Outcome` whose `error` is set and whose status is 0, `deliver` passes it on, and native lowering of `Random` reads the status alone, so the program's call returns with a short buffer. After it, the `probe` records no delivery for either request. Both end the run as a terminal provider failure whose diagnostic names `EIO`. This follows from core's promise that `host.random` fills exactly its spare and never fails, and from TLS-8's terminal rule. The runtime key-issue law also fails Linux's `getrandom` with `EIO` after generating a valid PKCS#8 document. This reaches ring's nonce-key initialization in `from_pkcs8` before signing. The run ends before issue returns, with `P-256 key loading: RNG failed` | `crates/hirsel-core-rt` unit tests of `os.rs`, with its `probe` module |

The mediums of that review are closed in the text and in PC5, PC7, PC8, PC10 and PC11, and
they add no build law.

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
| **CR5** STS file refusals | a wrong refusal, a blocked fetch, or file contents in output | Against CR4's loopback server, missing and empty tokens give `Token(NotFound)`, a permissions failure gives `Token(Denied)`, an oversized token gives `Malformed`, and a token pipe gives `Token(Other(EINVAL))`. Missing, unreadable, invalid PEM, and pipe roots give `Net(Tls(Roots))`. Output contains only the typed refusal labels and no diagnostics. Only the valid control reaches STS. |
| **MAC1** RFC 4231 | the wrong SHA-2 algorithm or failure to hash a key longer than the hash block | Cases 1 and 6 return their full HMAC-SHA384 and HMAC-SHA512 tags. Under a narrowed `verify` key, each full tag verifies and each one-byte-short tag gives `Ok(false)`. |
| **MAC2** charge per message byte | a fixed step cost regardless of message length | In the core law body, HMAC-SHA256 on a one-byte input fits in one step. On RFC 4231 case 1's eight-byte message, signing and verification give `Error(OutOfTime)` under seven steps and fit in eight. |
| **AE1** RFC 8439 | a wrong Poly1305 key, AAD or length block, or the tag placed before the ciphertext | The key of [RFC 8439 §2.8.2](https://www.rfc-editor.org/rfc/rfc8439#section-2.8.2), sealed as `aead(chacha20-poly1305)`, with its nonce, AAD and plaintext: `aead_seal` appends the section's ciphertext followed by its tag and answers 130. With Appendix A.5's key, nonce, AAD, ciphertext and tag, `aead_open` appends A.5's plaintext. |
| **AE2** CAVP GCM | a wrong GHASH, counter start or key schedule | In `gcmEncryptExtIV128.rsp` and `gcmEncryptExtIV256.rsp` from [gcmtestvectors.zip](https://csrc.nist.gov/CSRC/media/Projects/Cryptographic-Algorithm-Validation-Program/documents/mac/gcmtestvectors.zip), every record under `[IVlen = 96]` and `[Taglen = 128]`, with its key sealed as `aead(aes128-gcm)` or `aead(aes256-gcm)`: `aead_seal` appends `CT` then `Tag`. In `gcmDecrypt128.rsp` and `gcmDecrypt256.rsp` under the same headers, a record marked `FAIL` gives `Tampered` and every other record opens to its `PT`. |
| **AE3** Wycheproof corpus | an edge of either construction that the RFC and CAVP records miss | Over `aes_gcm_test.json` and `chacha20_poly1305_test.json` at the commit [core](core.md#signature-verification)'s laws pin: in groups with `ivSize` 96 and a key size the algorithm takes, each `valid` case seals to `ct` then `tag` and opens to `msg`, and each `invalid` case gives `Tampered` on open. In groups with any other `ivSize`, every case gives `Invalid` before issue, including valid AES-GCM tcId 69 and invalid ChaCha20-Poly1305 tcIds 317 and 318. The 192-bit-key AES groups give `Invalid` under both AES algorithms. |
| **AE4** a mismatch says nothing | plaintext released before the tag check, or a refusal that tells one failure from another | AES-GCM tcIds 41 to 46 and ChaCha20-Poly1305 tcIds 146 to 149 (tag bits flipped), AE1's A.5 input with one ciphertext byte changed, with one AAD byte changed, and under a different 32-byte key: each gives exactly `Tampered`, and `out`'s length is unchanged. |
| **AE5** the policy decides | a sink that seals with an open-only key, or uses one key with two algorithms | With a counting provider, each gives `Denied` and the provider sees no call: `aead_seal` under `open(aes256-gcm)`; `aead_open` with `Aes256Gcm` under `aead(chacha20-poly1305)`; either under `mac`; and `aead_seal` under the result of narrowing `aead(aes256-gcm)` by `open(aes256-gcm)`. `aead_open` under `open(aes256-gcm)` opens AE2's first decrypt record. |
| **AE6** argument refusals | a refusal after issue, a partial append, or a key length not checked | With a counting provider, nonces of 11 and 13 bytes give `Invalid` and the provider sees no call. A spare of `len(plain) + 15` gives `Capacity(c)` with `requested` `len(plain) + 16` and `available` `len(plain) + 15`, nothing appended and no call. A 16-byte key under `Aes256Gcm`, a 32-byte key under `Aes128Gcm` and a 16-byte key under `ChaCha20Poly1305` (each sealed with the matching use) give `Invalid` as a delivery. |
| **AE7** no key in a recording | the key reaching a frame | Record AE1's run with the key from a `file:` grant. The tape holds none of the key's raw bytes, hex or base64. Replayed with the file removed, the run appends the same bytes and ends `Sealed`. |
| **AE8** one step, nothing admitted | a per-byte or per-block charge, or the output admitted again | Under the reference count, with `out` reserved for 1 MiB + 16 bytes outside the scopes and moved into them, `bound bytes 0 { bound steps 1 { ... } }` around one `aead_seal` of 1 MiB succeeds, and the seal answers `Ok(1048592)`. By hand from MEM-30 and the charge rule. |
| **KD1** HKDF vectors | Extract and Expand swapped, salt and IKM swapped, or the empty-salt rule wrong | For [RFC 5869](https://www.rfc-editor.org/rfc/rfc5869) A.1, A.2 and A.3, the IKM sealed as `kdf` and the case's salt, info and `L`, with `uses` `mac`: `secret.hmac_sha256(child, "x")` equals Python's `hmac.new(OKM, b"x", "sha256")` over the case's `OKM`. A.3 has an empty salt and info. |
| **KD2** HKDF corpus and length bound | an off-by-one at 255 blocks, or the bound checked after work | Over `hkdf_sha256_test.json` at that commit, each `valid` case passes KD1's check against its `okm`, sizes up to 8160 among them. tcIds 25, 48 and 74 (`size` 8161) give `Invalid` before issue. |
| **KD3** HKDF to AEAD | a child key of the wrong bytes reaching the AEAD sink | A.1's IKM, salt and info with `len = 32` and `uses` `aead(chacha20-poly1305)`, then AE1's nonce, AAD and plaintext sealed under the child: the result equals Python `cryptography`'s `ChaCha20Poly1305(okm[:32]).encrypt(nonce, plain, aad)`, `okm` being A.1's OKM, whose first 32 bytes are the 32-byte output by RFC 5869's construction. |
| **KD4** child policy and opacity | a derived key escalated to a credential, a child of a granted key treated as program-held, or a `kdf`-only key or a sealed child reached as a MAC key through HMAC's long-key hash | Each child policy gives `Invalid` before issue for all three derivations: `tls_send(alpha.test)`, `sign(ed25519)`, `wrap`, `mtls_client_id`, and `mac` from a parent without `kdf`. `derive_hmac_sha256` with `uses` `aead(aes256-gcm)` succeeds. A child derived from a granted parent, wrapped under a sealed KEK, gives `Denied`; a child of a sealed parent wraps. Long keys (with U1 of [turn.md](turn.md#build-slices)): RFC 4231 test case 6's key (131 bytes of 0xaa) sealed as `kdf` only gives `Invalid` before issue with no child for `derive_digest(s, Sha256, "", Raw, "", "mac")`, and sealed as `digest` the same call gives `Invalid` before issue; without the rules, `hmac_sha256` under that child over "Test Using Larger Than Block-Size Key - Hash Key First" returns the published tag `60e43159…0ee37f54`. RFC 5869 A.1's IKM (22 bytes of 0x0b) sealed as `digest`, with `K` its 13-byte salt zero-padded to 64 bytes, gives `Invalid` before issue with no child for `derive_digest(s, Sha256, K ⊕ ipad, Raw, "", "digest")`; without the rule, `derive_digest` of that child with prefix `K ⊕ opad` and `uses` `mac` is A.1's PRK `07770936…d7c2b3e5`, and `hmac_sha256` under it over `info ‖ 0x01`, then `T(1) ‖ info ‖ 0x02`, returns A.1's 42-byte OKM `3cb25f25…87185865`. |
| **KD5** Argon2id derivation | the derivation computed differently from `argon2.id`, the steps taken after the work, or working memory not admitted | A secret sealed from `password` as `kdf`, salt `somesalt`, `m = 256`, `t = 2`, `p = 2`, `len = 32`, `uses` `mac`: `secret.hmac_sha256(child, "x")` equals Python `hmac` keyed with the tag of that case in `src/test.c` ([core AR2](core.md#argon2id-laws)). Under `bound steps 513` the call succeeds, and under `bound steps 512` it gives `Error(OutOfTime(steps: 512))` with no child made and the counting provider seeing no call. Under a limit with `262,207` bytes free it gives `Capacity(c)` with `requested` `262,208` (`262,144` working memory and 64 child storage), and with `262,208` free it succeeds, the working memory refunded after. |
| **KD6** no derivation bytes in a recording | the parent's or child's bytes reaching a frame | Record KD1's A.1 case and KD5 with the parents from `file:` grants. The tape holds none of the IKM, the password, the OKM or the Argon2 tag, raw, hex or base64. Replayed with the files removed, both runs give the same MAC tags and end `Sealed`. |

TL1 to TL11, TL14 to TL16 and SC1 to SC8 run against the OS provider over
loopback, and AE1 to AE8 and KD1 to KD6 against the OS provider. KD1–KD3 are in
`core/core/tests/hkdf_vectors.hirsel`, with KD4–KD5 in `core/std/tests/secret_kdf.hirsel`.
KD6 and the KD4–KD5 no-call checks are in the runtime tape laws. AE5 and AE6
also have scripted-provider cases. TL5, TL7, TL13, TL15, SC1 and SC5 also have scripted-provider cases, so that the
evaluator is covered. SC9 runs on both engines. CR1 to CR5 run natively against loopback servers. The recording laws of
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
| **T8** credential grants | M | T4, T7 | the `Cred` type and grant kind, the `imds` and `sts` sources, `cred.fetch` in the OS provider on a host helper, its frames and grant record in a recording, std `cred` | CR1–CR5 |
| **T9** AEAD sink | M | T4 | `AeadAlg`; the `aead(<alg>)` and `open(<alg>)` uses in policy parsing, `narrow` and the canonical text; `aead_seal` and `aead_open` in the OS provider over ring 0.17.14's `aead` (`LessSafeKey`, sealing and opening in the spare of `out`), and in the scripted and virtual paths as the MAC sink is; their issue and delivery frames in a recording | AE1–AE8 in the core and recording law harnesses |
| **T10** derivation sinks | M | T4, A2 | `derive_hkdf_sha256` over ring's `hkdf` and `derive_argon2id` over A2's wrapper, in the OS, scripted and virtual paths; the derivation rule's child set, applied to `derive_hmac_sha256` too; opacity; the child, working-memory and step charges; their frames in a recording | KD1–KD6 |
| **T11** packet cipher | M | T9, T14 | `packet` module: `Sealer`, `Opener`, `Space`, `PacketError`; `schedule.sealer` and `schedule.opener` with the sender ledgers; `seal`, `seal_into`, `open`, `open_into` per TLS-8's profile table, in the OS provider (AES-GCM over `os_tls/gcm.rs`'s `AesKey` and `HTable` built in place in the slot on VAES, ring's `aead` elsewhere; AES-CM and HMAC-SHA1-80 over ring's AES block and `hmac`); the nonce, domain and counter rules, `Range`, `Reused`, `TooLong`, the AAD scratch, erasure, charges and replay frames | PC1 to PC6, PC10, PC11; build laws `quic_path_id_requires_multipath`, `dtls13_seq_beyond_48_bits`, `replay_issue_binds_arguments` |
| **T12** header mask | S | T11, T14 | `HeaderMask`; `schedule.header_mask` and `packet.mask` (5 bytes) over the AES block and ring's ChaCha20 | PC1's mask cases, PC6 |
| **T13** key agreement and signing keys | M | T4, T10 | `KeyShare`, `Group`; `key_share`, `p256_key` over ring's `agreement` and `EcdsaKeyPair::generate_pkcs8` with `SystemRandom` for host draws, and over RustCrypto `p256` 0.14.0 and `x25519-dalek` 3.0.0 fed by the provider for virtual draws; `sign` gains its `Random` (all callers cut over, deterministic RFC 6979 signing for virtual keys, PKCS#8 decoding with ring's checks); `provide.random_tape` with its fault cleanup; opacity and bytes on replay; terminal provider failures for host draws, in the native adapter and the signing wrapper; std `x509_build` self-signed P-256 certificate with DER `ECDSA-Sig-Value` | PC7, PC8, PC9; build laws `dark_key_sign_replays`, `host_random_failure_ends_run` |
| **T14** key schedules | M | T10, T13 | `schedule` module: `Schedule`, `Side`, `Suite13`, `Suite12`, `Profile13`, `SrtpProfile`, `ScheduleError`; `tls13`, `quic_initial`, `dtls12`, `application`, `key_update`, `srtp`, `finished`, `verify_finished`; the SRTP KDF; `import_srtp`, `import_traffic` and the `srtp_import(<dir>)` and `traffic_import(<dir>)` uses with the atomic spent mark and `wrap`'s refusal; the agreement registry with its share-count retirement and recorded root verdicts, in the OS, scripted and virtual paths. Helper verdicts follow the same rule when [unit recordings](../recording.md#unit-recordings) are built (slice U5) | PC1's Finished case, PC6's use cases, PC10's PRF fixtures; build laws `tls13_prehashed_context`, `same_agreement_same_side_refused`, `import_direction_from_policy` |

T2 and T3 can run in parallel after T1. T4 waits for T2 because its designation in a
recording is part of its contract. T5, T6 and T7 run in any order once their dependencies land.
