# Stdin

[API overview](../API.md). [Io](io.md). [Core provider signatures](../../core/stdin.hirsel).
[Host contract](../../../docs/spec/core.md#contracts-the-signatures-do-not-state).

Core module `stdin` holds two
[provider operations](../../../docs/spec/capabilities.md#cap-5-provider-operations):

```
pub type StdinError = Aborted | TimedOut | Other(u32)
pub fn read(i: Stdin, mut buf: Bytes, until: Until) -> Result[u64, StdinError] waits
pub fn is_terminal(i: Stdin) -> bool
pub type SecretReadError = NotTerminal | Busy | Empty | TooLong | Eof | Aborted | TimedOut | Policy(SecretError) | Other(u32)
pub fn read_secret(i: Stdin, uses: String, until: Until) -> Result[Secret, SecretReadError] waits
```

`read_secret` reads a line with echo off into a `Secret` ([Secret input](#secret-input)).

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


## Secret input

```
pub type SecretReadError = NotTerminal | Busy | Empty | TooLong | Eof | Aborted | TimedOut | Policy(SecretError) | Other(u32)
pub fn read_secret(i: Stdin, uses: String, until: Until) -> Result[Secret, SecretReadError] waits
```

`stdin.read_secret(i, uses, until)` reads one line from the terminal on the run's standard
input with echo off, and returns it as a [`Secret`](../../../docs/spec/tls.md#tls-5-secrets)
with policy `uses`. The bytes go from the host into run-owned secret storage and never into
program memory, as for a [credential fetch](../../../docs/spec/tls.md#tls-7-credential-grants).
It is the one way a program prompts for a password or a token, as `getpass(3)`, Python's
`getpass` and Go's `term.ReadPassword` are used. The program writes its own prompt first,
to whichever stream it likes, then calls. There is no prompt parameter and no pre-`main`
prompt, so a program parses `--help`, `--no-input` or `--token-file` before it decides to
prompt.

- **Policy first.** `uses` is a policy in canonical spelling, as for `secret.seal`. A
  policy that does not parse is `Policy(Invalid)`. The call then admits the charge below,
  and a refusal is `Policy(Capacity(c))`, a value as for `seal`. Both are decided before
  the terminal is touched, have no host effect and pass no replay point.
- **A terminal, checked live.** The provider asks the descriptor for its modes
  (`tcgetattr`). If that fails, the result is `NotTerminal` and nothing is read: the call
  never falls back to an echoing read, and a following `stdin.read` sees every byte. The
  check is the descriptor's state at the call, not the terminal bit taken at launch.
- **One reader.** The read goes through the stream's one helper, so it cannot race
  `stdin.read`. If another read of the stream is pending, from any copy of `Stdin`, the
  result is `Busy` and the terminal is not touched. Bytes the helper holds from an earlier
  read are dropped when the modes change, and the mode change itself discards input typed
  before it (`TCSAFLUSH`), so typed-ahead input is discarded, as `getpass` does. The read
  leaves the helper's queue only once the modes are restored, so a `stdin.read` queued
  behind it starts after that flush: it never sees the rest of an over-long line, or a line
  typed while a timeout or a cancellation was ending the read.
- **Modes.** Echo is cleared, and `ECHONL` and canonical input (`ICANON`) are set; the
  signal characters and `ICRNL` are kept as the terminal had them. So the line is edited
  as usual, even on a terminal left in non-canonical mode, Ctrl-C still signals, and the
  terminal itself echoes the Enter as a newline. Nothing else is
  written: no `Stderr` is needed.
- **The line.** Bytes up to the first newline, with the newline and a `\r` before it
  removed. End of input after some bytes (Ctrl-D mid-line) ends the line there. An empty
  line is `Empty`. End of input before any byte is `Eof`. A line longer than 1024 bytes is
  `TooLong`, and the rest of it is discarded when the modes are restored. 1024 is the
  smallest canonical line limit of the supported hosts (Darwin's `MAX_CANON`; Linux allows
  4095).
- **Restore.** The modes the terminal had before the call are put back before the call
  returns, whatever its result. They are also put back if the run ends while the read is
  pending: at the run's end, at a run stop (status 102, before `_exit`), and on the
  terminating arm of each signal handler below. A run stop and a handler restore only
  while the terminal still holds the modes the call set, apart from `FLUSHO` and `PENDIN`,
  which the kernel changes as output and input state, so a run stopped by `SIGTTOU`
  before its change, and killed from there, writes nothing over the modes the foreground
  has set since. A process killed by `SIGKILL` cannot restore its terminal; `stty sane`
  repairs it, as after any program.
- **Signals.** At the first `read_secret` of a run, the host installs a handler that
  restores the modes and then takes the signal's default action, for `SIGHUP` and
  `SIGQUIT`, and for `SIGINT` and `SIGTERM` when the run has no `Process` stop relay. A
  signal whose action is not the default when the handler would be installed keeps it:
  one that is ignored (a run under `nohup`) or one an embedding host handles. The previous
  actions are put back at the run's end, except where another owner, such as the stop relay
  of a run overlapping in the same process, has set the action since. With `Process`, the first
  `SIGINT` or `SIGTERM` still completes `host.stopped`: the program cancels the read, and
  the cancellation restores the modes. A second one takes the relay's default action,
  which restores first. `SIGTSTP` and `SIGCONT` are not handled: an interactive shell saves
  and restores the terminal modes of a job it stops.
- **Background.** Changing the modes from a background process group stops the process
  with `SIGTTOU`, as for any program, so a background run does not turn echo off under
  whatever is in the foreground. Every restore blocks `SIGTTOU` while it runs, so a run
  moved to the background during the read still restores instead of stopping.
- **Waiting.** `read_secret` waits after issue until a line arrives. It is a cancellation
  point at issue and while it waits, and gives `Aborted` when cancellation reaches it there
  and `TimedOut` when `until` passes, as `read` does. Either way the modes are restored and
  no designation is made.
- **Failures.** `Other(errno)` reports any other host error, such as `EIO` from a hung-up
  terminal or a refused mode change.
- **Authority.** The `Stdin` capability. The result's uses are `uses`, and the secret is
  as opaque to the program as a granted one: the program never held its bytes.
- **Charging.** The call admits `seal`'s charge with the length fixed at 1024:
  `round_up_8(pattern_bytes + 1024 + 32)`, whatever was typed, so no charge, refusal or
  later refund depends on the secret. A call that gives an error refunds it at once; a
  secret's charge is refunded when its last designation is released, as `seal`'s is.
- **Replay.** The read is one provider operation, `STDIN_SECRET`. Its issue holds
  `until` and the policy text. Its delivery holds the outcome and, on success, the new
  designation's delivery position; never the bytes, their length or any function of them
  ([recording](../../../docs/recording.md#credentials)). A replay touches no terminal and
  installs no handler, and gets a designation with no bytes behind it, as for a granted
  secret. The operations that read the secret, such as `secret.hmac_sha256`, deliver their
  recorded results.
- **Virtual streams.** There is no virtual `Stdin`, so the call is tested on a native
  pseudo-terminal only.

### Secret input laws

All run natively on Linux on the slave of a pty from `openpty`, and each compares the
pty's modes after the run with the modes before it. Darwin builds them and has no runner
yet. Covered in `crates/hirsel-core-driver/src/secret_prompt_fixture.rs`.

| law | bug caught | verification setup |
|---|---|---|
| **A. The line becomes a secret and is not echoed** | echo left on, bytes copied into program memory, a missing or doubled newline, a read that keeps non-canonical mode | Type `hunter2` and Enter on the master. The program writes `hmac_sha256(secret, "data")` and the tag equals the value computed by hand. The master's output holds none of the typed bytes and exactly one newline. The same holds on a pty that starts in non-canonical mode, whose modes come back non-canonical. |
| **B. Restored on a terminating signal** | a handler that dies without restoring, or no handler | With no `Process` grant, send `SIGINT`, `SIGTERM`, `SIGHUP` and `SIGQUIT` to a program blocked in the read. Each run dies by that signal and the modes are as before. |
| **C. The stop relay keeps the first signal** | a handler that takes `SIGINT` from `host.stopped`, or a relay arm that dies without restoring | With `Process`, the first `SIGINT` completes `host.stopped`, the program cancels the read and gets `Aborted`, and the modes are restored. A program that ignores `host.stopped` and gets `SIGINT` until it dies is restored too. |
| **D. Restored after a timeout and a run stop** | a deadline or `_exit(102)` path that skips the restore | `until` 200 ms ahead gives `TimedOut` with the modes restored. A run stop during the read exits 102 with the modes restored. |
| **E. `SIGTTOU` during restore** | a restore that stops the process instead of restoring | The test is the pty's session leader and starts the program in its own foreground group. Once the program is blocked in the read, the test makes its own group the foreground one and sends `SIGTERM`. The modes are restored and the program dies by `SIGTERM`, not stopped. |
| **E2. A background read changes nothing** | arming that blocks `SIGTTOU` and turns echo off under the foreground, or a handler that restores a change never made | The program starts in a background group of the pty's session and calls `read_secret`. It is stopped by `SIGTTOU` with the modes unchanged. The test changes a mode, then sends `SIGTERM` and `SIGCONT`: the program dies by `SIGTERM` and the test's change stays. |
| **F. The recording holds no secret** | a recorded line, length or length-dependent charge | Record two runs that type secrets of different lengths: neither tape holds the typed bytes, and their `STDIN_SECRET` frames are identical. A replay on pipes ends `Sealed`. |
| **H. Nothing reaches a read queued behind** | a read that leaves the queue before the restore's flush | A task's `stdin.read` is queued behind the secret read, and the test stops the terminal's output (Ctrl-S) behind a pending write, so each restore waits in its drain. Typing 1100 bytes and Enter gives `TooLong`; a line typed after `until` passes gives `TimedOut`; one typed after a cancellation gives `Aborted`. After Ctrl-Q, the queued read gets only the line typed next. |
| **G. Not a terminal** | an echoing fallback, or a check that consumes input | With stdin on a pipe holding `abc`, the call gives `NotTerminal` and a following `stdin.read` reads `abc`. |
