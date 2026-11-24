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

Version note (2026-11-24, media-udp-amend-2225): closes the 13 findings of the
media-udp-attack-2219 review. A group is one exclusively bound socket that the host
dispatches, with no `SO_REUSEPORT`. Charges are refunded at a recorded retirement and
not at the drop. An opened socket receives only from sources its `UdpSend` rules cover.
Sends run in bounded windows, and an empty datagram ends a GSO train. Served receives stage
packet records apart from payload bytes. Each ring holds a completion reservation per
socket, and both calls take byte steps. `NetError` gains `Exhausted`.

Version note (2026-11-24, media-udp-amend2-2245): closes the ten findings of the
media-udp-recheck-2240 review. Both backends now share one datapath: `recvmmsg` and
`sendmmsg` on the owner's poller, with io_uring giving only readiness. No datagram and no
retirement waits on a completion. A helper's join keeps every unretired socket's charge
held. A sealed port retires its sockets. A member queue has one fixed owner, the intake. GRO
records split by a checked stride, and a cut record loses whole datagrams only. UDP host work
on a poller shares one budget per turn. A send window counts iovecs as well as messages. The
host allowance is a nominal per-run admission. A group's throughput is qualified apart from
the standalone target.

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

- `UdpBind` permits binding a socket to a local endpoint, which then receives from any
  source ([NET-16](#net-16-udp-sockets-and-batches)). It follows `Listen`'s rules exactly: a
  wildcard bind needs a `/0` rule of the same family, and a bind to port 0 needs the
  matching `UdpBind` rules to cover every nonzero port (`1-65535`) or it returns
  `EphemeralOutsideNetGrant` before issue.
- `UdpSend` permits sending to a destination endpoint. Every datagram's destination is
  checked, in every batch. The unspecified address, IPv4-mapped IPv6, unscoped
  `fe80::/10` and the IPv4 limited broadcast `255.255.255.255` are denied as destinations.
  The host never sets `SO_BROADCAST` and joins no multicast group, so a directed broadcast
  the kernel refuses is `Denied` after issue. On a socket that was opened and not bound,
  `UdpSend` also fixes the sources that the socket receives from (below).

A run grants them with the Net rule list:

```sh
hirsel run sfu --grant 'Net=UdpBind 0.0.0.0/0 3478-3478;UdpSend 0.0.0.0/0 1-65535'
```

**Receiving grants no reply authority.** A datagram's source address is unauthenticated: an
attacker can write any address there. If receiving from a peer let a program answer it, a
spoofed source would make the program send to a host the operator never named. That is the
reflection that QUIC (RFC 9000 §8) and STUN spend their address-validation machinery on.
So a reply needs a `UdpSend` rule like any other send. A server that answers the whole
Internet holds `UdpSend 0.0.0.0/0 1-65535` (and its IPv6 twin), which says so in the grant.
A TCP peer is different because its handshake proved the address, and NET-6 lets accepted
peers inherit the listener's authority for that reason. A narrower reply right needs a
trusted transport provider that has validated the return path, and is not part of W0.1.

**Ingress of an opened socket.** A socket made by `udp_open` holds no `UdpBind` rule, so it
has no authority to receive from the whole network. The host delivers to it only datagrams
whose source endpoint a `UdpSend` rule of its originating Net covers. It discards every
other datagram before it reaches a slot. So an opened socket hears only from endpoints its
program may already send to, and it holds no state about which peers it contacted. A
program that wants general ingress binds with a `UdpBind` rule. A client granted
`UdpSend 192.0.2.53/32 53-53` therefore receives DNS answers from `192.0.2.53:53` and
nothing else, even on the ephemeral port the kernel chose.

**Rule lookup.** The provider indexes a Net's rules of each UDP direction and family once,
when it makes the Net. For each prefix length present it keeps a hash set of prefixes, and
for each prefix its port intervals sorted and merged. A lookup probes at most 33 (IPv4) or
129 (IPv6) prefix lengths, with one binary search over intervals at each. Its cost is
bounded by the number of prefix lengths and the logarithm of the rules per prefix, not by
the number of rules. The send preflight and the ingress filter use this lookup.

Law: a UDP operation that designates an endpoint succeeds only under a matching rule of the
same direction in its originating Net, and the check is made before issue with no host
effect. An opened socket delivers a datagram only from a source that a `UdpSend` rule of its
Net covers. `net.narrow` keeps a UDP rule exactly as NET-6 keeps an address rule. The host,
a replay and a served Net make the same checks.

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

`NetError` gains two variants:

- `Oversize(max: u32)` (below).
- `Exhausted`, a host resource the provider could not obtain after issue: the UDP host
  allowance, a descriptor or a socket. It is recorded like any delivered error, and
  nothing of the operation remains.

`UdpSocket` is a mechanical resource of logical size 8. It is `send` and `cross`
([CONC-2](tasks.md#conc-2-the-send-bound)): like `Socket` and `Listener`, it may move to
another unit, so one unit per core can own its socket. It is move-only and never
`shareable`. Its authority is the Net it was made from:

- `udp_open` makes a socket of one family that the kernel binds to an ephemeral port at its
  first send, as a TCP connect takes an ephemeral local port with no `Listen` rule. It needs
  no `UdpBind` rule. An IPv6 socket is `IPV6_V6ONLY`. The socket becomes **receiving** when
  a `udp_send` on it returns `Ok(k)` with `k ≥ 1`. Before that, `udp_recv` and
  `udp_local_addr` return `Denied` before issue, even after a send that failed or committed
  nothing (Linux may already have bound a port, `af_inet.c:850-852`). It then receives under
  the ingress rule of NET-15.
- `udp_bind` needs a `UdpBind` rule (NET-15). It returns the socket, which is receiving at
  once, and its bound endpoint, with the kernel's port when `a.port` is 0. An IPv6 bind is
  `IPV6_V6ONLY`, so a v6 socket never receives or sends IPv4.
- `udp_bind_group` binds one kernel socket to `a` and returns `members` (2 to 65,535)
  **member** handles of it, appended to `out` in member order. `out`'s spare capacity must
  hold `members`, or the call returns `Denied` before issue. The host reads every datagram
  that reaches the socket and dispatches it to one member chosen by `steer` (below). Each
  member is a `UdpSocket` that receives only its own datagrams, sends from the group's
  endpoint and moves between units on its own.
- `udp_send` checks every packet's `to` against `UdpSend` (NET-15). A packet whose `to` is of
  the other family than the socket's returns `Denied` before issue with nothing sent.

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
packet over its family's maximum returns `Oversize(max)` before issue with nothing sent. This
static maximum is the only size checked before issue. The host sets
`IP_MTU_DISCOVER`/`IPV6_MTU_DISCOVER` to `PMTUDISC_PROBE`, so the sender never fragments
a datagram. In that mode the kernel compares a send with the outgoing device's MTU, not the
path's (`ip.h:455-458`, `ip_output.c:1302-1303`). A packet the kernel refuses for its size
at the send (`EMSGSIZE`) is `Oversize(0)`. A packet the kernel accepts may still exceed a
smaller link further along the path and be dropped there. A committed packet therefore
proves nothing about the path MTU: a program learns it from its protocol's
acknowledgments or losses, as RFC 8899 probes do. An empty datagram is a datagram.

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
   before it reaches `buf`, and `len` is still its whole length.
3. Each later datagram is delivered only whole: a datagram that does not fit `buf`'s
   remaining spare capacity, would need an entry beyond `out`'s spare capacity, or was cut by
   its slot, stays queued for the next receive, where it is first. So batching never
   truncates a datagram and never loses one.
4. A batch ends when the queued datagrams run out, `out` is full or the next datagram does not
   fit. It never waits for more after the first.

`udp_recv` returns the number of datagrams delivered and admits nothing: the caller's spare
capacities bound the batch, as `host.resolve` does (NET-11), and a receive writes only into
`out`'s spare entries. Its checks run in this order. A socket that is not receiving returns
`Denied` before issue. Otherwise, with `out` spare capacity 0, it returns `Ok(0)` at once,
takes nothing from the queue and does not wait. Every other receive delivers at least one
datagram or returns an error.

The kernel may coalesce datagrams with `UDP_GRO`. The host delivers such a coalesce as its
individual datagrams. A **record** is one message the host takes from the kernel, of aggregate
length `L` (the message's whole length, `MSG_TRUNC`). The host splits it this way:

- A record without a `UDP_GRO` control message is one datagram of length `L`, including when
  `L` is 0.
- A record with a complete `UDP_GRO` control message (a native `int`, `udp.h:164-172`) whose
  stride `S` is positive is the datagrams that start at offsets `0, S, 2S, …` below `L`, each
  of length `min(S, L − offset)`. At stride 1,200, 2,417 bytes are datagrams of 1,200, 1,200
  and 17 bytes, and exactly 2,400 bytes are two of 1,200.
- A record whose control data the kernel cut (`MSG_CTRUNC`) or whose `UDP_GRO` stride is not
  positive has no boundaries the host can establish. It is a named discard (Order and loss).
  The host's control space holds every control message it enables, so Linux 7.0 never cuts
  it.

Every datagram of one record gets the record's source and ECN bits. Linux merges only
segments of one flow whose traffic class matches, and ends a coalesce after a shorter
segment (`gro.h:452`, `gro.h:466-472`, `udp_offload.c:745-778`). A record holds up to
128 datagrams: a local `UDP_SEGMENT` send can reach a GRO socket unsegmented, since
`UDP_GRO` sets `ACCEPT_L4` (`udp.c:3085`, `udp.c:2517`, `UDP_MAX_SEGMENTS`). A record that a
batch delivered only in part stays in its slot with a cursor at its next datagram. The rest is
delivered from there, with no copy beyond the copy to `buf`. There is no receive timestamp,
no destination address and no interface index in
W0.1. The ECN field is the one control datum a QUIC sender needs to validate ECN
(RFC 9000 §13.4.2); the others have no consumer yet.

**Sending.** `udp_send` sends `pkts` in order: packet `i` is the bytes
`data[at, at + len)` to `to` with ECN bits `ecn` (`IP_TOS`/`IPV6_TCLASS`). Each `at + len`
must be at most `data`'s length and each `ecn` at most 3, or the call returns `Denied`
before issue with nothing sent. The whole list is validated before anything is issued:
every bound, every size, every family and every `to` against NET-15. One failing packet
refuses the whole call with nothing sent. The call returns `k`, the number of packets
committed: packets `0..k` were each handed to the kernel exactly once as one datagram, and
none after them was. Each packet commits whole or not at all; there is no partial datagram,
and an empty packet that commits is one datagram of zero bytes on every backend.

The call returns an error only when it committed nothing. When it commits `k ≥ 1` packets
and then stops, it returns `Ok(k)`, and the reason it stopped is not reported. The stop
might be a kernel refusal of packet `k`, the deadline or a cancellation between windows.
The kernel can discard a refusal after a partial batch (`sendmmsg(2)`, BUGS;
`socket.c:2752-2753`), so the call does not promise one. A later call that resends from
packet `k` reports that send's own outcome, which may be a success. The host never issues
a committed packet again. The call waits while the socket's send buffer is full and keeps
one absolute deadline through readiness retries. At the deadline it returns the `k`
committed so far, or `TimedOut` if none.

**GSO trains.** A program sends a GSO train by listing equal-length packets to one peer one
after another. The host groups each run of consecutive nonempty packets with the same `to`
and `ecn` into one `UDP_SEGMENT` send. Every packet of a run has one length except the last,
which may be shorter and nonempty. A run holds up to 64 packets and at most 65,507 bytes in
all. An empty packet is never part of a train: it ends the run before it and is issued alone.
A train the kernel accepts is one kernel message and commits all its packets together, so it
adds all of them to `k` at once. A train the kernel refuses commits nothing, and when the
host resends it as single datagrams (NET-17, GSO refusal) those commit one at a time and can
stop partway like any other packets. The grouping changes the number of kernel crossings and
never which bytes reach the wire or their order. It may change where a send stops: within
one execution `k` obeys the prefix law below, but two executions of the same list need not
stop at the same `k`.

**Order and loss.** UDP may drop, duplicate and reorder datagrams on the network, and the
kernel drops datagrams when a socket's receive queue is full. The host adds none of these,
with exactly these named exceptions, which `std.sim.udp` models in the same way:

- an opened socket discards a datagram from a source outside its `UdpSend` rules (NET-15);
- a group discards a datagram steered to a member that is dropped, or whose queue is full
  (below);
- a `UDP_GRO` record that its slot cut (its length `L` above the captured bytes) loses every
  datagram not wholly captured. The host delivers the datagrams that end within the captured
  bytes and discards the rest, including a partly captured one, so no datagram of a cut
  record is ever delivered truncated. With 65,536 captured bytes, a stride of 1,200 and
  `L` above 65,536, it delivers 54 datagrams. Linux 7.0 keeps a UDP coalesce below 65,536
  bytes (`gro.c:116-119`, `gro.c:226`) and a local GSO send below the family maximum, both of
  which a 65,536-byte payload room holds, so this happens only if a kernel breaks that bound;
- a record whose boundaries cannot be established (above) is discarded whole.

Apart from these, every datagram the host takes from the kernel is delivered once, in the
kernel's order. The host never drops one to make room.

**Groups and steering.** A group is one kernel socket bound without `SO_REUSEPORT`, so no
other socket, of this run or of any process of the same user, can share its endpoint (the
kernel admits a same-user socket only when both set the option, `udp.c:157-159`). The host
owns all dispatch. Its **intake**, the group's own pool of `slots` slots and the kernel
socket, lives on the poller of the slot where `udp_bind_group` ran, for the group's whole
life. The intake splits every record into its datagrams first and steers each one alone, so
two connections on one address and port tuple can reach two members. It copies each datagram
into its member's queue, a ring of `slots` entries of `slot_bytes` each.

**Member queues.** A member's queue has one owner for the group's whole life: the intake. It
is a single-producer, single-consumer ring in host memory. The intake is its only writer, and
the unit holding the member handle, wherever it runs, is its only reader. Moving a member
moves only the right to read. The queue, its entries and its capacity stay where they are,
so a move hands over no datagrams and needs no ordering between owners. The intake takes a
queue entry before it copies, and when the queue is full it discards that datagram and goes
on. A full member queue therefore holds a bounded number of datagrams, and a datagram for a
full member never takes another member's entry.

Notifications carry no datagrams. A member records which poller its current reader waits
on. After a turn that made a member's queue nonempty while its reader was waiting, the intake
posts one wake to that poller (several datagrams in one turn give one wake). A wake to a
reader that has since moved is harmless. The new reader checks its queue when it attaches and
before every wait. A wake the destination ring refuses (`EOVERFLOW` from `MSG_RING`,
`msg_ring.c:136-141`) stays pending at the intake, which retries it at its next turn.

`steer` picks the member for one datagram, from the datagram and its source alone. Every
provider, the host and `std.sim.udp` compute it with the same function:

- `Hash` is `fnv1a64(key) mod members`, where `key` is the family tag (`04` or `06`), the
  source address in network order (4 or 16 bytes) and the source port (2 bytes,
  big-endian), and `fnv1a64` is 64-bit FNV-1a (offset `0xcbf29ce484222325`, prime
  `0x100000001b3`, each byte XORed then multiplied, modulo 2^64). One source endpoint
  always reaches one member. Shared vectors:
  - `192.0.2.1:443` has key `04c000020101bb` and hash `0x8a708e0d333ba182`, so member 2 of 8;
  - `198.51.100.7:50000` has key `04c6336407c350` and hash `0x5b30a0307882ad36`, so member
    2 of 3;
  - `[2001:db8::1]:4433` has key `0620010db80000000000000000000000011151` and hash
    `0x313cf93d25cae0e6`, so member 230 of 1,024.
- `Quic` reads only QUIC's version-independent header (RFC 8999 §5), so it treats v1, v2
  and every other version alike. A server using it issues every connection ID in this
  format: one first octet the server chooses (QUIC-LB keeps it for config rotation), then
  the owning member's index as a big-endian `u16`, then a nonce. It is at least 3 bytes,
  and 8 or more is recommended. A **member field** is that `u16` when it is below `members`.
  - A long-header datagram (`byte0 & 0x80`) has DCID length `d` at offset 5 and the DCID at
    offsets `6..6+d`. When the datagram is shorter than 6 bytes or than `6 + d`, the datagram
    steers by `Hash`. When `d ≥ 3` and bytes 7 and 8 form a member field, it goes to that
    member. Otherwise, when `d ≥ 1`, it goes to member `fnv1a64(DCID) mod members`, and when
    `d = 0` it steers by `Hash`.
  - A short-header datagram has its DCID at offset 1 and carries no length for it. When it is
    at least 4 bytes and bytes 2 and 3 form a member field, it goes to that member; otherwise
    it steers by `Hash`.
  - An empty datagram steers by `Hash`.

  So a client's first Initial, its retransmissions and its 0-RTT packets share one DCID and
  reach one member. An Initial after a Retry, and every Handshake and short-header packet,
  carries a DCID the server issued and reaches the member it names. A Retry token or a
  Handshake that names a member reaches that member whatever the source address. A
  client-chosen DCID whose bytes 1 and 2 happen to form a member field also goes to that
  member, which is still a function of the DCID. A shared vector: the long-header DCID
  `8394c8f03e515708` (its member field `94c8` is out of range for 8 members) hashes to
  `0xa3c8f732fcd9bf08`, so member 0 of 8.

Steering gives affinity only. A sender chooses its source address (spoofed) and its DCID
bytes, so it can aim every datagram at one member. Steering promises no fairness and
authenticates no peer. A member queue bounds only the datagrams resident for its member.
Every member still shares the kernel receive queue, the intake's core and the poller's turn
budget, so a flood aimed at one member does slow the others. They lose only what the kernel
or their own full queues drop.

A member index is fixed for the group's life. Dropping a member retires its queue (below).
From then on, the datagrams steered to it are discarded until the last member is dropped and
the kernel socket closes. A program that stops serving a member keeps it, or rebinds the group.

**Endpoint query.** `udp_local_addr` returns the bound endpoint, which for a member is the
group's. An opened socket that is not yet receiving gets `Denied`. The query borrows, admits
nothing and waits only as NET-1's queries do.

**The receive pool.** Each socket owns a receive pool of `slots` slots of `slot_bytes` each.
The host receives into a slot and copies the payload to the caller's `buf` at delivery, so a
slot is never lent to program memory. A slot returns to the pool once every datagram in it has
been delivered.
The first 256 bytes of each slot are reserved for the host's per-record header (the source
address, the control messages, and the record's length and cursor, NET-17); the rest is the
slot's payload room. When `slot_bytes` is 65,792 the payload room is 65,536. That is more
than any coalesce Linux 7.0 builds (see Order and loss), and the host enables `UDP_GRO`. With
any smaller value it does not, because the kernel truncates a coalesced datagram to the buffer
it is given and the datagrams after the cut would be lost (`udp.c:2093-2098`). This is the
rule that `quinn-udp` states for its receive buffers (`quinn-udp/src/unix.rs:133-141`), made
a program-visible choice: a socket that many small flows reach wants many small slots; one
that a few fast flows reach wants few large ones. A group's intake pool and each member's
queue have the same `slots` and `slot_bytes`.

**Charges.** The pool is run memory that the program sized, so it is charged, unlike other
host memory behind a provider ([MEM-19](budgets.md#mem-19-limits)). The host charges these
amounts, each computed in exact arithmetic (a product above `2^64 − 1` is refused as
MEM-22 says):

- `udp_open` and `udp_bind` admit `slots × slot_bytes` bytes.
- `udp_bind_group` admits `(members + 1) × slots × slot_bytes`: the intake pool and one queue
  per member.

Each admission happens before issue, against the limit current at the call, together with
each result handle, in one admission. A refused admission raises `Capacity` with no host
effect. The charge is the same on io_uring, on epoll, on a replay and against a served Net,
because it is computed from the options and `members` alone
([CAP-5](capabilities.md#cap-5-provider-operations)). A group whose host creation fails
after admission, partly or wholly, closes what it made. It then returns the whole charge
once, after that is retired, and delivers the error. No member handle exists for a group that
failed. The pool's record keeps its payer when the socket moves to another task or unit
([MEM-20](budgets.md#mem-20-limit-records-and-hand-over)).

The charge outlives its payer the way every MEM-20 charge does. When the paying limit ends,
its record joins its parent's, and the parent keeps holding the socket's bytes as part of the
reservation it gave. A helper's join is such an end. CONC-15 lists the socket's charge among
the survivors that the join does not refund, whatever route the handle left by (the
helper's result, a connection, or a drop whose retirement is still pending at the join). With
nested helpers the record leads on, join by join, to the nearest limit still live. Only the
retirement refunds the charge, to the limit the record leads to at that moment.

**Retirement.** A dropped socket's charge stays held until its host backing can be freed.
The pool, its kernel registration and its descriptor are torn down first. That point is a
**retirement**, and only then does the charge return as a refund to the limit its record
leads to (MEM-20). Dropping cannot wait, so the drop starts retirement and returns, and
retirement completes later as an input event of the **refunded unit**: the unit of the live
limit that the record leads to when the retirement completes
([CONC-10](tasks.md#conc-10-event-order), [CONC-15](tasks.md#conc-15-units)). That unit
records it, as it records any cross-unit refund. It is recorded at its position as
`Retired(ordinal)`, so an admission
on replay sees exactly the refunds the recorded run saw. A later open whose admission comes
before that event sees the old charge still held and can be refused. A pool's credit
therefore never funds a new pool while the old one's storage is still live.

- **Retiring a socket** removes its readiness watch, closes its descriptor and frees its
  pool (NET-17, Teardown). No step waits for a completion. The kernel holds no pointer into
  the pool between calls, since both backends receive by `recvmmsg` into it. Datagrams still
  queued in the kernel or the pool are discarded, and there is no linger.
- **Retiring a member** stops the intake's dispatch to it and frees its queue at the intake's
  next turn. The intake pool retires with the kernel socket, after the last member's
  retirement.
- **Retiring on a served Net** happens when the server takes the socket's `Released` call.
  A sealed port takes no more calls, so the seal retires every socket id of the port that has
  been dropped and whose `Released` the server had not taken, all at the seal. A socket
  dropped after the seal retires at its drop. Either way the refunded unit records
  `Retired(ordinal)` as an input event, ordered after the seal. A replay supplies the
  recorded position.

A run that ends first discards its retirement events with the rest of its state.

**Host allowance.** The kernel's socket queues use host memory up to the host's defaults
(`net.core.rmem_default` and `wmem_default`), which no program limit pays for. The host
therefore keeps one **UDP host allowance** per run, read once at run start from the host
setting `HIRSEL_UDP_KERNEL_BYTES` (a byte count; unset means 64 MiB, and a value that is not
a byte count refuses the run), as `HIRSEL_NET_REACTOR` chooses the backend. A socket reserves
`rmem_default + wmem_default`, read once at run start, from the allowance from its creation
until its retirement. A group reserves that amount once, for its one kernel socket. A
reservation the allowance cannot meet, or a descriptor or socket the kernel refuses, returns
`Exhausted` after issue with nothing created, and the refusal is recorded. This is host
backpressure, kept out of the logical admission of MEM-21: a served Net has no allowance and
never returns `Exhausted` for it, so a refusal is a host input like any other delivered error.

The allowance is a **nominal per-run admission**, not a ceiling on kernel memory. It counts the
sizes a socket starts with, as read at run start. The kernel sets them at socket creation from
the defaults current then (`sock.c:3704-3705`), and an operator can change those defaults
after run start. The kernel can also queue one datagram beyond a socket's receive limit when
its queue is empty (`udp.c:1727-1730`). Each run keeps its own allowance, so several runs on
one host do not share a bound. What it promises is that one run opens no more sockets than its
allowance names at their nominal size. A host-wide physical ceiling is the operator's, set
through the kernel's own `net.ipv4.udp_mem` or a cgroup memory limit.

**Steps.** Each operation is one call step ([MEM-30](budgets.md#mem-30-step-bounds)), and a
UDP batch takes further steps, all taken from the call's arguments before issue:

- `udp_recv` takes one **entry step** for each 64 entries, or part of 64, of `out`'s spare
  capacity. It also takes one **byte step** for each 16 KiB, or part of 16 KiB, of
  `min(r, e × (slot_bytes − 256))`, where `r` is `buf`'s spare capacity and `e` is `out`'s
  spare entries. That is the most it can deliver.
- `udp_send` takes one entry step for each 64 packets, or part of 64, of `pkts`. It then
  validates the list (the entry steps pay for this, since each packet costs a bounded rule
  lookup, NET-15). Then it takes one byte step for each 16 KiB, or part of 16 KiB, of the sum
  of the packets' `len`. Overlapping slices count once per packet, and the sum is the same
  whatever commits or is retried.

The products and sums are exact: `e × (slot_bytes − 256)` and the sum of `len` are computed
in 128-bit arithmetic before the minimum and the rounding, so neither wraps. The rounded step
counts are taken as MEM-30 takes a bulk charge, with the same slice ends.

Steps never depend on how many datagrams arrive or commit, so a bound stops a run at the same
step on the host, on a replay and against a served Net. The byte steps bound copying as
`crc32c.update`'s 16 KiB chunks bound hashing. The host's own work outside calls takes no
program steps: an intake's reading and dispatch, an opened socket's ingress filter and the
discards. The poller's turn budget bounds it instead (NET-17).

**Deadlines and cancellation.** Each operation keeps one absolute deadline through readiness
retries and is a cancellation point at issue and while it waits
([CONC-7](tasks.md#conc-7-cancellation-points)). A receive or send whose host call has
committed delivers its result even when cancellation arrives after it. A cancellation or a
deadline can stop a send only between two `sendmmsg` windows (NET-17), never inside one kernel
message. An accepted train is therefore never split, while a refused train resent as single
datagrams can stop between them. A cancelled
send returns `Aborted` only when it committed nothing; otherwise it returns the `k` committed.

**Dropping.** Dropping a socket starts its retirement (above). Dropping a member also ends
dispatch to it. It leaves the intake and every other member's queue as they were. The
group's kernel socket closes at the retirement of its last member.

Laws:

- On a receiving socket with `out` spare capacity `e ≥ 1`, a receive delivers at least one
  datagram, and the first one with room `r` appends exactly `min(r, room, len)` bytes. Here
  `room` is the slot's payload room, and `truncated` equals `len > min(r, room)`. Every later
  datagram of the batch is whole, and `at` of each entry is the length of `buf` before its
  payload. With `e = 0` the receive returns `Ok(0)` and takes nothing. Across successive
  receives, the datagrams delivered are exactly what the host took from the kernel for this
  socket or member, in order, less only the named exceptions of Order and loss. Only a
  datagram that was a whole record can be truncated. A datagram split from a `UDP_GRO` record
  is either delivered whole or discarded under the cut-record exception.
- A send returning `k` committed packets `0..k`, each once and whole, and none after them.
  This holds within each execution, whatever the host's GSO grouping, its windows or its
  backend, and when the kernel refused a GSO train that the host resent as single datagrams.
  Two executions may return different `k`. An empty packet commits as one datagram.
- The charge admitted at open, bind or group creation equals the refund at retirement. It
  depends only on the options and `members`. No refund comes before the retirement of the
  storage it paid for.
- A member's index never changes while the group lives. Every datagram the intake takes goes
  to the member `steer` names, or is discarded under a named exception. No socket outside the
  group receives a datagram sent to the group's endpoint.

**Moving between units.** A standalone socket (from `udp_open` or `udp_bind`) moved to
another unit keeps its descriptor, its bound endpoint, its pool and its pool's payer. The
host moves with it every record it took from the kernel but had not delivered, including the
unread rest of a partly delivered record with its cursor. These are delivered first on the
new owner, before anything the new poller reads. So a move never loses, repeats or reorders
a datagram. A member moves differently (Member queues): it keeps its group index and its
pool's payer, while its queue, the intake and the kernel socket stay with the intake. Only
the right to read the queue moves.

Law: for any interleaving of moves and receives, the datagrams delivered across all owners
are, in order, the datagrams of the host's own dequeue trace for that socket or member, less
the named discards of Order and loss. For a standalone socket the trace lists each datagram
the host kept after splitting the records it took from the kernel and filtering them. For a
member it lists each datagram the
intake wrote into the member's queue. The ingress filter, a full queue, a dropped member and
a cut record discard before that point, so they never appear in the trace. The harness is the
existing socket-transfer tests (`crates/hirsel-core-net/src/reactor/tests.rs`). Its oracle is
that trace, not the sender's sequence: a loopback kernel may drop on a full queue, and only
the trace tells that drop from a transfer error.

**Replay.** The grant snapshot holds each UDP rule's direction, prefix and ports. An issue
holds the Net or socket ordinal, the endpoint or family, the options, `members` and `steer`,
the spare capacities of `buf` and `out`, the packets and their bytes for a send, and the
deadline. A delivery holds one of:

- the new ordinals and bound endpoint;
- the delivered datagrams (peer, length, truncation, ECN and appended bytes);
- `k`;
- the error.

A replay supplies the recorded datagrams and touches no socket. Which datagrams formed a
batch is part of the record, so a replay delivers the same batches. A retirement is recorded
as `Retired(ordinal)` at its position in the refunded unit's order. Dropping a host-issued
`UdpSocket` is the provider's `CLOSE` at its position.

Every creation delivery, recorded or served, passes one **creation validator** before any
handle exists. It checks the count, the ids and the endpoint against the operation:

- `udp_open` gets exactly one new ordinal. Its endpoint is the family's unspecified address
  with port 0, since the socket is not yet bound; the bound port becomes visible only through
  `udp_local_addr` once the socket is receiving.
- `udp_bind` gets exactly one new ordinal, and `udp_bind_group` exactly `members` ordinals in
  member order. The endpoint has the request's family and address. Its port equals the
  request's when that is nonzero, and is nonzero when the request's is 0.
- Every ordinal is nonzero and fresh. A served reply's ids `first` to `first + count − 1` must
  not wrap past `2^64 − 1`, and none may be live on the port.

Every receive delivery, recorded or
served, passes one validator before any buffer changes. It checks that the count is from 1
to the spare entries, and that the packet records satisfy the receive law for the recorded
`r` and slot room. It checks each `len` against the family maximum, each `ecn` against 3,
each peer's family against the socket's, and for an opened socket each peer against its
`UdpSend` rules. Finally, the appended bytes must total the sum of the records' appended
lengths. A send delivery passes when `k` is at most the number of packets. A delivery that
fails is `Corrupt`.

**Served UDP.** A `provide.net` Net permits every UDP endpoint through one internal rule of
each UDP direction, as served Unix does (NET-14); intersecting it with a concrete rule gives
the concrete rule. Its port receives these calls in addition to NET-14's:

```hirsel unbuilt
pub type UdpNetOp = UdpOpen(v6: bool, slots: u32, slot_bytes: u32) | UdpBind(addr: Addr, slots: u32, slot_bytes: u32, members: u16, steer: Steer) | UdpRecv(socket: u64, room: u64, entries: u64) | UdpSend(socket: u64, data: u64, pkts: u64) | UdpLocal(socket: u64)
pub type UdpNetReply = UdpSockets(first: u64, count: u16, addr: Addr) | UdpPackets(count: u64)
pub type UdpStaged = UdpStaged(peer: Addr, len: u64, appended: u64, ecn: u8)
pub fn fill_packets(mut s: Served[NetOp], mut c: Call[NetOp], take p: List[UdpStaged]) -> u64
```

These variants join `NetOp` and `NetReply` when NET-16 is built. `UdpSend` is answered with
the existing `Count(k)`, `UdpLocal` with `Address`, and any call with `Failed`. The build lane
fixes their payload field numbers with `provide.payload`, as NET-14's are fixed. A `UdpBind`
with `members` 1 is `udp_bind`. A served receive stages two things apart:

- **payload bytes** with `provide.fill`, at most `room`;
- **packet records** with `provide.fill_packets`, at most `entries`.

`provide.fill_packets` stages at most `entries` records and returns how many it took. Its
staging holds at most `entries` records of 48 bytes each, admitted from the server's current
limit at the fill and freed at the reply. The caller derives each `at` itself from the
running total of `appended`. So a receive with room 0 and one entry can still return the
peer, the whole length, the ECN bits and `truncated` (`len > appended`). A `UdpPackets(count)`
reply passes when `count` equals the staged records and they and the staged bytes pass the
validator above with `r` set to `room`. Otherwise the server fails with `Fail` before the
caller changes. A `Count` reply to `UdpSend` above its packet count fails it the same way. A
`UdpSockets(first, count, addr)` reply answers `UdpOpen` or `UdpBind` and passes the creation
validator above. `count` must be 1 for `UdpOpen` and equal the request's `members` for
`UdpBind`. The ids `first` to `first + count − 1` must be nonzero, must not wrap, and must
not be live, and the room must hold 32 bytes for each. For `UdpBind`, `addr` has the
request's family and address, and a port that equals a nonzero request port or is nonzero
for a request port of 0. For `UdpOpen`, which names no address, `addr` is the requested
family's unspecified address with port 0. A reply that fails any check fails the server with
`Fail`, as an id reply does under the port contract. The caller validates all of this and
reserves every id before it appends any handle, so a group is created whole or not at all. The caller has already checked the rules, the options and the
handle state before queueing the call, and has admitted the pool. A simulated network
(`std.sim.udp`, Wave 0.3) is a server over exactly these calls. It decides loss, delay,
reordering, duplication and address rebinding from its seed, applies the same `steer`
function and the same named discards, and `hirsel test --sim` stands in for the host
through it.

### NET-17: UDP datapaths

NET-16's behaviour is the same on both host backends. This section fixes how the Linux host
reaches it; nothing here is visible to a program except through time.

**Backend.** A socket uses its owning slot's poller ([NET-7](#net-7-owned-connections) and
`HIRSEL_NET_REACTOR`): io_uring on Linux 6.13 and later where the ring opens, epoll
otherwise. That is today's D5 default for every Net socket (Sam's Ruling 5, 2026-11-24:
io_uring stays the `auto` default on 6.13 and later until the unified net and disk ring is
measured), and UDP follows it with no selection of its own. A group's intake uses the poller
of its home slot (NET-16).

**One datapath.** Both backends move UDP datagrams with the same system calls on the owning
poller's thread: `recvmmsg` into a pool and `sendmmsg` from the caller's `data`. A backend only
tells the poller when a descriptor is ready, by an io_uring multishot poll as for every Net
socket (NET-7) or by epoll. So no completion queue entry ever carries a datagram, a send's
result or a retirement, and a UDP socket adds one readiness watch to its ring and nothing
more. There is no provided-buffer ring, no multishot `recvmsg` and no linked send chain.

**Lost readiness.** On io_uring a ring's completion queue is shared by every producer the
reactor has: readiness polls, mailbox wakes and TCP transmit completions. When the queue is
full, the kernel keeps an overflow entry only while it can allocate one, and otherwise drops
the completion (`io_uring.c:630-645`). For UDP a dropped completion can only be a readiness
edge, so it delays a read and loses no datagram. The kernel counts each drop in the ring's
`cq_overflow` word (`io_uring.c:641`), which user space maps at `cq_off.overflow`
(`io_uring.c:2896`). The poller reads that word every turn. When it has changed, the poller:

- marks every UDP source it owns runnable (below);
- removes and re-adds each of their readiness watches, since a multishot poll whose terminal
  completion was dropped has ended without notice.

The recovery needs no completion of its own. A runnable source is read by a nonblocking
`recvmmsg`, and `EAGAIN` simply makes it idle. A removal whose completion is itself dropped
loses nothing, since no UDP state waits for it. An epoll backend drops no readiness.

**Turn budget.** All UDP host work on one poller shares one budget per poller turn. A turn
handles at most 64 logical datagrams and copies at most 256 KiB of payload, whichever comes
first, and it always handles at least one datagram when one is pending. A **logical
datagram** is one datagram split from a record. It counts whether it is delivered, copied
into a member queue, or discarded by the ingress filter, a full or dropped member, or the
cut-record exception. The budget covers:

- an intake's reading and dispatch;
- the reading and filtering a waiting `udp_recv` does before it can deliver. The datagrams
  it delivers are paid for by the call's own steps (NET-16), and it delivers a whole batch
  once it starts.

Retirement drains nothing, so it is not on that list. The kernel's own work queueing
datagrams for a socket, which its softirq does, is not host work and not charged to a turn.

The budget is enforced before each system call. A read asks `recvmmsg` for at most as many
messages as there are free slots, logical datagrams left in the budget, and whole slot
payload rooms in the byte budget left, and at least one. A record can hold up to 128
datagrams (NET-16), so the budget can end inside a record. The record then keeps its slot
and its cursor, and dispatch resumes there in a later turn.

A source with work left when the budget ends stays **runnable**: a partly dispatched record,
a kernel queue that was not read to `EAGAIN`, or a `udp_recv` still waiting. The poller serves
runnable sources in rotation, so a busy intake cannot hold the head of every turn, and it
reaches them again with no fresh readiness edge. A poller does not block while it has a
runnable source, a pending member wake (NET-16, Member queues) or an overflow it has not
rescanned: its next wait has a zero timeout. Reaping completions that carry no UDP work is
bounded by the reactor's existing event array per turn.

**Receiving.** The pool is the iovec array of a `recvmmsg`, one slot per message. `MSG_TRUNC`
is among its input flags, so each message's length is its record's whole length even after
the slot cut it (`udp.c:2162-2163`). The first 256 bytes of each slot hold the host's
per-record header: the source address, the control space for the `UDP_GRO` and TOS/TCLASS
messages, and the record's length and cursor. The rest is payload room. A standalone
socket's waiting `udp_recv` reads on its owner's poller. An intake is read on its home
poller, by host work under the budget.

**Sending.** The host validates the whole list first (NET-16) and then sends it in
**windows**, in order, under one deadline. A message is one GSO train or one single datagram,
and an empty packet is always a single message. Each packet is one iovec over its slice of
`data`, so the packets of a train need not be contiguous in `data`, and they may overlap.
`UDP_SEGMENT` cuts the concatenation of a message's iovecs at the stride, and every packet of a
train but the last has exactly that length, so the cut gives back the packets in order. A
window is the longest run of next messages with at most 32 messages and at most 64 iovecs in
all. A train holds at most 64 packets, so every message fits in a window by itself. The host
keeps 32 message headers and 64 iovecs per socket for its windows, whatever the caller's list
length.

A window is one `sendmmsg` with `UDP_SEGMENT` and `IP_TOS`/`IPV6_TCLASS` control messages, on
both backends, below the kernel's window of 1,024 messages (`UIO_MAXIOV`, `socket.c:2704`).
It returns how many messages the kernel accepted, which is a prefix, and reports an error only
when the first message failed. It discards the error of any later message
(`socket.c:2752-2754`). `k` is the sum of the packets that the accepted messages carried. After
a short count the next window starts at the first unaccepted message, so the host learns that
message's refusal as the first error of the next window. On `EAGAIN` for a window's first
message the host waits for the socket to be writable under the deadline, then retries from the
same message. Any other error on a window's first message ends the call: GSO refusals are
handled as below, and otherwise the call returns `Ok(k)`, or the error when `k` is 0. The
kernel copies a datagram's bytes inside the call, so the caller's borrow of `data` ends when
`udp_send` returns. The next window is issued only after the previous one has returned. No
chain of submissions exists that a short enter could split. `SEND_ZC` is not used for UDP.

**GSO refusal.** The kernel refuses a whole train at once, since a train is one skb, so a
refused train sends nothing (`udp.c:1139-1159`). The host learns a refusal as the error of a
window's first message, and treats it this way:

- `EMSGSIZE` means a segment is larger than the device MTU (`udp.c:1143-1145`). A single
  datagram of that size would fail too, so the refusal is `Oversize(0)` for the train's first
  packet (NET-16), and GSO stays on.
- `EIO` means the route uses IPsec transformation (`udp.c:1155-1157`). The host resends the
  train as single datagrams in the following windows, and sends that socket's later packets
  without GSO, as `quinn-udp` does (`quinn-udp/src/unix.rs:553-566`). The singles commit one
  at a time (NET-16).
- `EINVAL` comes past 128 segments (`UDP_MAX_SEGMENTS`, `include/linux/udp.h:133`,
  `udp.c:1147-1149`), or from a driver. The host's limit of 64 segments stays under 128, and
  the host never sets `SO_NO_CHECK`, so `EINVAL` can only come from a driver. The host
  handles it as `EIO`.

**Teardown.** Retirement (NET-16) works the same way on both backends. The host removes the
socket's readiness watch, closes its descriptor and frees its pool, and then emits the
retirement event. None of these waits for a completion. On io_uring the removal is a
poll-remove submission, and a cancelled poll gives up its reference to the file through task
work (`io_uring/poll.c:380-385`). So the kernel socket and its bound endpoint can outlive the
close briefly. The pool does not depend on them. Retirement therefore does not promise that
the endpoint is free. A bind of the same fixed port just after a drop can get the kernel's
refusal, `InUse`, which is delivered as any bind error is.

A move of a standalone socket removes its watch on the old poller and adds it on the new one.
It carries every filled slot not yet delivered, and the cursor of a partly delivered record. A
member's move touches no descriptor and no ring (NET-16, Member queues).

**Target.** The build lane measures `udp_bind` sockets on Linux 7.0 loopback with
1,200-byte payloads. One unit on one pinned core receives, counted as datagrams returned by
`udp_recv`. One unit on another pinned core sends 64-packet logical batches to one peer,
counted as packets committed by `udp_send`. Each side must reach at least 1.0 million
datagrams per second. A 64 × 1,200-byte batch is 76,800 bytes, so it goes to the kernel as
two trains under the 65,507-byte limit (54 and 10 packets), which make one window (2 messages,
64 iovecs). The matched peer is `quinn-udp` (`recvmmsg` with `UDP_GRO`, `sendmsg` with
`UDP_SEGMENT`) on the same CPUs, kernel and packet size, run from a minimal Rust loop with the
same two-train split. Both arms report instructions and cycles per datagram.

The target applies to `udp_bind` only. A group adds steering, a copy into the member queue,
wakes across pollers and queue discards, so the standalone figure says nothing about it. The
build lane qualifies a group separately, with no target number:

- members both on the intake's poller and on other pollers;
- a stated offered load (packets per second, sizes and the number of source endpoints);
- delivered datagrams per member, and each named discard counted apart;
- cycles per datagram on the intake core and on the member cores, and the p99 and p99.9
  delay from intake read to member delivery.

A group's receive rate is bounded by its one intake core in W0.1. Any deployment envelope (for
example the number of 20 ms audio streams one endpoint carries) is quoted from that
measurement per endpoint, never derived from the standalone target.
