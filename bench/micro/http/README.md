# HTTP B1 Rust baselines

From the repository root, with Cargo, Python 3 and Valgrind installed:

```sh
HIRSEL_MEMORY_LIMIT=2G taskset -c 0-11,16-27 tools/capped bench/micro/http/instructions tokio
HIRSEL_MEMORY_LIMIT=2G taskset -c 0-11,16-27 tools/capped bench/micro/http/instructions hyper
```

Each command builds the isolated benchmark workspace in the locked release
profile, pins itself and its children to one inherited CPU in 0-11 or 16-27,
and prints a short text table. `--cpu` selects an allowed inherited CPU.
`--command '/path/to/server argument'` skips building and accepts another named
server, including the future Hirsel implementation. The command is split into
arguments without a shell.

The workload is `http1-plaintext-keepalive`. One client sends the following
request, waits for the complete response, and repeats on the same connection:

```text
GET / HTTP/1.1\r\nHost: localhost\r\nConnection: keep-alive\r\n\r\n
```

Hyper and Hirsel send the same response, with a live IMF-fixdate timestamp:

```text
HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 12\r\nDate: <timestamp>\r\n\r\nHello, world
```

HTTP/1.1 keeps the connection alive without a response `Connection` header.
Hyper uses its current-thread Tokio runtime, TCP_NODELAY, automatic Date,
HTTP/1 connection and a `Full<Bytes>` body. Hirsel also records replay, charges
budgets, validates Host and rejects duplicate Content-Length. Tokio+httparse
is a minimal server floor with a static response, lowercase headers and no Date.

Only the server runs under Callgrind. One sample consists of two fresh server
processes, serving 1,024 and 8,192 requests respectively. The reported cost is
`(Ir_large - Ir_small) / (large - small)`. Initialization, one accept, and EOF
teardown occur on both sides and cancel in the difference. `--small` and
`--large` change the counts. Every response must match the expected wire bytes,
and both processes must finish successfully with their exact served counts.
Temporary Callgrind profiles are removed after reading their `Ir` summaries.
The table includes both whole-process totals so subtraction is visible.
The shared instruction-measurement lock is held for each server process only,
and released between the two counts. Builds do not hold the lock.

This is an instruction baseline for one serial plaintext connection, not a
throughput or latency claim. No regression threshold is set here. The spec
sets it after the first Hirsel measurement.

## Server command contract

A server accepts `--requests N --port 0`, binds only to loopback, writes
`LISTEN 127.0.0.1:<port>` with a newline and flushes stdout before accepting.
It serves one connection. After N replies the client shuts down its write
side; the server consumes EOF, writes `SERVED N`, and exits successfully.
There is no extra readiness connection. Each request/reply has a 30-second
inactivity timeout, and each sample has a 15-minute execution limit.

## Checks and prior art

`instructions tokio --check` and `instructions hyper --check` each check three
byte-identical replies without Callgrind. Run the arithmetic and profile-reader
tests with `python3 -m unittest discover -s bench/micro/http -p 'test_*.py'`.
These isolated benchmarks and Python tests have no Buck
target; run them directly when changing this workload.

- ADAPTED Goose `bench/run_bench.py:1-25`: its common-output check prevents
  comparisons of different work, and its startup floor exposes short-run
  overhead. Here wire bytes are the common output and two totals subtract the
  startup floor instead of reporting it beside the result. Goose measures wall
  time; Callgrind's instruction event avoids scheduling-time noise.
- ADAPTED [Hyper's server example](https://github.com/hyperium/hyper-util/blob/v0.1.21/examples/server.rs):
  `handle_request` at line 26 and `serve_connection` at line 57 provide a real
  protocol implementation. Its task-per-connection loop is useful for general
  service, but adds a task for this one-connection workload. Both baselines
  directly await the single connection; Hyper selects HTTP/1 only, uses an
  unboxed fixed body and includes Date. The Tokio floor retains the existing
  httparse loop, which is smaller but deliberately lacks Hyper's general HTTP
  framing and service behavior.
- ADOPTED Callgrind's `events` and `summary` format. The reader selects `Ir` by
  event name; other counters and the uninstrumented client are excluded.

## Hirsel

Run `instructions hirsel --compiler PATH --runtime ARCHIVE --cc PATH` with
the checkout's checked driver, runtime archive and pinned Clang wrapper.
The command builds the HTTP connection task with LLVM release code and
serves one socket. It verifies the same status, content type, body length
and body as Hyper, including the casing and format of the live Date field. The
served count comes from responses whose writes completed before peer EOF.
