# Checkbox wire benchmark

The reference uses Tokio `f3e855f`, tungstenite `89576c6` and
tokio-tungstenite `76e10f0`. This is a separate Cargo workspace, like the other
Rust benchmark references. It does not add networking dependencies to Hirsel.

```sh
cargo build --locked --release --manifest-path bench/wire/Cargo.toml --workspace --all-targets
cargo test --locked --manifest-path bench/wire/Cargo.toml --workspace --all-targets
cargo clippy --locked --manifest-path bench/wire/Cargo.toml --workspace --all-targets -- -D warnings
cargo fmt --manifest-path bench/wire/Cargo.toml --check
```

Run each process on the allowed host CPUs under `tools/capped`, with
`HIRSEL_MEMORY_LIMIT=2G` set before that command. Both binaries use two Tokio
workers. The commands below show their arguments; use the release executables
for measurements.

```sh
cargo run --locked --release --manifest-path bench/wire/Cargo.toml --bin wire-reference -- \
  --listen 127.0.0.1:7700 --wal /tmp/checkbox-reference.wal \
  --per-ip-sessions 20000 --principal-rate 1000000000 \
  --target x86_64-unknown-linux-gnu --n-place 1048576
cargo run --locked --release --manifest-path bench/wire/Cargo.toml --bin wire-load -- \
  echo --samples 2000
cargo run --locked --release --manifest-path bench/wire/Cargo.toml --bin wire-load -- \
  run --url ws://127.0.0.1:7700/hirsel --clients 1000 --rate 2000 --seconds 60
cargo run --locked --release --manifest-path bench/wire/Cargo.toml --bin wire-load -- \
  converge --url ws://127.0.0.1:7700/hirsel --clients 8 --toggles 1000 --pace 10
```

`--wants PATH` on both binaries accepts a binary `Hello(None, [toggle, board])`
with the generated checkbox identities and fingerprints. The benchmark's default
identities are for its own reference. In particular, its default erased Cover
fingerprint is zero. Supply the real generated Hello to exercise Hirsel with
the same driver. The hand codec follows WIRE-SPEC v2, with tests reading W3a's
canonical `core/programs/store/frame-vectors.json`. The Rust convergence driver
is a smoke test until the W4c/W6 SDK harness is available. A local Rust smoke
pass does not qualify the Node/browser `converge` acceptance.

The `hirsel.2` Welcome carries the Caller, launch target and `n_place`.
`--n-place` is required;
`--target` defaults to the host architecture with `unknown-linux-gnu`. These
identify the run and do not claim that the hand-written Rust registry enforces
a Hirsel task allowance.

The reference has one 125,000-byte bitmap. Its sole head appends fixed-size WAL
records containing the principal, stream, position, bit, result and version. It
groups for 1 ms, up to 1,024 commands, and calls Linux `fdatasync` before publishing
rows or outcomes. Startup locks the WAL, replays complete records, discards an
incomplete final record, and syncs recovery before accepting requests. Replaying
the latest position returns its durable answer without another flip. A stream
starts at zero. Keep the WAL when restarting to test recovery; use a new path
for an independent board.

Each connection has a bounded four-frame mpsc outbox and one coalesced pending
Batch. Coalescing keeps the latest row for each subscription/key and all owed
outcomes. An oversized pending Batch drops subscriptions and sends Reset; a
second overflow closes the connection. Cover subscriptions keep only their ids,
not private row copies. Each send holds one of five call units until its outcome
row reaches the writer. Up to five unsent V1 refusals are held separately; the
next refusal closes with 1008. `DONE_MAX` is the constant 8,192 B, with an 8,172 B
Returned payload bound. The outcome reserve is 41,140 B at q=4, leaving 220,976 B
for rows in a Batch completing one subscription. Toggle's sole application
reply is the five-byte value codec of `Ok(bool)`, within that bound by construction.
Server, load, and echo sockets use 8 KiB read buffers, zero write-buffer
thresholds, and 256 KiB frame/message caps. The write buffer can hold one
maximum wire frame and one maximum control frame (262,297 B total). Kernel socket buffers
are unchanged. The defaults are 32 sessions per IP, 10 new principals per IP per minute, and
20 calls/s per principal with burst 40. The command above applies the prescribed
benchmark overrides of 20,000 sessions and new principals per IP and 10^9 calls/s
per principal. Session admission refuses with HTTP 503; new-principal admission
refuses after Hello with Close 1013.

The load run counts exactly N subscribed connections, including one observer
that makes no calls. Each other connection uses one stream with at most one
call in flight. The scheduler offers arrivals at absolute instants independently
of replies. Offered arrival i toggles bit i modulo 1,000,000, and connections
are selected round-robin. This deterministic input is identical for both servers;
the reported coalescing counts expose how many calls share a publication.
Each connection has one queued arrival; a full queue increments
`missed_arrivals`. A caller's local position advances when an arrival enters
its queue. The `admitted` count records calls sent to the socket, and
`returned` records calls confirmed by a durable outcome. Incomplete runs
report the counts collected before the failure and exit with a failure status.

Latency correlates the caller's durable Batch version with the independent
observer's first Batch at that version or later, retaining timestamps even if the
observer arrives before the answer. JSON includes scheduled-to-observe and
socket-to-observe p50/p99, schedule lag, offered/admitted/returned/observed counts,
shared-Batch counts, process CPU time and wall time. An impossible observer-before-send ordering fails the run. The socket timestamp is
taken immediately before `send`; the scheduled timestamp also includes queuing
and generator delay. Echo RTT reports the framing/socket/clock floor and CPU
overhead separately. For the W9 throughput search, vary `--rate` while keeping
N=1,000 and the duration fixed; a qualifying run must be complete and have
scheduled-to-observe p99 at most 100 ms. Report the RSS of the reference process
separately from the generator. If connection setup fails, the load reports the
number of established subscriptions and the error before returning a failure.
No calls have been offered at that point.

On a fresh board, run `wire-load memory-subscribed` and
`wire-load memory-unsubscribed` with `--clients 10000 --server-pid PID` on
separate server runs. Both seed and verify all 1,954 chunks, then hold the
sessions open through the process memory sample. In the subscribed run every
session must complete the same snapshot. The output records the reached
population if an admission limit prevents the requested sample.

`wire-load codec` reads one binary Frame from stdin and writes its strict
re-encoding to stdout for the third-codec differential driver.

The Rust convergence driver uses seeded overlapping bits, including the six
specified boundary bits, and reconnects every client once after sending a Call.
It resends that same position under the same token, then XOR-folds Returned
calls. Every existing client image and a fresh subscription must equal that
fold. The recovery test also checks the durable version count, since parity
alone does not prove exactly-once execution.

Prior art: the checked endian reader adapts Goose `stdlib/binary.goose:9-25`
with strict Rust errors. The WAL uses Rust `File::sync_data` and lock ownership;
Goose `src/runtime/runtime_os.h:88-110,240-264` supplies the file/directory sync
ordering precedent, but whole-file replacement is unsuitable for group commit.
The fan-out adapts tokio-tungstenite `examples/server.rs:44-65`; its unbounded
channels are replaced with Tokio bounded mpsc plus chunk coalescing. Tungstenite
owns RFC framing and masking. No protocol implementation is copied. Open-loop scheduling adapts
`bench/apps/tasks/rust/src/bin/load.rs:114-157`; its per-request spawning is
replaced with bounded per-connection arrivals to preserve one in-flight Call
per stream. Echo measures the generator separately. Goose has no implemented
network benchmark to adopt.

Run these standalone tests with the commands above. The hand-derived vectors
catch field/tag/order
mistakes; the independent XOR and durable version oracles catch lost rows and
duplicate effects; fault injection checks that a failed sync emits nothing; the
synthetic timing test catches omitted scheduling delay and answer/observer
reordering. They assert behavior and remain valid across internal refactors.

## Private checkbox memory sweep

Run `python3 bench/awfy/checkbox_memory.py OUT --artifacts TOOLCHAIN
--build-cpus BUILD_CPUS --measure-cpus MEASURE_CPUS` on the board host.
Use at least two measurement CPUs for the server and loader. It launches a new private Hirsel server for each subscribed and
unsubscribed point at 64, 1,000 and 10,000 sessions. Pass any positive client count or list of counts with `--clients`.
Use `--wire-tools DIRECTORY` to reuse a release wire build.
Use `--mode memory-subscribed` or `--mode memory-unsubscribed` to select one
mode. The sweep retains the verified session count, highest
complete point per mode, and Linux `smaps`, `smaps_rollup` and `status`
snapshots with the seeded server idle, the sessions held, and 500 ms after
closing their transports. A completed point accounts for every held
session socket and checks that each closes. The snapshots describe process mappings
and OS threads.
