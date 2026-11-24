# Net sockets

`Socket`, `ReadHalf`, `WriteHalf` and `Listener` are mechanical resources of
logical size 8. They are `send`, move-only and never `shareable`.
`Ip = V4(u32) | V6(u64, u64)` uses numeric network-order address values;
`Addr { ip: Ip, port: u16 }` has no resolver or scope id.
`Until = Never | At(Clock, u64)` names an absolute monotonic deadline.

### NET-1: Socket operation declarations

Core declares `host.connect`, `host.listen`, `host.accept`, `host.recv_bounded`,
`host.recv_ready` and `host.send_pair`. Each operation is `waits`. Run `hirsel inspect --core --interface`
for their signatures.

```hirsel core
pub fn connect(n: Net, a: Addr, until: Until) -> Result[Socket, NetError] waits
pub fn listen(n: Net, a: Addr, backlog: u16) -> Result[(Listener, Addr), NetError] waits
pub fn accept(mut l: Listener, until: Until) -> Result[(Socket, Addr), NetError] waits
pub fn recv_bounded(mut r: ReadHalf, mut buf: Bytes, max_receive: u64, until: Until) -> Result[u64, NetError] waits
pub fn recv_ready(mut r: ReadHalf, until: Until) -> Result[(), NetError] waits
pub fn send_pair(mut w: WriteHalf, prefix: Bytes, tail: Bytes, until: Until) -> Result[u64, NetError] waits
```

Receive appends at most `max_receive` bytes and cannot exceed the caller's
spare capacity. Send attempts the concatenation of its two borrowed slices
and returns the number of prefix bytes committed across both. Each operation
keeps one absolute deadline through readiness retries.

`recv_ready` waits, with no buffer and no capacity, until the next receive on the half
would not wait: bytes are buffered, the peer has ended its write side, or the half has an
error. End and error count as ready, so a wait on an ended half returns `Ok(())` at once.
It consumes nothing. A host may report readiness early, so the next receive can still
time out when its own deadline is reached; a caller treats that receive's `TimedOut` before
its overall deadline as stale readiness and waits again. An error may surface either from
the wait or from the next receive. The wait returns `TimedOut` at its deadline and is a
cancellation point ([CONC-7](tasks.md#conc-7-cancellation-points)). On a TLS stream it is
ready when decrypted plaintext is buffered, the peer has sent `close_notify`, or the
stream has an error. On a pipe served by an Exec port, which has no readiness call, it is
ready at once and the next receive waits.

An address argument outside the grant returns `Denied` before issue.
An ephemeral listen whose matching rules leave a nonzero port uncovered returns
`EphemeralOutsideNetGrant` before issue. `listen` has a backlog, with no
`Until` parameter.
`NetError = Denied | Refused | Unreachable | InUse | Reset | TimedOut | Aborted | Other(u32) | Tls(TlsFailure) | BoundNotListening(u32) | EphemeralOutsideNetGrant`.
[TLS-1](tls.md#tls-1-client-handshake) defines `Tls`. On a TLS stream, receive and send
move plaintext ([TLS-2](tls.md#tls-2-plaintext-on-a-tls-stream)).

`net.local_addr(s: Socket)` and `net.peer_addr(s: Socket)` return the local and
remote TCP endpoints. `net.listener_addr(l: Listener)` returns the bound TCP
endpoint. The queries borrow the resource and return `Result[Addr, NetError]`.
They `waits` because a served Net can supply the answer. A native TCP query
reads the socket's address without waiting for network I/O.

### NET-7: Owned connections

`net.split(take s: Socket) -> (ReadHalf, WriteHalf)` consumes the socket and
transfers its existing record to two directional owners. It allocates and admits
nothing, cannot fail, and passes no replay point. Dropping a write half performs
`SHUT_WR`, ignoring the result. On a TLS stream it first queues `close_notify` and makes
one nonblocking attempt to send it ([TLS-2](tls.md#tls-2-plaintext-on-a-tls-stream)). Dropping a read half releases its ownership.
The last owner closes the descriptor once. Dropping an unsplit socket or a
listener closes it once. No finalizer waits or enables positive linger. A send that
ends in `Aborted` or `TimedOut` marks its socket abandoned until the next send on it
starts: closing an abandoned socket resets the connection (`SO_LINGER` with a zero
timeout), so bytes still queued for a peer that stopped reading are dropped at the close
instead of being retried after it, and the peer sees a reset, not end of stream. A later
send clears the mark, so a connection that recovers closes normally. The abort itself
still delivers at once; the host first takes back any send the kernel still holds for
the socket, so the close is not held open by it.

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
listen requires a `/0` rule of the same family. Listen port 0 lets the kernel
choose a port. The matching Listen rules must together cover every nonzero port
(`1-65535`) so the kernel cannot bind outside the grant. IPv4-mapped IPv6 and
unscoped `fe80::/10` are denied.
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

Core's `Rule` type includes `Unix` for directory-relative endpoints. Use
`hirsel inspect --core --interface` for the Unix declarations and
`hirsel explain NET-1` for the TCP operations.

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

```hirsel core
pub type NetOp = Connect(addr: Addr) | Resolve(name: u64, room: u64) | Listen(addr: Addr, backlog: u16) | Accept(listener: u64) | Recv(socket: u64, room: u64, max: u64) | Send(socket: u64, prefix: u64, tail: u64) | Handshake(socket: u64, name: u64, alpn: u64, system: bool) | SendSecret(socket: u64, wire: u64, secrets: u64, marks: u64) | Released(id: u64, part: Part) | ConnectUnix(root: u64, path: u64) | ListenUnix(root: u64, path: u64, backlog: u16) | AcceptUnix(listener: u64) | OpenDatagram | BindDatagram(root: u64, path: u64) | ConnectDatagram(datagram: u64, root: u64, path: u64) | SendDatagram(datagram: u64, len: u64) | SendTo(datagram: u64, root: u64, path: u64, len: u64) | RecvDatagram(datagram: u64, room: u64) | Ready(socket: u64)
pub type NetReply = Done | Failed(NetError) | Count(u64) | Socket(u64, Addr) | Listener(u64, Addr) | Secured(Handshake) | UnixSocket(u64) | UnixListening(u64) | Datagram(u64) | Packet(len: u64, truncated: bool)
```

The server receives these variants in `NetOp` and answers with the corresponding `NetReply`
variants. `Ready(socket)` is one `recv_ready`: it carries no room and no payload, `Done`
answers it when that socket's next `Recv` would not wait, and `Failed` answers it with the
error. Any other reply fails the server with `Fail`. The `root` and `path` fields hold byte lengths. `provide.payload` reads root and
path from fields 0 and 1 of ConnectUnix, ListenUnix and BindDatagram, and from fields
1 and 2 of ConnectDatagram and SendTo. Packet bytes are in field 1 of SendDatagram and
field 3 of SendTo. The caller checks handle state before queueing a call.
A `Packet` reply must have staged exactly `min(room, len)` bytes with `truncated` equal to
`len > room`. An error reply to `RecvDatagram` delivers no bytes and discards whatever the
server staged. Any other packet, or a reply of the wrong kind for its call, fails the server
with `Fail` before the caller changes. A server models a listener as a named queue and a
datagram endpoint as a bounded packet queue. It commits a dequeue or a send only after its
reply returns `Ok(())`. A virtual server never receives a host directory.

## UDP

Version note (2026-11-24, media-w0-udp-2201): first text of NET-15 to NET-17, Wave 0.1 of the
real-time media track. Not yet built; the fences are `unbuilt`. MEM-19 and MEM-24 gain the
receive pool charge in the same change.

UDP gives programs IP datagrams for media, QUIC and DNS-like protocols. Its operations
move **batches**: one call receives or sends many datagrams, so a program pays one call,
one wait and one host crossing for a burst. The host's segmentation offloads
(`UDP_SEGMENT` on send, `UDP_GRO` on receive) and its choice of io_uring or epoll are
invisible: a program, a recording and a served Net see individual datagrams, each whole or
explicitly truncated, never a coalesced train.

### NET-15: UDP rules

```hirsel unbuilt
pub type NetDirection = Connect | Listen | UdpBind | UdpSend
```

UDP authority is two more directions of the address rule of
[NET-6](#net-6-network-rules), with the same prefix, prefix length and inclusive port
interval, the same `Invalid` refusals, the same rule storage (37 bytes) and the same
intersection: `net.narrow` keeps a `UdpBind` or `UdpSend` rule only against a rule of the
same direction, and drops every other intersection. A `Connect` or `Listen` rule never
permits a UDP operation, and a UDP rule never permits a TCP one.

- `UdpBind` permits binding a socket to a local endpoint, which then receives
  ([NET-16](#net-16-udp-sockets-and-batches)). It follows `Listen`'s rules exactly: a
  wildcard bind needs a `/0` rule of the same family, and a bind to port 0 needs the
  matching `UdpBind` rules to cover every nonzero port (`1-65535`) or it returns
  `EphemeralOutsideNetGrant` before issue.
- `UdpSend` permits sending to a destination endpoint. Every datagram's destination is
  checked, in every batch. The unspecified address, IPv4-mapped IPv6, unscoped
  `fe80::/10` and the IPv4 limited broadcast `255.255.255.255` are denied as destinations.
  The host never sets `SO_BROADCAST` and joins no multicast group, so a directed broadcast
  the kernel refuses is `Denied` after issue.

A run grants them with the Net rule list:

```sh
hirsel run sfu --grant 'Net=UdpBind 0.0.0.0/0 3478-3478;UdpSend 0.0.0.0/0 1-65535'
```

**Receiving grants no reply authority.** A datagram's source address is unauthenticated: an
attacker can write any address there. If receiving from a peer let a program answer it, a
spoofed source would make the program send to a host the operator never named, the
reflection that QUIC's address validation (RFC 9000 §8) and STUN's spend their address-validation machinery on.
So a reply needs a `UdpSend` rule like any other send. A server that answers the whole
Internet holds `UdpSend 0.0.0.0/0 1-65535` (and its IPv6 twin), which says so in the grant.
A TCP peer is different because its handshake proved the address, and NET-6 lets accepted
peers inherit the listener's authority for that reason.

Law: a UDP operation that designates an endpoint succeeds only under a matching rule of the
same direction in its originating Net, and the check is made before issue with no host
effect. `net.narrow` keeps a UDP rule exactly as NET-6 keeps an address rule. The host, a
replay and a served Net make the same checks.

### NET-16: UDP sockets and batches

```hirsel unbuilt
pub opaque resource type UdpSocket
pub type Steer = Hash | Quic
pub type UdpOptions = UdpOptions(slots: u32, slot_bytes: u32)
pub type UdpPacket = UdpPacket(peer: Addr, at: u64, len: u64, truncated: bool, ecn: u8)
pub type UdpTx = UdpTx(to: Addr, at: u64, len: u64, ecn: u8)

pub fn udp_open(n: Net, v6: bool, opts: UdpOptions) -> Result[UdpSocket, NetError] waits
pub fn udp_bind(n: Net, a: Addr, opts: UdpOptions) -> Result[(UdpSocket, Addr), NetError] waits
pub fn udp_bind_group(n: Net, a: Addr, opts: UdpOptions, members: u16, steer: Steer, mut out: List[UdpSocket]) -> Result[Addr, NetError] waits
pub fn udp_recv(mut s: UdpSocket, mut buf: Bytes, mut out: List[UdpPacket], until: Until) -> Result[u64, NetError] waits
pub fn udp_send(mut s: UdpSocket, data: Bytes, pkts: List[UdpTx], until: Until) -> Result[u64, NetError] waits
pub fn udp_local_addr(s: UdpSocket) -> Result[Addr, NetError] waits
```

`NetError` gains one variant, `Oversize(max: u32)` (below).

`UdpSocket` is a mechanical resource of logical size 8. It is `send` and `cross`
([CONC-2](tasks.md#conc-2-the-send-bound)): like `Socket` and `Listener`, it may move to
another unit, so one unit per core can own its socket. It is move-only and never
`shareable`. Its authority is the Net it was made from:

- `udp_open` makes a socket of one family that the kernel binds to an ephemeral port at its
  first send, as a TCP connect takes an ephemeral local port with no `Listen` rule. It needs
  no `UdpBind` rule. It receives only after its first committed send; before that, `udp_recv`
  returns `Denied` before issue.
- `udp_bind` needs a `UdpBind` rule (NET-15) and returns the socket and its bound endpoint,
  with the kernel's port when `a.port` is 0.
- `udp_bind_group` binds `members` sockets (2 to 1024) to one endpoint with `SO_REUSEPORT`
  in one call, and appends them to `out` in member order; `out`'s spare capacity must hold
  `members`, or the call returns `Denied` before issue. The kernel spreads arriving
  datagrams across members by `steer` (below). With `a.port` 0, the first member takes the
  kernel's port and the others bind to it.
- `udp_send` checks every packet's `to` against `UdpSend` (NET-15); `udp_recv` needs a bound
  socket or an opened socket that has sent.

**Options.** `slots` is the number of receive slots and `slot_bytes` the size of each; both
are explained under the receive pool below. `slots` must be a power of two from 1 to 32,768
and `slot_bytes` a multiple of 8 from 1,536 to 65,792. Any other value returns `Denied`
before issue, as an invalid address does. Every option has an effect on every provider;
there is no option a host may ignore. There is no buffer-size option: `SO_RCVBUF` and
`SO_SNDBUF` are clamped by the kernel to `net.core.rmem_max` and `wmem_max` without telling
the caller, which a hidden cap may not do. The socket keeps the host's defaults, and the
pool below is what a program sizes.

**Datagram sizes.** A datagram payload is at most 65,507 bytes on IPv4 and 65,527 on IPv6,
the largest a UDP header's length field and the IP header allow without jumbograms. A send
packet over its family's maximum returns `Oversize(max)` before issue with nothing sent. The
host sets `IP_MTU_DISCOVER`/`IPV6_MTU_DISCOVER` to `PMTUDISC_PROBE`, so datagrams are never
fragmented by the sender. A packet larger than the route's MTU is refused by the kernel
(`EMSGSIZE`) as `Oversize(0)`: the route's limit is smaller than that packet, and an
unconnected socket cannot read it (`IP_MTU` needs a connected one). A program doing path MTU
discovery probes for it, as RFC 8899 does. An empty datagram is a datagram.

**Receiving.** `udp_recv` waits until at least one datagram is queued for the socket or its
deadline passes, then delivers a batch:

1. Datagrams are taken in the socket's arrival order. For each, the host appends its payload
   to `buf` at offset `at` (the length of `buf` before that append) and appends one
   `UdpPacket` to `out`. `peer` is the source address and port. `ecn` is the two ECN bits of
   the IP header's traffic class (RFC 3168), read with `IP_RECVTOS`/`IPV6_RECVTCLASS`.
2. The first datagram is always delivered: when its payload is longer than `buf`'s spare
   capacity `r`, exactly `r` bytes are appended, `truncated` is true, `len` is its whole
   length and its rest is lost, as in [NET-14](#net-14-unix-datagrams). A datagram longer
   than one slot's payload room (`slot_bytes - 256`) is truncated to that room the same way,
   before it reaches `buf`.
3. Each later datagram is delivered only whole: a datagram that does not fit `buf`'s
   remaining spare capacity, would need an entry beyond `out`'s spare capacity, or was cut by
   its slot, stays queued for the next receive, where it is first. So batching never
   truncates a datagram and never loses one.
4. A batch ends when the queued datagrams run out, `out` is full or the next datagram does not
   fit. It never waits for more after the first.

`udp_recv` returns the number of datagrams delivered, at least 1, and admits nothing: the
caller's spare capacities bound the batch, as `host.resolve` does (NET-11). With `out` spare
capacity 0 it returns `Ok(0)` at once, consumes nothing and does not wait.

A datagram the kernel received with a `UDP_GRO` coalesce is delivered as its individual
datagrams. Every datagram of one coalesce has the same source, the same ECN bits and the
same length, except that the last may be shorter (the `UDP_GRO` control message gives the
stride). There is no receive timestamp, no destination address and no interface index in
W0.1. The ECN field is the one control datum a QUIC sender needs to validate ECN
(RFC 9000 §13.4.2); the others have no consumer yet.

**Sending.** `udp_send` sends `pkts` in order: packet `i` is the bytes
`data[at, at + len)` to `to` with ECN bits `ecn` (`IP_TOS`/`IPV6_TCLASS`), each `at + len`
at most `data`'s length and `ecn` at most 3, or the call returns `Denied` before issue with
nothing sent. Every packet's `to` is checked first (NET-15); one uncovered destination
denies the whole call with nothing sent. The call returns `k`, the number of packets
committed: packets `0..k` were each handed to the kernel exactly once as one datagram, and
none after them was. Each packet commits whole or not at all; there is no partial datagram.
When the kernel refuses packet `k` after packets `0..k` committed, the call returns `Ok(k)`;
the caller resends from packet `k`, which is then first and gets the error. So an error is
returned only by a call that committed nothing, and the failing packet is always the first
one not committed. The call waits while the socket's send buffer
is full and keeps one absolute deadline through readiness retries; at the deadline it
returns the `k` committed so far, or `TimedOut` if none. A program sends a GSO train by
listing equal-length packets to one peer one after another: the host groups any run of
consecutive packets with the same `to` and `ecn`, all of one length but the last which may be
shorter, into one `UDP_SEGMENT` send of up to 64 packets and at most 65,507 bytes in all.
That grouping is invisible: it changes the number of kernel crossings, never which bytes
reach the wire, in what order, or `k`.

**Order and loss.** UDP may drop, duplicate and reorder datagrams on the network, and the
kernel drops datagrams when a socket's receive queue is full. The host adds none of these.
Between the kernel queue and the caller, every datagram the host takes is delivered once,
in the kernel's order; the host never drops one to make room.

**Steering a group.** `udp_bind_group` attaches one classic BPF program to the group
(`SO_ATTACH_REUSEPORT_CBPF`). The kernel delivers a datagram to the member whose index the
program returns:

- `Hash` returns a fixed hash of the source address and port, read from the network header,
  modulo `members`, so one flow stays on one member. It does not use the skb's receive hash
  (`SKF_AD_RXHASH`), which reads the raw `skb->hash` (`filter.c:412-415`) and is 0 when
  neither the NIC nor the stack has computed one, which would send every datagram to member 0.
- `Quic` reads a QUIC v1 or v2 header (RFC 9000 §17). A server issues every connection ID
  with the owning member's index in its first byte. A short-header packet (fixed bit set,
  long bit clear) steers by the first byte of its Destination Connection ID, at offset 1. A
  Handshake long-header packet steers by the first byte of its Destination Connection ID, at
  offset 6. An Initial or 0-RTT packet carries a connection ID the client invented, so it and
  every other datagram, including a version this cannot read and a datagram too short for its
  loads, steers by `Hash`. A retransmitted Initial keeps its source, so it reaches the member
  that received the first one, which owns the handshake. The program checks each datagram's
  length before every load, because a classic BPF load past the end of the packet ends the
  program with 0, which would pick member 0. This is `moq-sock`'s filter
  (`/tmp/ref-moq/rs/moq-sock/src/shard.rs:40-50,812-833`) for one generation.

Because the program returns an index below `members`, a socket that some other process of
the same user later joins to the group (Linux lets any process of the same effective UID
join, `udp.c:157-159`) is placed past the members and never chosen. A member that is
dropped would let the kernel move the last socket into its index
(`sock_reuseport.c:142-143`) and shift the steering. So the host keeps a dropped member's
descriptor open until every member of its group is dropped, and discards what it receives.
Member indices stay fixed for the group's life. Datagrams the group steers to a dropped
member are lost until the whole group is dropped, so a program that stops serving a member
keeps it, or rebinds the group.

**Endpoint query.** `udp_local_addr` returns the bound endpoint, or `Denied` for an opened
socket that has not yet sent. It borrows, admits nothing and waits only as NET-1's queries
do.

**The receive pool.** Each socket owns a receive pool of `slots` slots of `slot_bytes` each.
The host receives into a slot and copies the payload to the caller's `buf` at delivery, so a
slot is never lent to program memory. A slot returns to the pool once every datagram in it has
been delivered; a coalesce that a batch delivered only in part keeps its slot until the rest
is delivered.
The first 256 bytes of each slot are reserved for the host's per-datagram header (on
io_uring, `io_uring_recvmsg_out`, the source address and the control messages); the rest is
the slot's payload room. When `slot_bytes` is 65,792 the payload room is 65,536, at least
the largest possible coalesce, and the host enables `UDP_GRO`. With any smaller value it does
not, because the kernel truncates a coalesced datagram to the buffer it is given and the
datagrams after the cut would be lost (`udp.c:2093-2098`). This is the rule that
`quinn-udp` states for its receive buffers (`quinn-udp/src/unix.rs:133-141`), made a
program-visible choice: a socket that many small flows reach wants many small slots; one
that a few fast flows reach wants few large ones.

The pool is run memory that the program sized, so it is charged, unlike other host memory
behind a provider ([MEM-19](budgets.md#mem-19-limits)). `udp_open` and `udp_bind` admit
`slots × slot_bytes` bytes, and `udp_bind_group` `members × slots × slot_bytes`, before
issue, to the limit current at the call, together with each result handle. A refused
admission raises `Capacity` with no host effect. The charge is the same on io_uring, on
epoll, on a replay and against a served Net, because it is computed from the options alone
([CAP-5](capabilities.md#cap-5-provider-operations)). The pool's record keeps its payer when
the socket moves to another task or unit ([MEM-20](budgets.md#mem-20-limit-records-and-hand-over)),
and dropping the socket refunds the whole charge to the limit that record leads to, at the
drop, even while the host is still retiring the pool and its kernel registration. A
cross-unit refund is an input of the paying unit, as for every refund.

**Steps.** Each operation is one call step ([MEM-30](budgets.md#mem-30-step-bounds)).
`udp_recv` takes one further step for each 64 entries, or part of 64, of `out`'s spare
capacity, and `udp_send` one for each 64 packets, or part of 64, of `pkts`. Both are taken at
the call, before issue, from the call's arguments, so they never depend on how many datagrams
arrive or commit, and a bound stops a run at the same step on the host, on a replay and against
a served Net. The extra steps make a step bound limit the per-datagram work of a batch as
`crc32c.update`'s per-chunk steps limit hashing.

**Deadlines and cancellation.** Each operation keeps one absolute deadline through readiness
retries and is a cancellation point at issue and while it waits
([CONC-7](tasks.md#conc-7-cancellation-points)). A receive or send whose host call has
committed delivers its result even when cancellation arrives after it. A cancelled send
returns `Aborted` only when it committed nothing; otherwise it returns the `k` committed.

**Dropping.** Dropping a socket closes it once (a group member: at the group's last drop),
with no linger. Datagrams still queued in the kernel or the pool are discarded.

Laws:

- a receive delivers at least one datagram; the first with room `r` appends exactly
  `min(r, room, len)` bytes, where `room` is the slot's payload room, with `truncated` equal to
  `len > min(r, room)`; every later datagram of the batch is whole; `at` of each entry is the
  length of `buf` before its payload; and the datagrams delivered across successive receives
  are exactly the kernel's queue, in order, with none lost or repeated by the host;
- a send returning `k` committed packets `0..k`, each once and whole, and none after them,
  whatever the host's GSO grouping, its backend, or a GSO send the kernel refused and the host
  resent as single datagrams;
- the charge admitted at open or bind equals the refund at drop, and depends only on the
  options and `members`;
- a group member's index never changes while the group lives, and no socket outside the
  group's members receives a datagram the group's steering program chose.

**Moving between units.** A socket moved to another unit keeps its descriptor, its bound
endpoint, its pool, its pool's payer and its group index. A datagram the host took from the
kernel but had not delivered moves with it and is delivered first on the new owner, so a move
never loses, repeats or reorders a datagram. Law: for any interleaving of moves and receives,
the datagrams delivered across all owners are the kernel's queue in order. Its harness is
the existing socket-transfer tests (`crates/hirsel-core-net/src/reactor/tests.rs`), and the
oracle is the peer's sent sequence on loopback.

**Replay.** The grant snapshot holds each UDP rule's direction, prefix and ports. An issue
holds the Net or socket ordinal, the endpoint or family, the options, `members` and `steer`,
the spare capacities of `buf` and `out`, the packets and their bytes for a send, and the
deadline. A delivery holds the new ordinals and bound endpoint, or the delivered datagrams
(peer, length, truncation, ECN and appended bytes), or `k`, or the error. A replay supplies
the recorded datagrams and touches no socket; which datagrams formed a batch is part of the
record, so a replay delivers the same batches. A delivery that breaks the receive law above,
or a send delivery with `k` above the number of packets, is `Corrupt`, found before any buffer
changes. Dropping a host-issued `UdpSocket` is the provider's `CLOSE` at its position.

**Served UDP.** A `provide.net` Net permits every UDP endpoint through one internal rule of
each UDP direction, as served Unix does (NET-14); intersecting it with a concrete rule gives
the concrete rule. Its port receives these calls in addition to NET-14's:

```hirsel unbuilt
pub type UdpNetOp = UdpOpen(v6: bool, slots: u32, slot_bytes: u32) | UdpBind(addr: Addr, slots: u32, slot_bytes: u32, members: u16, steer: Steer) | UdpRecv(socket: u64, room: u64, entries: u64) | UdpSend(socket: u64, data: u64, pkts: u64) | UdpLocal(socket: u64)
pub type UdpNetReply = UdpSockets(first: u64, members: u16, addr: Addr) | UdpPackets(count: u64)
```

These variants join `NetOp` and `NetReply` when NET-16 is built. `UdpSend` is answered with
the existing `Count(k)`, `UdpLocal` with `Address`, and any call with `Failed`; the build lane fixes their
payload field numbers with `provide.payload`, as NET-14's are fixed. A `UdpBind` with
`members` 1 is `udp_bind`. A `UdpPackets` reply must have staged exactly the batch the
receive law allows for its `room` and `entries`, or the server fails with `Fail` before the
caller changes; a `Count` reply to `UdpSend` above its packet count fails it the same way.
The caller has already checked the rules, the options and the handle state before queueing the
call, and has admitted the pool. A simulated network (`std.sim.udp`, Wave 0.3) is a server
over exactly these calls: it decides loss, delay, reordering, duplication and address
rebinding from its seed, and `hirsel test --sim` stands in for the host through them.

### NET-17: UDP datapaths

NET-16's behaviour is the same on both host backends. This section fixes how the Linux host
reaches it; nothing here is visible to a program except through time.

**Backend.** A socket uses its owning slot's poller ([NET-7](#net-7-owned-connections) and
`HIRSEL_NET_REACTOR`): io_uring on Linux 6.13 and later where the ring opens, epoll
otherwise. That is today's D5 default for every Net socket (Sam's Ruling 5, 2026-11-24:
io_uring stays the `auto` default on 6.13 and later until the unified net and disk ring is
measured), and UDP follows it with no selection of its own.

**io_uring receive.** The socket registers its pool as one provided-buffer ring
(`IORING_REGISTER_PBUF_RING`, ring entries a power of two below 65,536, `kbuf.c:634-637`) on
its slot's ring, under a buffer group of its own, and arms one multishot `IORING_OP_RECVMSG`
(`IORING_RECV_MULTISHOT` in the SQE's `ioprio`) with `MSG_TRUNC` in its message flags. Each
completion fills one slot with an `io_uring_recvmsg_out` header, the source address, the
`UDP_GRO` and TOS/TCLASS control messages and the payload. With `MSG_TRUNC` the header's
`payloadlen` is the datagram's whole length even when the slot cut it (`io_uring/net.c:987-989`,
`udp.c:2162-2163`), which is the `len` NET-16 delivers. The buffer ring is not incremental
(`IOU_PBUF_RING_INC`): an incremental buffer cannot back a multishot `recvmsg`, because its
leftover tail can be shorter than the header (`moq-uring` `udp.rs:5-10`). The pool is
per socket, not shared per slot, so its charge has one payer and a socket moves without
touching another socket's buffers.

When every slot is full the kernel ends the multishot receive with `ENOBUFS`. Datagrams then
wait in the socket's kernel queue, not lost by the host, and the host re-arms after a
`udp_recv` returns slots. A multishot receive posts at most `slots` completions before it
stops, so a slot ring's completion queue is sized for the sum of its armed sockets' `slots`
plus its other armed operations; the host still relies on `IORING_FEAT_NODROP` for a burst
past that, and handles overflow as the slot ring already does.

**io_uring send.** `udp_send` issues one `IORING_OP_SENDMSG` per GSO train or single datagram,
with `UDP_SEGMENT`, `IP_TOS`/`IPV6_TCLASS` as control messages on each message, all linked
(`IOSQE_IO_LINK`) and submitted in one `io_uring_enter`. A failure cancels the rest of the
chain, which gives NET-16's prefix commit. The kernel copies a datagram's bytes during the
send, so the caller's borrow ends at the terminal completion; an abort first cancels the chain
and waits for its terminal completions, as NET-7 takes back a held send. `SEND_ZC` is not
used for UDP.

**epoll receive and send.** On epoll the same pool is the iovec array of one `recvmmsg` per
readiness turn, with the same control messages; and a send is one `sendmmsg` whose messages
are the same trains with the same `UDP_SEGMENT` control messages. `sendmmsg` returns the
count of messages sent, a prefix, which gives NET-16's commit the same way.

**GSO refusal.** The kernel refuses a GSO send with `EIO` when the route uses IPsec
transformation (`udp.c:1154-1157`), and with `EINVAL` past 128 segments
(`UDP_MAX_SEGMENTS`, `include/linux/udp.h:133`, `udp.c:1146-1149`); some drivers also
refuse it. A refused train is one skb, so nothing of it was sent; the host resends it as
single datagrams and sends that socket's later packets without GSO, as `quinn-udp` does
(`quinn-udp/src/unix.rs:553-566`). The host's train limit of 64 segments stays under the
kernel's 128.

**Teardown.** Before a socket's descriptor closes, its buffer group is unregistered, or its
slot ring exits, the host cancels the armed multishot receive and any send chain
(`IORING_OP_ASYNC_CANCEL`) and waits for their terminal completions (a completion without
`IORING_CQE_F_MORE`). A move between units does the same on the old ring, carries any filled
slot not yet delivered, re-registers the pool on the new slot's ring and re-arms there.

**Target.** The build lane measures on Linux 7.0 loopback with 1,200-byte payloads: one unit
on one pinned core receiving, counted as datagrams returned by `udp_recv`, and one unit on
another pinned core sending 64-packet trains to one peer, counted as packets committed by
`udp_send`. Each side must reach at least 1.0 million datagrams per second. The matched peer
is `quinn-udp` (`recvmmsg` with `UDP_GRO`, `sendmsg` with `UDP_SEGMENT`) on the same CPUs,
kernel and packet size, run from a minimal Rust loop with the same batch size, reporting
instructions and cycles per datagram for both arms.
