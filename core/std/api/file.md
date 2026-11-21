# File

[API overview](../API.md). [Operations and contracts](../file.hirsel). [Dir](dir.md).

Files use core's `HostFile` resource directly. An open file owns its position;
its finalizer closes it and discards the close error, while `close` exposes it.
Read, Write, Append, CreateNew and ReadWrite retain the host modes' distinct effects.
Allocating readers return `file.Error` to include Capacity and NotUtf8.
Each operation's `Prefix:` clause names effects that survive error or cancellation.
`file.stat(mut f)` reports the open file's kind, size and modification time
([Dir, Metadata](dir.md#metadata)).

## ReadWrite

`OpenMode` gains `ReadWrite`. It opens a file for reading and writing, creates it when it
is absent, and never empties it (`O_RDWR | O_CREAT`, Go's `os.OpenFile(name,
os.O_RDWR|os.O_CREATE, 0o666)`, Rust's `read(true).write(true).create(true)`). It is the
one mode in which a program reads back what it wrote through one handle, and it is the
mode a store opens its files with. The failures and replay of `open` are unchanged:
`NotFound` when a parent is missing, `Denied`, `Other(errno)`, and on failure nothing is
created. A cancellation at delivery closes the handle and may leave the file created.

## Positioned I/O

```
pub fn host.read_at(mut f: HostFile, at: u64, mut buf: Bytes) -> Result[u64, IoError] waits
pub fn host.write_at(mut f: HostFile, at: u64, b: Bytes) -> Result[(), WriteError] waits
pub fn file.read_at(mut f: HostFile, at: u64, mut buf: Bytes) -> Result[u64, IoError] waits
pub fn file.write_at(mut f: HostFile, at: u64, data: Bytes) -> Result[(), WriteError] waits
```

`read_at` reads from offset `at` and `write_at` writes at offset `at` (`pread` and
`pwrite`, Go's `ReadAt` and `WriteAt`, Rust's `FileExt::read_at` and `write_all_at`).
Neither moves the file's position.

- **Reading.** `read_at` appends at most the spare capacity of `buf` and returns the count.
  `Ok(0)` means `at` is at or past the end of the file, or `buf` has no spare capacity. A
  shorter count than requested is a partial read, as with `host.read`. A failed read has
  appended nothing.
- **Writing.** `write_at` writes all of `b`, extending the file when the range passes its
  end. Bytes skipped over by a write past the end read back as zeros. A `WriteError`'s
  `written` bytes, from `at` onward, stay written.
- **Modes.** `read_at` needs a Read or ReadWrite handle, and `write_at` needs a Write,
  CreateNew or ReadWrite handle. Any other handle gives `Denied` before issue, with no host
  effect. `write_at` on an Append handle gives `Denied`, because Linux `pwrite` would append
  instead of writing at `at`, which Go also rejects.
- **Failures.** `Denied` as above. `Closed` for a closed handle. `Other(errno)`, for example
  `EFBIG` or `ENOSPC`. `at + len(b)` overflowing `u64` is `Bounds`, a failure checked before
  issue.
- **Charging.** Nothing is admitted, and the bytes land in capacity the caller already
  holds.
- **Replay.** Issue: the handle's ordinal, `at`, and the spare capacity or the canonical
  bytes. Delivery: the bytes read, or the committed count. Neither waits after issue, and
  each is a cancellation point at issue only. A write cancelled at delivery leaves its
  bytes written. Durability is still `host.sync`'s alone
  ([core](../../../docs/spec/core.md#contracts-the-signatures-do-not-state)).
