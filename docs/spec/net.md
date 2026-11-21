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
`NetError = Denied | Refused | Unreachable | InUse | Reset | TimedOut | Aborted | Other(u32) | Tls(TlsFailure)`.
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
