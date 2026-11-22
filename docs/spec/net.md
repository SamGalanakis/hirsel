# Net sockets

`Socket`, `ReadHalf`, `WriteHalf` and `Listener` are mechanical resources of
logical size 8. They are `send`, move-only and never `shareable`.
`Ip = V4(u32) | V6(u64, u64)` uses numeric network-order address values;
`Addr { ip: Ip, port: u16 }` has no resolver or scope id.
`Until = Never | At(Clock, u64)` names an absolute monotonic deadline.

### NET-1: Socket operation declarations

Core declares these provider operations, each `waits`:

```hirsel core
pub fn connect(n: Net, a: Addr, until: Until) -> Result[Socket, NetError] waits
pub fn listen(n: Net, a: Addr, backlog: u16) -> Result[(Listener, Addr), NetError] waits
pub fn accept(mut l: Listener, until: Until) -> Result[(Socket, Addr), NetError] waits
pub fn recv_bounded(mut r: ReadHalf, mut buf: Bytes, max_receive: u64, until: Until) -> Result[u64, NetError] waits
pub fn send_pair(mut w: WriteHalf, prefix: Bytes, tail: Bytes, until: Until) -> Result[u64, NetError] waits
```

Receive appends at most `max_receive` bytes and cannot exceed the caller's
spare capacity. Send attempts the concatenation of its two borrowed slices
and returns the number of prefix bytes committed across both. Each operation
keeps one absolute deadline through readiness retries.

An address argument outside the grant returns `Denied` before issue.
`listen` has a backlog, with no `Until` parameter.
`NetError = Denied | Refused | Unreachable | InUse | Reset | TimedOut | Aborted | Other(u32) | Tls(TlsFailure) | BoundNotListening(u32)`.
[TLS-1](tls.md#tls-1-client-handshake) defines `Tls`. On a TLS stream, receive and send
move plaintext ([TLS-2](tls.md#tls-2-plaintext-on-a-tls-stream)).

### NET-7: Owned connections

`net.split(take s: Socket) -> (ReadHalf, WriteHalf)` consumes the socket and
transfers its existing record to two directional owners. It allocates and admits
nothing, cannot fail, and passes no replay point. Dropping a write half performs
`SHUT_WR`, ignoring the result. On a TLS stream it first queues `close_notify` and makes
one nonblocking attempt to send it ([TLS-2](tls.md#tls-2-plaintext-on-a-tls-stream)). Dropping a read half releases its ownership.
The last owner closes the descriptor once. Dropping an unsplit socket or a
listener closes it once. No finalizer waits or enables positive linger.

```hirsel core
pub type Unjoined = Unjoined(ReadHalf, WriteHalf)
pub fn join(take r: ReadHalf, take w: WriteHalf) -> Result[Socket, Unjoined]
```

`net.join` is `split` undone: it consumes the two halves of one plain socket and gives back
that socket, with the record `split` divided. It is how a client upgrades an established
TCP connection to TLS in place (STARTTLS): the program exchanges plaintext over the
halves, joins them, and passes the socket to `tls.connect`
([TLS-1](tls.md#tls-1-client-handshake)). The joined socket is the same connection: same
descriptor, peer, issuing provider and object ordinal, with nothing sent, received, shut
down or buffered by the join. Like `split`, it allocates and admits nothing, takes one step,
does not wait, passes no replay point, and on a socket of `provide.net` asks the server
nothing ([core](core.md#provide)).

These checks run in order. The first that fails returns `Error(Unjoined(r, w))` with both
halves unchanged and still usable, and nothing else happens:

1. `r` and `w` are not the two halves of one socket.
2. The socket is a TLS stream. Its halves stay halves; there is no TLS inside TLS.
3. The socket was not made by `host.connect` (TLS-1's check 1). A socket from `accept` is
   never the client side of a handshake, and joining it has no other use.

A STARTTLS client must read the server's go-ahead with `max_receive` equal to the go-ahead's
length, so that no byte past it reaches program memory as plaintext. Bytes the program
buffered before TLS starts but treats as arriving after it are the classic STARTTLS
injection hole: an attacker on the path appends plaintext to the go-ahead, and the client
reads it as if the server had sent it under TLS (CVE-2021-23222 in libpq, CVE-2011-0411 in
Postfix). Bytes that the host has received but nobody has read are TLS-1's to refuse: a
handshake fails with `Tls(Protocol(0))` and sends nothing when such bytes are present
([TLS-1](tls.md#tls-1-client-handshake)).

For PostgreSQL before 17, the go-ahead is one byte. The client sends the 8-byte
`SSLRequest`, `00 00 00 08 04 D2 16 2F` (length 8, then request code 80877103), and the
server answers `S` to proceed with TLS or `N` to refuse it:

```hirsel
pub type StartTls = Secured(ReadHalf, WriteHalf, Handshake) | Declined(ReadHalf, WriteHalf, u8) | Ended() | NotClient()

fn pg_starttls(n: Net, take s: Socket, name: String, trust: Trust, until: Until) -> Result[StartTls, NetError] waits {
  let (r, w) = net.split(take s)
  let mut read = take r
  let mut write = take w
  let req: Bytes = b"\x00\x00\x00\x08\x04\xD2\x16\x2F"
  let mut sent: u64 = 0
  while sent < 8 { sent += host.send_pair(mut write, list.slice(req, sent, 8), list.slice(req, 8, 8), until)? }
  let mut reply: Bytes = list.with_capacity(1)
  if host.recv_bounded(mut read, mut reply, 1, until)? == 0 { return Ok(Ended()) }
  match list.get(reply, 0) {
    Some(0x53) => {},                                                     // `S`: go ahead
    Some(b) => return Ok(Declined(take read, take write, b)),                     // `N`, or anything else
    None => return Ok(Ended()),
  }
  match net.join(take read, take write) {
    Ok(take joined) => match tls.connect(n, take joined, name, trust, list.new(), until) {
      Ok((take secure_read, take secure_write, h)) => Ok(Secured(take secure_read, take secure_write, h)),
      Error(e) => Error(e),
    },
    Error(_) => Ok(NotClient()),
  }
}
```

`max_receive` 1 is the bound: the receive cannot append a second byte, whatever the server
or an attacker sent.

### NET-6: Network rules

`net.narrow(n: Net, r: Rule) -> Result[Net, RuleError]` intersects each parent
rule with `r`, dropping empty intersections. Address rules have `dir: NetDirection`,
`prefix: Ip`, `bits: u8`, `ports: (u16, u16)`, where
`NetDirection = Connect | Listen`. A `Tls` rule has name patterns, ports and a pinned trust
instead ([TLS-3](tls.md#tls-3-authority)), and a `Resolve` rule has name patterns alone
(NET-11). In Hirsel, use
`Tls(names: String, ports: (u16, u16), trust: TlsPin)` with `SystemPin` or
`Sha256(Bytes)`. The latter holds the 32 raw digest bytes. It permits only a client handshake on a socket
that a `Connect` rule already allowed. An invalid prefix length, nonzero prefix host
bits, reversed ports or IPv4-mapped IPv6 prefix returns `Invalid` without changing
anything; `RuleError` is `Invalid()`. Rule storage is admitted, and a refused admission
raises `Capacity` ([MEM-21](budgets.md#mem-21-admission)). The immutable result storage remains charged to its payer
until the run ends; plain copies of `Net` add no charge or refund.

A run grants Net with an explicit rule list:

```sh
hirsel run app --grant 'Net=Connect 127.0.0.0/8 1-65535;Listen 127.0.0.1/32 8000-9000'
```

The syntax is `--grant 'Net=<dir> <prefix>/<len> <lo>-<hi>[;<rule>]'`, where
`dir` is `Connect` or `Listen` and the prefix is an IPv4 or IPv6 literal. A `Tls` rule is
written `Tls <pattern>[,<pattern>] <lo>-<hi> <trust>`, and a `Resolve` rule
`Resolve <pattern>[,<pattern>]`. Its trust is `System` or
`Roots=sha256:<hex>`. An example:
`--grant 'Net=Connect 203.0.113.7/32 443-443;Tls api.example.com 443-443 System'`. Bare
`--grant Net` is refused with the required syntax. Full authority is explicit,
for example `Connect 0.0.0.0/0 0-65535`; it is never implicit.

A rule permits its direction, prefix and inclusive port interval only. A wildcard
listen requires a `/0` rule of the same family. Listen port 0 requires the whole
`0-65535` interval. IPv4-mapped IPv6 and unscoped `fe80::/10` are denied.
The unspecified address is denied as a connect target. Accepted peers inherit
the listener's authority; there is no peer rule check. Handles cannot become Net
values or widen authority.

A native launch may supply one inherited TCP listener at descriptor 3 using
`LISTEN_PID` and `LISTEN_FDS`. The runtime uses activation metadata addressed
to its PID and requires exactly one listening stream socket. It restores
close-on-exec and clears `LISTEN_PID`, `LISTEN_FDS`, `LISTEN_FDNAMES` and
`LISTEN_PIDFDID` on receipt. The first authorized `listen` takes that socket
without binding or changing its backlog. The socket's actual bound address
must also pass the calling grant's Listen rule. An IPv6 wildcard listener
must be IPv6-only so a same-family rule covers every accepted connection.
The runner starts accepting after it publishes `Ready`. Dropping the listener closes only
this process's descriptor, leaving the host's socket and accept queue live.

Graceful finish, prefix I/O, try-first waiting, reset propagation and the scheduler's
poll registration are specified by NET-1 and NET-7 and implemented after W1a.

### NET-11: Name resolution

`host.resolve(n: Net, name: String, mut out: List[Ip], until: Until) -> Result[u64, NetError] waits`
is the one way a program turns a DNS name into addresses, and it needs a `Resolve` rule
of `n` that covers `name`. A query name is itself a message to the resolver and to every
nameserver it reaches, so resolution is gated by name as a handshake is
([TLS-3](tls.md#tls-3-authority)), not by address. The rule is
`Resolve(names: String)` in `Rule`, written `Resolve <pattern>[,<pattern>]` in a grant,
with TLS-3's pattern syntax and normalisation:

```sh
hirsel run app --grant 'Net=Resolve api.example.com,*.internal;Connect 10.0.0.0/8 443-443'
```

`net.narrow` intersects `Resolve` patterns as TLS-3 intersects `Tls` names, and drops an
empty intersection. A resolved address grants nothing. Connecting to it still needs a
`Connect` rule, so `Resolve` alone reaches no host but the resolver.

The host provider resolves through the system resolver (`getaddrinfo` with a stream
socket type, so `/etc/hosts` and the configured nameservers apply), as Go's
`net.LookupIP` with the cgo resolver and Rust's `ToSocketAddrs` do. The `Resolve` rule
covers the caller's name, which the provider passes to `getaddrinfo` as written, and the
host's resolver configuration, including search-list expansion of a relative name, is the
operator's policy. The provider runs the lookup on a
helper, so the operation waits after issue and is a cancellation point at issue and while
it waits ([CONC-7](tasks.md#conc-7-cancellation-points)). Cancellation there gives
`Aborted`, and a passed `until` gives `TimedOut`. A lookup still running when it is aborted
is discarded. An IP literal `name` is covered by a pattern for that literal, and it
resolves to itself without a query.

`resolve` appends to `out`, at most its spare capacity, the distinct addresses in the
resolver's order (RFC 6724 when the host sorts), and returns how many it appended.
Addresses past the spare capacity are not delivered, so a caller that needs every address
reserves for them. A name with no address (`EAI_NONAME`, `EAI_NODATA`) is `Ok(0)`. An
uncovered or malformed name is `Denied` before issue, with no query. A temporary resolver
failure (`EAI_AGAIN`) is `Unreachable`, and any other failure is `Other(code)`. The
operation admits nothing.

Its issue passes the replay point with `n`'s ordinal, `name`, the spare capacity and
`until`, and its delivery with the addresses or the error
([CAP-8](capabilities.md#cap-8-the-replay-point)). A replay supplies the recorded addresses
and makes no query ([recording](../recording.md#what-a-recording-holds)).

## Unix-domain sockets

The NET-12 to NET-14 core declarations are checked. The laws describe the required
runtime behavior.

### NET-12: Unix rules

```hirsel core
pub type UnixDirection = UnixConnect | UnixListen | UnixBind | UnixSend
pub type Rule = Rule(dir: NetDirection, prefix: Ip, bits: u8, ports: (u16, u16)) | Tls(names: String, ports: (u16, u16), trust: TlsPin) | Resolve(names: String) | Unix(dir: UnixDirection, root: String, path: String)
```

A `Unix` rule names one exact endpoint as a **root label** and a valid path beneath it
([core](core.md#contracts-the-signatures-do-not-state)). The launcher binds each root label to
a host directory, which it opens before `main`. The rule's direction permits one kind of
operation:

- `UnixConnect` permits a stream connect;
- `UnixListen` permits a stream bind and listen;
- `UnixBind` permits a datagram bind, which then receives;
- `UnixSend` permits a datagram connect or send-to.

A run grants them with the Net rule list:

```sh
hirsel run app --grant 'Net=UnixRoot pg /run/postgresql;UnixConnect pg .s.PGSQL.5432'
```

`UnixRoot <label> <absolute-path>` binds a label. A rule whose label no `UnixRoot` of the same
grant binds is refused with the required syntax, and so is a label bound twice. A `UnixRoot` is
not a rule: it grants nothing alone, and `net.narrow` cannot name one. `net.narrow` intersects
a `Unix` rule only with a `Unix` rule of the same direction, root and path, and drops every
other intersection. So an address, `Tls` or `Resolve` rule never matches a Unix operation, and
a `Unix` rule never matches anything else. A path that is not valid, or an empty label, gives
`Invalid`. A narrowed `Unix` rule's storage is 8 plus its label's and path's bytes, rounded up
to 8, added to the narrowed Net's rule storage ([NET-6](#net-6-network-rules)).

A Unix rule authorizes only the lookup or creation its operation needs. It grants no listing,
no file I/O and no unlink. A `Dir` grants no connect, bind or send. An operator who wants a
service to remove a stale socket node grants a writable `Dir` for that as well. There are no
abstract names, no unnamed binds, no peer credentials and no descriptor passing. A Linux
abstract name ignores file permissions and would need its own name space of grants. A
descriptor passed over a socket duplicates authority to an open object that a byte tape cannot
reconstruct. An approved pathname designates whichever peer occupies that slot, and that peer
may act on what it receives, as an IP peer may.

A run with a `Unix` rule probes once before `main` that it can bind and connect beneath a
root as NET-13 does, even when it holds no `Dir`. Where it cannot, the run does not start and
its one line names the missing operation.

Law: for every pair of Net rule lists, `net.narrow` keeps a `Unix` rule exactly when both
lists hold it. Each operation that designates a Unix endpoint succeeds only under a matching
rule of its originating Net. Opening an unbound datagram socket designates no endpoint and
gives no endpoint authority. A connected send uses its previously authorized peer, a receive
needs a bound socket, and accepted streams and existing connections keep the authority they
were made with. The host, a replay and a served Net make the same checks.

### NET-13: Unix streams

```hirsel core
pub opaque resource type UnixListener
pub type NetError = Denied | Refused | Unreachable | InUse | Reset | TimedOut | Aborted | Other(u32) | Tls(TlsFailure) | BoundNotListening(u32)

pub fn connect_unix(n: Net, root: String, path: String, until: Until) -> Result[Socket, NetError] waits
pub fn listen_unix(n: Net, root: String, path: String, backlog: u16) -> Result[UnixListener, NetError] waits
pub fn accept_unix(mut l: UnixListener, until: Until) -> Result[Socket, NetError] waits
```

An endpoint that the rules do not permit, or a root label of no `UnixRoot`, gives `Denied`
before issue. So does a path whose bytes do not fit the host's socket address, which is never
truncated. Each operation admits its result handle before issue, so a node is never created
for a result that cannot be delivered. Everything that depends on host files is decided after issue and delivered, and a
replay supplies it without looking at a host path. A connect, bind or listen resolves `path`
beneath its root as a `Dir` path is resolved, through no symbolic link and never out of the
root, and acts on what that resolution found. There is no fallback that checks a path and then
hands it to the kernel to resolve again.

- Every operation first resolves the endpoint's parent components. A symbolic link, a
  non-directory or an escape among them gives `Denied`.
- `connect_unix` then resolves the final component, checks that it is a socket, and keeps
  that socket's identity through the connect. A final component that is a symbolic link or
  any node other than a socket gives `Denied`. A successful connect reaches that socket.
  Its listener may close or stop accepting before the connect completes, and the connect
  then fails with the host's error, such as `Refused`, with no second lookup.
- `listen_unix` pins the parent directory and binds one validated final component inside
  it. The final component is the bind slot: an existing node of any kind there, a regular
  file, a directory, a socket or a symbolic link, gives `InUse`.
- A pinned directory is the directory itself, not its path, as for every `Dir` handle. If a
  process outside the run moves that directory while the bind runs, the node is created where
  the directory now is. An operator who needs every node to stay beneath the root keeps the
  root's subtree from being renamed by others.
- Bind creates the node. A listen that fails after its bind committed gives
  `BoundNotListening(code)`, which says that this call created a node in the slot and that
  its listen then failed. It says nothing about the slot at delivery: another process may
  already have removed or replaced that node. Every other `listen_unix` error says that this
  call created no node. The provider never unlinks a node: not on this error and not when a
  `UnixListener` is dropped. Removing one is a `Dir` operation.
- Only `listen_unix` produces `BoundNotListening`. Any other Unix or IP operation that
  delivers it is `Corrupt` on a tape and fails a server with `Fail`.

A pathname names a slot, not a peer. A later operation resolves the slot anew, so if a
legitimate rebind replaces the node, a later connect reaches the new listener.

A connected or accepted Unix socket is a `Socket` with NET-1 and NET-7's I/O, halves,
deadlines, partial commits and drop rules. Accepted peers inherit the listener's authority, and
there is no peer address. `host.accept` on a `UnixListener` is a type error. `net.join` refuses
a Unix socket's halves (NET-7 check 3), and `tls.connect` refuses a Unix socket
([TLS-1](tls.md#tls-1-client-handshake)'s check 1), so TLS and granted `Secret` checks stay on
their TCP origin. Connect starts nonblocking and reports success only once connected. `listen_unix`
has a backlog and no `Until`.

Laws:

- when a symbolic link, a non-directory or a directory outside the root is among the
  endpoint's parent components at resolution, connect and bind give `Denied` and reach no
  socket outside the root;
- for connect, a symbolic link or a non-socket node at the final component gives `Denied`;
  for bind, any existing node there gives `InUse`;
- when the slot is replaced after the connect resolved it, that connect either reaches the
  socket it resolved or fails, and never reaches the replacement; a later connect resolves
  the new slot;
- a listen that fails after its bind gives `BoundNotListening`, and that call created the
  node; a listen that fails before its bind creates no node.

### NET-14: Unix datagrams

```hirsel core
pub opaque resource type UnixDatagram
pub type Received = Received(len: u64, truncated: bool)

pub fn open_datagram(n: Net) -> Result[UnixDatagram, NetError] waits
pub fn bind_datagram(n: Net, root: String, path: String) -> Result[UnixDatagram, NetError] waits
pub fn connect_datagram(mut d: UnixDatagram, root: String, path: String) -> Result[(), NetError] waits
pub fn send_datagram(mut d: UnixDatagram, b: Bytes, until: Until) -> Result[(), NetError] waits
pub fn send_to(mut d: UnixDatagram, root: String, path: String, b: Bytes, until: Until) -> Result[(), NetError] waits
pub fn recv_datagram(mut d: UnixDatagram, mut buf: Bytes, until: Until) -> Result[Received, NetError] waits
```

`UnixDatagram` is a mechanical resource of logical size 8, `send`, move-only and never
`shareable`. Its authority is the Net it was made from:

- `open_datagram` makes an unbound socket, which can only send;
- `bind_datagram` needs a `UnixBind` rule and binds to receive at that endpoint, as
  `listen_unix` binds;
- `connect_datagram` fixes one peer and needs a `UnixSend` rule;
- `send_to` needs a `UnixSend` rule for its destination in the originating Net;
- `send_datagram` needs a connected socket, and `recv_datagram` a bound one.

Each of these checks is made before issue and gives `Denied` with no host effect. Each
operation that makes a socket admits it before issue, as NET-13's operations do. No Net is
passed per send, so a datagram never combines two capabilities' authority
([CAP-7](capabilities.md#cap-7-attenuation)). The provider sends to the very socket it
resolved, as NET-13 connects.

A send commits the whole packet or fails, and an empty packet is a packet. `recv_datagram`
receives one packet. It appends at most `buf`'s spare capacity and returns the packet's whole
length and whether it was cut. A packet longer than the room is truncated, and its rest is
lost. There is no sender address: a sender's address would be a host path, and it would grant
no reply authority. Control data is never received, so a descriptor sent to the run is closed
by the host. Each operation keeps one absolute deadline through readiness retries. A receive or
send whose host call has committed delivers its result even when cancellation arrives after.

Law: a receive with room `r` of a packet of length `len` appends exactly `min(r, len)` bytes
and reports `len` and `truncated` equal to `len > r`, including at room 0. A zero-length
packet is delivered as one. A send of `n` bytes either delivers one packet of `n` bytes or
returns an error having delivered none.

**Replay.** The grant snapshot holds each `Unix` rule's direction, root label and path, and
never a root's host path. An issue holds the Net or handle ordinal, the root label, the path,
the backlog or deadline where the operation has one, and the offered bytes or room. A
delivery holds the new ordinal, the committed outcome, or the packet with its length and
truncation flag. Dropping a host-issued `UnixListener`, `UnixDatagram` or Unix socket half
is the provider's `CLOSE` at its position, as for every host handle and for a `Proc`
([EXEC-8](exec.md#exec-8-exec-at-the-replay-point-and-served-exec)). A packet delivery whose
appended bytes, length and flag break the law above is `Corrupt`, found before the buffer
changes.

**Served Unix.** A `provide.net` Net also permits every Unix endpoint through one internal rule
of each direction over every root and path. A universal rule intersected with a concrete
`Unix` rule gives that concrete rule. No grant or tape can write it. Its port receives these
calls and answers them with these replies, in addition to the existing ones
([core](core.md#provide)):

```hirsel unbuilt
pub type UnixOp = ConnectUnix(root: u64, path: u64) | ListenUnix(root: u64, path: u64, backlog: u16) | AcceptUnix(listener: u64) | OpenDatagram | BindDatagram(root: u64, path: u64) | ConnectDatagram(datagram: u64, root: u64, path: u64) | SendDatagram(datagram: u64, len: u64) | SendTo(datagram: u64, root: u64, path: u64, len: u64) | RecvDatagram(datagram: u64, room: u64)
pub type UnixReply = UnixSocket(u64) | UnixListening(u64) | Datagram(u64) | Packet(len: u64, truncated: bool)
```

These cases join `NetOp` and `NetReply` themselves. They are shown apart here only because
those types are built. The caller makes the handle-state checks above before it queues a call.
A `Packet` reply must have staged exactly `min(room, len)` bytes with `truncated` equal to
`len > room`. An error reply to `RecvDatagram` delivers no bytes and discards whatever the
server staged. Any other packet, or a reply of the wrong kind for its call, fails the server
with `Fail` before the caller changes. A server models a listener as a named queue and a
datagram endpoint as a bounded packet queue. It commits a dequeue or a send only after its
reply returns `Ok(())`. A virtual server never receives a host directory.
