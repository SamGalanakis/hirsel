# File

[API overview](../API.md). [Operations and contracts](../file.hirsel). [Dir](dir.md).

Files use core's `HostFile` resource directly. An open file owns its position;
its finalizer closes it and discards the close error, while `close` exposes it.
Read, Write, Append, CreateNew and ReadWrite retain the host modes' distinct effects.
`file.Error` has the variants `Io(IoError)` and `NotUtf8`. A refused buffer
growth raises `Capacity`. Cleanup closes the file and refunds the buffer.

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

## Replace

*Specified 2026-11-24 (std Group 2), not yet implemented.*

```
pub fn host.mkstemp(d: Dir, mut name: String) -> Result[HostFile, IoError] waits
pub fn file.create_temp(d: Dir, mut name: String) -> Result[HostFile, IoError] waits
pub type ReplaceError = NotReplaced(IoError) | Unsynced(IoError)
pub fn file.replace(d: Dir, name: String, data: Bytes) -> Result[(), ReplaceError] waits
```

`file.replace(d, name, data)` makes the file `name` in `d` hold exactly `data`, so that a
reader or a host crash sees either the old contents or the new ones, never a mix. This is
the write-to-temp, sync, rename, sync-directory sequence that every program writing a
config, state or cache file otherwise hand-rolls, often without one of the two syncs.

```hirsel
let state_dir = std.dir.narrow(d, "state")?        // once, at startup
std.file.replace(state_dir, "cursor.json", bytes)?   // each save
```

`name` is one component, so the file is directly in `d`'s root. A program that saves into
a subdirectory narrows to it once (`dir.narrow`) and keeps that `Dir`. The directory sync
covers only the names in a `Dir`'s root (core.md, `host.sync_dir`), and a narrowed `Dir`'s
descriptor stays open until the run ends (dir.md, Narrowing), so narrowing inside every
call would cost one descriptor per call.

**Temporary files.** `host.mkstemp(d, mut name)` is `host.mkdtemp` for a file. It checks
before issue that `name` has at least 32 bytes of spare capacity, failing with `Bounds`
otherwise. It creates a new, empty regular file in `d` named `name`'s bytes followed by a
fresh suffix of 1 to 32 ASCII letters and digits, as one exclusive create (`O_CREAT |
O_EXCL`, never an existing file). It appends only the suffix to `name` and returns the file
open for writing, as the `CreateNew` mode does. The prefix rules are `mkdtemp`'s: a prefix
with which the name is not one valid component gives `Denied`, and on an error nothing is
created or appended. The file is created with the permissions that `open` in `Write` mode
gives a new file. It is not removed automatically. `file.create_temp` is the std name, as
`dir.create_temp` is for `mkdtemp`.

**The sequence.** `replace` does, in order:

1. Refuse before issue, with `NotReplaced(Denied)` and no host effect, a `name` that is not
   a valid path of exactly one component (`path.is_valid` and no `/`).
2. `host.mkstemp(d, mut tmp)` with the prefix `.hirsel-tmp-`. The prefix is fixed, and the
   temp name is at most 44 bytes (the 12-byte prefix plus `mkstemp`'s suffix of at most 32), so it fits whatever `name`'s length.
3. `host.write` all of `data`, then `host.sync` the file, then `host.close` it.
4. `host.rename(d, tmp, name)`.
5. `host.sync_dir(d)`.

A failure in steps 2 to 4 returns `NotReplaced(e)`. Before it returns, `replace` removes
the temp file (`host.remove`) if step 2 created it, and ignores that removal's own error.
`name` still holds its old contents, or is still absent. A failure in step 5 returns
`Unsynced(e)`. Readers already see the new contents, but whether `name` survives a host
crash with them is an unknown outcome (core.md, sync barriers). The caller may call
`host.sync_dir(d)` again, but a later success says nothing about this rename. Every error
passes through from the host primitive that failed:

- `NotReplaced(Denied)` when `d` is read-only (`host.read_only`) or when `name` exists and is
  not a regular file. `host.rename` refuses a directory or a symbolic link at `to`, so a
  symbolic link at `name` is refused, never replaced and never followed.
- `NotReplaced(NotFound)` cannot come from `name` itself, because replace creates it when
  it is absent.
- `NotReplaced(Other(errno))` for, for example, `ENOSPC` or `EDQUOT` from the write or the
  sync, or `EMFILE` from the create.

`Prefix:` (what a failure, a cancellation or a crash leaves):

- After a `NotReplaced` return, `name` is unchanged and the temp file is gone, unless its
  removal failed.
- A cancellation at any wait point from step 2 to step 4 (each is a cancellation point at
  issue, CONC-7) stops `replace` without its removal step. Cleanup closes the temp file,
  and the file stays in `d` under a `.hirsel-tmp-` name. `name` is unchanged.
- A cancellation observed at the delivery of step 4 or at step 5 leaves `name` holding
  the new contents for readers, with durability unknown, as `Unsynced`.
- A host crash before step 5 completes leaves `name` holding the old contents or the new
  ones, each complete, because the data was synced before the rename. It may also leave
  one `.hirsel-tmp-` file. Nothing removes stale temp files automatically. A program that
  cares lists `d` at startup and removes the names that start with `.hirsel-tmp-`.

- **Authority.** `d` alone, writable.
- **Failures.** As above. A refused allocation raises `Capacity` before any host call, and
  cleanup refunds it.
- **Charging.** `replace` admits the 44-byte temp-name `String` under the current limit and
  refunds it before it returns. The data is written from the caller's `Bytes`. No copy is
  made. `mkstemp` admits nothing, and its name bytes land in capacity the caller holds.
  Steps: `replace` makes at most seven host calls, each one step (MEM-30), plus O(1) of its
  own, whatever `data`'s length. The host's write time is a wait, not steps.
- **Replay.** Every host call passes its own replay point. `mkstemp`'s issue records
  `d`'s ordinal, the prefix and the spare capacity, and its delivery records the new
  file's ordinal and the suffix, so a replay sees the same temp name.

### Prior art

- Go has no std atomic write. `os.CreateTemp` (`/tmp/ref-go`, go 8eaf19e,
  `src/os/tempfile.go:35`) is the temp-file half. ADOPTED: the exclusive create with a fresh
  random suffix, which is `mkstemp` here. NOT TAKEN: its 0600 mode, because a temp file
  that becomes `name` would silently make a replaced file private.
- Rust `tempfile` (`/tmp/ref-tempfile`, tempfile 57a1a34, `src/file/mod.rs:767` `persist`)
  renames a temp file over a path but leaves both syncs to the caller. REPLACED: here both
  syncs are part of the one call, so the durable form is the default one.
- Goose's `write_file_atomic` (`/tmp/ref-goose`, goose d5ae5bb, `stdlib/os.goose:17`,
  `src/runtime_inline.h:4475`) is the same sequence: an exclusive create beside the
  target, write, fsync, close, rename, then a sync of the directory. ADOPTED: the order of
  steps and the removal of the temp file on failure. REPLACED: Goose ignores the directory
  sync's result (`gs_os_sync_dir(p)`), and here it is `Unsynced`. Goose returns one bool,
  and here the error separates not replaced from replaced with unknown durability. NOT
  TAKEN: Goose copies the old file's permission bits (`fchmod`). Hirsel has no permission
  operations (dir.md, Metadata), so a replaced file gets a new file's permissions.

### Open

- **Permissions are not kept.** A replaced file gets a new file's permissions, because
  Hirsel has no permission operations. A program that keeps a file private would see
  `replace` reset it. If permission operations are ever added, `replace` would copy the
  old bits, as Goose does. That change is additive.
- **Stale temporary files.** A crash between `mkstemp` and `rename` leaves a
  `.hirsel-tmp-` file that nothing removes. Removing them at start is unsafe, because
  another process may be mid-replace in the same directory. A program that cares lists
  the directory and removes old ones itself. A `file.reclaim_temps(d, older_than)` would
  be additive.
