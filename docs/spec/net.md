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
instead ([TLS-3](tls.md#tls-3-authority)). In Hirsel, use
`Tls(names: String, ports: (u16, u16), trust: TlsPin)` with `SystemPin` or
`Sha256(Bytes)`. The latter holds the 32 raw digest bytes. It permits only a client handshake on a socket
that a `Connect` rule already allowed. An invalid prefix length, nonzero prefix host
bits, reversed ports or IPv4-mapped IPv6 prefix returns `Invalid` without changing
anything. Rule storage is admitted, so `RuleError` also includes
`Capacity(Capacity)`. The immutable result storage remains charged to its payer
until the run ends; plain copies of `Net` add no charge or refund.

A run grants Net with an explicit rule list:

```sh
hirsel run app --grant 'Net=Connect 127.0.0.0/8 1-65535;Listen 127.0.0.1/32 8000-9000'
```

The syntax is `--grant 'Net=<dir> <prefix>/<len> <lo>-<hi>[;<rule>]'`, where
`dir` is `Connect` or `Listen` and the prefix is an IPv4 or IPv6 literal. A `Tls` rule is
written `Tls <pattern>[,<pattern>] <lo>-<hi> <trust>`. Its trust is `System` or
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
poll registration are specified by NET-1 through NET-10 and implemented after W1a.


### NET-11: CLI recording and replay

A recorded run checks each LISTEN and CONNECT against its source Net rules.
It records a denied outcome without asking the source provider to perform
the operation. Replay supplies every socket result from the tape. Successful
LISTEN, ACCEPT and CONNECT produce run-local socket designations on replay.
The bound LISTEN address and ACCEPT peer address come from the tape.
Splitting or releasing a replay socket performs its local ownership change.

Tape format 4 stores a Run's root Net rules, in order and with duplicates,
in the header's grant snapshot. Each address rule occupies 23 bytes.
[TLS-6](tls.md#tls-6-tapes) defines the TLS rule record.
Replay starts with no live Net grant. It computes narrowed rule lists and
their Capacity charges from the snapshot and each parent's intersections.

The tape checks socket arguments at issue. It stores the delivered receive
bytes and appends them to the caller's buffer in the original fragment sizes.
Each send checks both offered buffers and the absolute deadline. The delivery
records how many bytes the source committed. [TLS-6](tls.md#tls-6-tapes) says what a TLS
session and a `Secret` add to a tape. It also says which tapes are credential-bearing. A final readiness attempt at a
deadline can deliver a successful receive, including its bytes.
