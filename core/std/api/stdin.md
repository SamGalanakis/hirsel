# Stdin

[API overview](../API.md). [Io](io.md). [Core provider signatures](../../core/stdin.hirsel).
[Host contract](../../../docs/spec/core.md#contracts-the-signatures-do-not-state).

Core module `stdin` holds one provider operation
([CAP-5](../../../docs/spec/capabilities.md#cap-5-provider-operations)):

```
pub type StdinError = Aborted | TimedOut | Other(u32)
pub fn read(i: Stdin, mut buf: Bytes, until: Until) -> Result[u64, StdinError] waits
```

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
([CONC-7](../../../docs/spec/tasks.md#conc-7-cancellation-points)). It is a
cancellation point at issue and while it waits. If cancellation reaches the read
while it waits, the read gives `Aborted`. `until` is an absolute deadline, as in NET-1.
When the deadline passes before input arrives, the read gives `TimedOut`. `Never`
waits without a deadline. In both cases the read has appended nothing. Bytes the host
supplies after an abort or a timeout are not lost: the next read delivers them first.
The host provider does not change the flags of the inherited descriptor, because the
file description can be shared with other processes. It reads through a helper that
waits on the host, as Tokio's `stdin` does.

**Failures.** `Other(errno)` reports any other host error, for example `EIO` from a
closed terminal. A failed read leaves the stream where it was.

**Charging.** `read` admits nothing and charges no limit. The bytes land in capacity
the caller already holds ([core](../../../docs/spec/core.md#contracts-the-signatures-do-not-state)).

**Replay.** Every read passes the replay point
([CAP-8](../../../docs/spec/capabilities.md#cap-8-the-replay-point)). At issue it
passes the spare capacity of `buf` and `until`, and at delivery its bytes or its
error. When recording is on, the recording holds both, and a replay delivers the
recorded bytes without reading the host
([recording](../../../docs/recording.md#what-a-recording-holds)). Recording is off by
default.

**Std.** [Io](io.md) reads the stream through its `Input` source:
`io.lines(Stdin(i), cap)` reads lines and `io.read_all(i)` reads to end of input.
