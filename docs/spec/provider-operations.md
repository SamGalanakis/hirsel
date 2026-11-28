# Provider operations

## Provide

`provide` makes capabilities of virtual construction
([CAP-2](capabilities.md#cap-2-capabilities-are-opaque)) and answers their operations in the
program's own code ([CAP-5](capabilities.md#cap-5-provider-operations)). Callers see nothing
new: a `Dir`, `Net`, `Exec`, `Stdout`, `Stderr`, `Clock`, `Random` or `Process` from `provide` has its
ordinary type and supports the same operations as a host grant.
A `provide.net` port also answers the Unix operations ([NET-14](net.md#net-14-unix-datagrams)).
`provide.exec` makes an `Exec` answered on a `Served[ExecOp]` port
([EXEC-8](exec.md#exec-8-exec-at-the-replay-point-and-served-exec)).

```hirsel core
pub opaque resource type Served[Op]
pub opaque resource type Call[Op]
pub opaque type Reply[Op]
pub opaque resource type ClockHand
pub opaque resource type StopTrigger
pub type Withdrawn = Withdrawn()
pub type Part = Whole | Read | Write
pub type DirOp = Open(dir: u64, path: u64, mode: OpenMode) | OpenDir(dir: u64, path: u64) | Mkdtemp(dir: u64, prefix: u64, room: u64) | Stat(dir: u64, path: u64) | Mkdir(dir: u64, path: u64) | Remove(dir: u64, path: u64) | RemoveTree(dir: u64, path: u64) | Rename(dir: u64, from: u64, to: u64) | SyncDir(dir: u64) | Entries(dir: u64, path: u64) | Read(file: u64, room: u64) | ReadAt(file: u64, at: u64, room: u64) | Write(file: u64, len: u64) | WriteAt(file: u64, at: u64, len: u64) | StatFile(file: u64) | Sync(file: u64) | TryLock(file: u64) | Unlock(file: u64) | Close(file: u64) | NextEntries(entries: u64, room: u64) | Released(id: u64) | Mkstemp(dir: u64, prefix: u64, room: u64) | ZeroExtension(dir: u64) | TryLockShared(file: u64) | ZeroAt(file: u64, at: u64, len: u64)
pub type DirReply = Done | Failed(IoError) | WriteFailed(WriteError) | Count(u64) | Locked(bool) | Stat(Stat) | File(u64) | Dir(u64) | Entries(u64) | ZeroExtensionSupported(bool)
pub type NetOp = Connect(addr: Addr, public: bool) | Resolve(name: u64, room: u64) | Listen(addr: Addr, backlog: u16) | Accept(listener: u64) | Recv(socket: u64, room: u64, max: u64) | Send(socket: u64, prefix: u64, tail: u64) | Handshake(socket: u64, name: u64, alpn: u64, system: bool) | SendSecret(socket: u64, wire: u64, secrets: u64, marks: u64) | Released(id: u64, part: Part) | ConnectUnix(root: u64, path: u64) | ListenUnix(root: u64, path: u64, backlog: u16) | AcceptUnix(listener: u64) | OpenDatagram | BindDatagram(root: u64, path: u64) | ConnectDatagram(datagram: u64, root: u64, path: u64) | SendDatagram(datagram: u64, len: u64) | SendTo(datagram: u64, root: u64, path: u64, len: u64) | RecvDatagram(datagram: u64, room: u64) | Ready(socket: u64) | LocalAddr(id: u64) | PeerAddr(socket: u64) | UdpOpen(v6: bool, slots: u32, slot_bytes: u32) | UdpBind(addr: Addr, slots: u32, slot_bytes: u32, members: u16, steer: Steer) | UdpRecv(socket: u64, room: u64, entries: u64) | UdpSend(socket: u64, data: u64, pkts: u64) | UdpLocal(socket: u64) | Hello(socket: u64, room: u64) | Serve(socket: u64, chain: u64, alpn: u64)
pub type NetReply = Done | Failed(NetError) | Count(u64) | Socket(u64, Addr) | Listener(u64, Addr) | Secured(Handshake) | UnixSocket(u64) | UnixListening(u64) | Datagram(u64) | Packet(len: u64, truncated: bool) | Address(Addr) | UdpSockets(first: u64, count: u16, addr: Addr) | UdpPackets(count: u64) | Accepted(protocol: Option[u64])
pub type StreamOp = Write(len: u64)
pub type StreamReply = Done | WriteFailed(WriteError)

pub fn dir(bytes n: u64) -> (Dir, Served[DirOp])
pub fn net(bytes n: u64) -> (Net, Served[NetOp])
pub fn stdout(bytes n: u64, terminal: Option[TermSize]) -> (Stdout, Served[StreamOp])
pub fn stderr(bytes n: u64, terminal: Option[TermSize]) -> (Stderr, Served[StreamOp])
pub fn next[Op](mut s: Served[Op]) -> Option[Call[Op]] waits
pub fn dir_op(c: Call[DirOp]) -> DirOp
pub fn net_op(c: Call[NetOp]) -> NetOp
pub fn stream_op(c: Call[StreamOp]) -> StreamOp
pub fn payload[Op](c: Call[Op], field: u64, mut out: Bytes)
pub fn payload_prefix[Op](c: Call[Op], field: u64, max: u64, mut out: Bytes) -> u64
pub fn fill[Op](mut s: Served[Op], mut c: Call[Op], b: Bytes) -> u64
pub fn secret(c: Call[NetOp], i: u64) -> Secret
pub fn reply[Op](mut s: Served[Op], take c: Call[Op], take r: Reply[Op]) -> Result[(), Withdrawn]
pub fn dir_reply(r: DirReply) -> Reply[DirOp]
pub fn net_reply(r: NetReply) -> Reply[NetOp]
pub fn stream_reply(r: StreamReply) -> Reply[StreamOp]
pub fn clock(start_mono_ns: u64, start_wall_ms: u64) -> (Clock, ClockHand)
pub fn random(seed: u64) -> Random
pub fn process() -> (Process, StopTrigger)
pub fn advance_to(mut h: ClockHand, mono_ns: u64)
pub fn advance_to_with_boot(mut h: ClockHand, mono_ns: u64, boot_ns: u64)
pub fn set_wall(mut h: ClockHand, wall_ms: u64)
pub fn idle(mut h: ClockHand) -> Option[u64] waits
pub fn trigger(take t: StopTrigger)
```

`advance_to`, `set_wall` and `idle` are in module `clock`, and `trigger` in module `process`.
- **Served ports.** `provide.dir`, `net`, `stdout` and `stderr` admit the port's room of `n`
  bytes, with a cell's 32, from the current limit and raise `Capacity` when it is refused;
  the room is held until that limit ends, when the port is withdrawn (Cell lifetime,
  below). Each operation on the capability waits after issue as a `Call`
  queued on the port, in the order the operations were issued. `provide.next` returns the
  first queued call, and `provide.dir_op`, `net_op` and `stream_op` read its `Op`, which holds
  only scalars: a path, name or data field is its byte length, which `provide.payload(c,
  field, mut out)` appends to `out` (fields numbered from 0 in declaration order;
  `Bounds` when `out`'s spare capacity is short). `provide.payload_prefix` appends only the
  first `max` bytes and returns the number of bytes omitted. It checks spare capacity
  against the bytes to append. A settled call supplies no bytes and returns zero.
  The `room` field is the spare capacity the caller offers for a result.
  `provide.fill` stages result bytes in the room, at most the
  call's `room`, and returns how many it took. No caller storage reaches the server, and
  no server storage reaches the caller until the reply.
- **Handles.** Ids name the server's own objects: 0 is the capability `provide` returned,
  and every other id is one the server issued in a reply (`File`, `Dir`, `Entries`,
  `Socket`, `Listener`). The caller receives a fresh handle of the ordinary type bound to
  the port, the kind and the id, so it reaches only that port's object of that kind, and a
  handle of another provider or another port given to an operation is refused before it
  reaches the server (`Denied`). A port's ids are one space, since `Released(id)` names no
  kind. Each issued id holds 32 bytes of the port's room until the server receives its
  `Released`; a `Dir` id, plain like the capability, holds them until the run ends. A
  `DirOp` is answered by `Failed` or by its own variants: `File` for `Open` and `Mkstemp`;
  `Dir` for `OpenDir` and `Mkdtemp`; `Stat` for `Stat` and `StatFile`; `Entries` for
  `Entries`; `Count` for `Read`, `ReadAt` and `NextEntries`; `Done` or `WriteFailed` for
  `Write` and `WriteAt`; `Locked` for `TryLock` and `TryLockShared`;
  `ZeroExtensionSupported` for `ZeroExtension`; and `Done` for every other operation, `ZeroAt`
  included. `Released` takes no reply. A reply
  that issues id 0, an id already live, or a variant that does not answer the call's
  operation fails the server with `Fail`, and a reply that issues an id when the room is
  short raises `Capacity` in the server; either way the call is settled as for a sealed
  port. `Close(file)` ends the caller's handle at issue, and its id once the server has
  taken the call; a `Close` cancelled before then queues `Released(file)` as a drop does.
  `NextEntries` bytes are whole records in `host.next_entries`'s format. `net.split` divides a socket in the caller
  and asks the server nothing; dropping a half queues `Released(id, Read)` or
  `Released(id, Write)` (FAIL-13).
- **Reply.** `provide.reply` is the commit point of the call: it copies the staged bytes into
  the caller's capacity and completes the operation with the reply's result. If
  cancellation or the call's deadline settled it first
  ([CONC-7](tasks.md#conc-7-cancellation-points)), it returns `Error(Withdrawn)`, discards
  the staged bytes and issues nothing. A `Call` given to the `reply` of another port is
  consumed as a dropped one and answers `Error(Withdrawn)`. A `Released` call needs no
  reply. A `Call` dropped without a reply settles its caller as a sealed port does.
- **Sealing.** Cleaning the `Served` end, or the end of its task, seals the port: every
  queued and later call completes with `Closed` (`Dir`, `Stdout`, `Stderr`) or `Reset`
  (`Net`), and drops of its handles still succeed. A sealed `Net` port retires its dropped
  `UdpSocket` ids at the seal and later ones at their drop, since no server will take their
  `Released` ([NET-16](net.md#net-16-udp-sockets-and-batches)). `provide.next` returns `None` only at
  quiescence ([FAIL-15](failure.md#fail-15-run-stops)).
- **Terminal.** `provide.stdout` and `provide.stderr` take the stream's terminal
  explicitly: `None` is not a terminal, and `Some(size)` is a terminal of that size. For the
  stream's whole life `stdout.is_terminal` and `stderr.is_terminal` on the result return
  whether it is `Some`, and `stdout.size` and `stderr.size` return the size, or `None` when
  either dimension is 0 ([Size](../../core/std/api/stdin.md#size)). Both are answered by the
  capability itself, never queued on the port, and a sealed port still answers them. There
  is no virtual `Stdin`; a model-world stream answers `false` and `None`.
- **Net authority.** A `provide.net` `Net` permits every address, name and TLS name, and
  `net.narrow` narrows it. Its TLS handshakes are answered by its server, so it never
  authorizes a granted `Secret` ([TLS-5](tls.md#tls-5-secrets)). `net.join` reunites a
  virtual socket's halves in the caller and asks the server nothing
  ([NET-7](net.md#net-7-owned-connections)). `secret.scram_sha256` on a virtual TLS half
  reaches its server only as `Send` calls, and only for a sealed `Secret`
  ([TLS-6](tls.md#tls-6-scram-sha-256)).
- **Cells.** `host.mono_ns`, `host.boot_ns`, `host.wall_ms`, `host.random` and `host.stopped` on a cell's
  capability answer at issue from the cell, never switch task and never allocate.
  `provide.clock` starts its monotonic and boot readings at `start_mono_ns` and its wall
  reading at `start_wall_ms`. `clock.advance_to(mut h, t)` raises monotonic
  time to `t` (a smaller `t` changes nothing), advances boot time by the same increase,
  and completes every sleep and expires every
  deadline at or before it, in deadline order and then issue order. `clock.set_wall` sets
  wall time alone and completes every `host.sleep_until_wall` it reaches, in reading order
  and then issue order. `clock.advance_to_with_boot(mut h, mono, boot)` raises the two
  elapsed readings to the supplied values without decreasing either, with boot time
  at least monotonic time. Both readings update before any monotonic wait is readied.
  This lets a simulated node's boot clock count time during a pause in its monotonic clock.
  A pending wall wait is no deadline of `clock.idle`'s. `clock.idle(mut h)` waits until every other task in every unit of the run
  waits on something inside the run (another task, a channel, a connection between its units, or a
  virtual provider), then returns the earliest pending deadline on the clock, or `None` when there is none. `provide.random(seed)`
  answers each draw with the next bytes of a SplitMix64 stream from `seed`, eight
  little-endian bytes per step, so the same seed gives the same bytes on every engine.
  `provide.random_tape(take b)` supplies successive whole draws from `b`, or raises
  `Bounds` without moving the cursor if the next draw does not fit. The cell retains
  its 32-byte charge and `len(b)` until it is withdrawn (below), as specified in [TLS-8](tls.md#tls-8-packet-ciphers-and-key-agreement).
  `provide.process()` gives a `Process` whose `host.stopped` completes once
  `process.trigger` has run, including a trigger before the call; dropping a `StopTrigger`
  without triggering never stops. Dropping a `ClockHand` freezes its clock: pending sleeps
  and deadlines on it never fire.
- **Cell lifetime.** A Clock, Random or Process cell or a port is charged to the limit
  current at its construction and withdrawn when that limit ends: its bound scope exits,
  its finalizer returns or its task finishes, by a fault too
  ([MEM-24](budgets.md#mem-24-logical-charges)).
  Its charge is refunded then, not handed over. Its capability is plain, so a copy can
  outlive the limit; every operation through such a copy, a `ClockHand`'s and a
  `StopTrigger`'s included, fails at issue with `Bounds`, as an exhausted
  `provide.random_tape` does. A wait parked on the cell when it is withdrawn is readied
  and fails the same way, and an `Until` on a withdrawn clock has passed. A withdrawn port
  is sealed: every unsettled call, a parked one included, fails its caller with `Bounds`,
  and so does every later operation through its capability or handles but a close, which
  succeeds; a parked `provide.next` fails with `Bounds`, and so does every later one. A
  `Call` its server already holds is settled: `payload` and `fill` supply nothing and
  `reply` returns `Error(Withdrawn)`. The slot can serve a new port of the same vocabulary.
  The new port has fresh designations even when its server uses the same object IDs.
- **Replay.** A virtual provider's results are computed by the run, so they are not
  inputs of [CAP-8](capabilities.md#cap-8-the-replay-point): a replay recomputes them. The
  host operations a server makes pass the replay point as every provider operation does.
  A virtual capability is `cross`, so it can reach another unit
  ([CONC-15](tasks.md#conc-15-units)). There its operations depend on another unit's order,
  so they pass the replay point in the unit that issues them, and so does every operation
  on the same cell in its home unit after the home's frame that first carries a
  capability of the cell out of the unit
  ([unit recordings](../recording.md#unit-recordings)).
  A seeded simulation is a test-run configuration over today's task order, not an order
  promise.
