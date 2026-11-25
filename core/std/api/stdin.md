# Stdin

[API overview](../API.md). [Io](io.md). [Core provider signatures](../../core/stdin.hirsel).
[Host contract](../../../docs/spec/core.md#contracts-the-signatures-do-not-state).

Core module `stdin` holds one
[provider operation](../../../docs/spec/capabilities.md#cap-5-provider-operations):

```
pub type StdinError = Aborted | TimedOut | Other(u32)
pub fn read(i: Stdin, mut buf: Bytes, until: Until) -> Result[u64, StdinError] waits
pub fn is_terminal(i: Stdin) -> bool
```

Modules `stdout` and `stderr` declare the same query, `stdout.is_terminal(o: Stdout) -> bool`
and `stderr.is_terminal(e: Stderr) -> bool` ([Terminals](#terminals)).

**Authority.** `Stdin` is the run's standard input: a plain, `shareable`, `send`
capability that `main` receives only from `--grant Stdin`. Without the grant, no
code in the run can read the process's descriptor 0.

**Reading.** `read` appends at most the spare capacity of `buf` and returns the number
of bytes it appended. It never changes the capacity of `buf`. `Ok(0)` means end of input,
or that `buf` had no spare capacity. A read after end of input asks the host again, so a
terminal can deliver more after Ctrl-D, as in Go's `os.Stdin.Read` and Rust's
`Stdin::read`. A read that returns an error has appended nothing. Copies of one `Stdin`
read one stream: each byte the host supplies is delivered by exactly one read. Bytes
reach one task's successive reads in stream order.

**Waiting.** `read` waits after issue until input is ready
([cancellation points](../../../docs/spec/tasks.md#conc-7-cancellation-points)). It is a
cancellation point at issue and while it waits. If cancellation reaches the read
while it waits, the read gives `Aborted`. `until` is an absolute deadline, as for socket
operations. When the deadline passes before input arrives, the read gives `TimedOut`. `Never`
waits without a deadline. In both cases the read has appended nothing. Bytes the host
supplies after an abort or a timeout are not lost: the next read delivers them first.
The host provider does not change the flags of the inherited descriptor, because the
file description can be shared with other processes. It reads through a helper that
waits on the host, as Tokio's `stdin` does.

**Failures.** `Other(errno)` reports any other host error, for example `EIO` from a
closed terminal. A failed read leaves the stream where it was.

**Writes to `Stdout` and `Stderr`.** A write whose reader has gone, such as `tool | head -1`
after `head` exits, reports `WriteError(written, error: Closed)`, where `written` counts the
bytes the reader's end accepted first. `Closed` is also the answer for a stream that was
absent at launch ([entry grants](../../../docs/spec/capabilities.md#cap-4-entry-grants)). A
command-line program that stops quietly when its output is closed matches `Closed`; it never
needs the host's `EPIPE` number.

**Charging.** `read` admits nothing and charges no limit. The bytes land in capacity
the caller already holds ([core](../../../docs/spec/core.md#contracts-the-signatures-do-not-state)).

**Replay.** Every read passes
[the replay point](../../../docs/spec/capabilities.md#cap-8-the-replay-point). At issue it
passes the spare capacity of `buf` and `until`, and at delivery its bytes or its
error. When recording is on, the recording holds both, and a replay delivers the
recorded bytes without reading the host
([recording](../../../docs/recording.md#what-a-recording-holds)). Recording is off by
default.

**Std.** [Io](io.md) reads the stream through its `Input` source:
`io.lines(Stdin(i), cap)` reads lines and `io.read_all(i)` reads to end of input.

## Terminals

`stdin.is_terminal(i)`, `stdout.is_terminal(o)` and `stderr.is_terminal(e)` answer whether
the stream's descriptor referred to a terminal when the run started. A program uses the
answer to choose colour, progress output or an interactive prompt, as Rust's
`IsTerminal` and Go's `term.IsTerminal` are used.

- **Snapshot.** The native launcher takes one bit per granted stream before `main`:
  `isatty(0)`, `isatty(1)` or `isatty(2)` returning 1 gives `true`, and any other result,
  including a closed descriptor, gives `false`. A pipe, a regular file and a character
  device that is not a terminal, such as `/dev/null`, are all `false`. The evaluator's host
  launch takes the same bits. An embedded runtime receives them from its embedder before
  entry, as it receives launch metadata ([launch](../../../docs/spec/launch.md)). A stream
  that is not granted has no bit and is not probed.
- **Never changes.** The bit is the stream's for the whole run. If the descriptor is
  redirected, closed or hung up after launch, or the terminal goes away, the answer does
  not change. Every copy of the capability gives the same answer.
- **Authority.** The stream's own capability. Nothing else is observed: the call does not
  read the descriptor, the terminal's size or its modes. The size is its own query
  ([Size](#size)).
- **Charging.** The call is an ordinary core call: one step, nothing admitted, no wait,
  no failure.
- **Replay.** The bit is part of the stream's entry grant, so it passes
  [the replay point](../../../docs/spec/capabilities.md#cap-8-the-replay-point) once, at entry, and
  the call adds no frame. A recording keeps the bit in the stream's grant, and a replay
  answers from it whatever its own descriptors are
  ([recording](../../../docs/recording.md#what-a-recording-holds)).
- **Virtual streams.** `provide.stdout(bytes n, terminal)` and `provide.stderr(bytes n,
  terminal)` take an `Option[TermSize]` when they are constructed: `None` is not a terminal
  and `Some(size)` is a terminal of that size, so a test chooses either answer
  ([core](../../../docs/spec/core.md#provide)). The query is answered by the capability and
  never reaches the port. A model-world stream answers `false`.

### Size

`stdout.size(o)` and `stderr.size(e)` return `Option[TermSize]`, where
`pub type TermSize { cols: u16, rows: u16 }` counts character cells. A program uses it to
wrap help text or size a progress bar, as Rust's `terminal_size` and Go's `term.GetSize`
are used. There is no `stdin.size`, and the run never opens `/dev/tty`.

- **Answer.** `None` when the stream's terminal bit is `false`, when the descriptor gives no
  size, or when either dimension is 0. Otherwise the size the descriptor reports at the
  call: `ioctl(fd, TIOCGWINSZ)` on the stream's own descriptor, on Linux and Darwin alike,
  with the request constant taken from libc. The size is read at each call, so a window
  resized during the run answers its new size; there is no resize notification.
- **Authority.** The stream's own capability, and nothing else.
- **Charging.** One step, nothing admitted, no wait, no failure.
- **Replay.** Unlike the bit, the size is not part of the grant. A call on a stream whose
  bit is `false` answers `None` without reaching the provider and adds no frame. A call on a
  terminal is one provider operation, so a recording holds one frame per call with its
  answer, and a replay answers each call from its frame whatever its own descriptors are.
- **Virtual streams.** `Some(TermSize(cols: c, rows: r))` given to `provide.stdout` or
  `provide.stderr` is the size the stream answers for its whole life, and a zero dimension
  is a terminal that answers `None`, as a descriptor's. The query never reaches the port.
  A model-world stream answers `None`.

### Verification properties

| property | bug caught | verification setup |
|---|---|---|
| **Bit matches `isatty` per stream** | testing for a character device in place of a terminal, or reading one descriptor for all three | Run a program that writes the three bits to a file, natively on both backends. Case 1: stdin and stdout on the slave of a pty from `openpty`, stderr on a pipe (`true`, `true`, `false`). Case 2: stdin from `/dev/null`, stdout to a regular file, stderr on the pty (`false`, `false`, `true`). Covered in `crates/hirsel-core-driver/src/host/child.rs`. |
| **Replay answers from the grant** | a replay that probes its own descriptors | Record case 1 above, then replay it with all three descriptors on pipes. The replay ends `Sealed`. |
| **Virtual bit** | a constructor that ignores its argument, or a query that queues on the port | `provide.stdout(64, Some(TermSize(cols: 80, rows: 24)))` gives `true` and `provide.stderr(64, None)` gives `false`, including after the `Served` end is dropped. The server's `provide.next` sees no call. Covered in `core/core/tests/provide.hirsel`. |
| **Size matches `TIOCGWINSZ` per stream** | reading one descriptor for both streams, swapping the dimensions, or answering a size for a pipe | A program writes both sizes to a file, natively on both backends. Stdout and stderr on a pty whose window is set to 132 by 43 give `132x43` twice; both on pipes give `none` twice. Covered in `crates/hirsel-core-driver/src/terminal_fixture.rs`. |
| **Replay answers the size from the tape** | a replay that queries its own descriptors, or a non-terminal query that records a frame | Record that program with stdout on the 132 by 43 pty and stderr on a pipe: one size frame. Replay it on pipes: it writes the recorded sizes and ends `Sealed`. A run with both streams piped records no size frame. Covered in `crates/hirsel-core-driver/src/terminal_fixture.rs`. |
| **Virtual size** | a constructor that drops the size, or a query that queues on the port | `provide.stdout(64, Some(TermSize(cols: 100, rows: 30)))` answers that size, a zero dimension answers `None` on a terminal, and `None` answers `None`; no port sees a call. Covered in `core/core/tests/provide.hirsel`. |

