# Dir

[API overview](../API.md). [Operations and contracts](../dir.hirsel).

Paths resolve below the supplied Dir capability and cannot leave its tree.
`create_dir`, `remove_all` and `create_temp` use host primitives with replay points
at issue and delivery. Their source contracts state which host effects remain
when cancellation arrives at delivery.

## Narrowing

```
pub fn host.open_dir(d: Dir, p: String) -> Result[Dir, IoError] waits
pub fn dir.narrow(d: Dir, p: String) -> Result[Dir, IoError] waits
```

`dir.narrow(d, p)` returns a `Dir` rooted at the existing directory `p` below `d`
([CAP-7](../../../docs/spec/capabilities.md#cap-7-attenuation)). It is `host.open_dir`, with
the same confinement as every Dir: every path through the child resolves beneath it,
symbolic links included, so the child never permits more than `d`. This is how a caller
hands a store or another library exactly one subtree (the 2026-11-18 O1 ruling: Disk takes
a caller-narrowed Dir). It matches Rust cap-std's `Dir::open_dir` and WASI's
`path_open` with `O_DIRECTORY`.

- **Authority.** `d` alone. The child is a plain, `send` capability like any Dir.
  Copying it copies the designation.
- **Failures.** `NotFound` when `p` is absent. `Denied` when `p` is not a valid path,
  leaves `d`, or names something other than a directory (a final symbolic link is not
  followed). `Other(errno)` for any other host error, for example `EMFILE`. On every
  failure nothing is opened.
- **Charging.** Nothing is admitted. The child's descriptor is run-owned, like the Dir that
  `create_temp` returns, and stays open until the run ends.
- **Replay.** The operation passes the replay point at issue (`d`'s ordinal and `p`) and at
  delivery (the new Dir's ordinal, or the error). On replay the child's operations are
  checked and supplied through that ordinal. A cancellation observed at delivery leaves
  nothing on the host to undo.

## Listing

```
pub type Kind = File | Directory | Symlink | Other
pub fn host.entries(d: Dir) -> Result[HostEntries, IoError] waits
pub fn host.next_entries(mut h: HostEntries, mut buf: Bytes) -> Result[u64, IoError] waits
pub type Entry { name: String, kind: Kind }
pub fn dir.entries(d: Dir) -> Result[HostEntries, IoError] waits
pub fn dir.next_entries(mut h: HostEntries, mut buf: Bytes) -> Result[u64, IoError] waits
pub fn dir.list(d: Dir) -> Result[List[Entry], file.Error] waits
```

A listing reads the directory a Dir is rooted at. To list a subdirectory, narrow to it
first, so a listing takes no path. `HostEntries` is a `send` mechanical resource, like
`HostFile` ([MEM-24](../../../docs/spec/budgets.md#mem-24-logical-charges)). Making one
admits nothing, and its finalizer closes it.

`host.next_entries` appends whole records to `buf`: a kind byte (File 0, Directory 1,
Symlink 2, Other 3), the name length as a little-endian `u16`, then the name bytes. It
returns the number of records appended, and `Ok(0)` once the listing is exhausted.
`.` and `..` are never listed. A record that does not fit stays for the next call. The
call fails with `Bounds` (a failure, checked before issue, changing nothing) when `buf`
has less than 258 bytes of spare capacity, which is one record with a 255-byte name, the
Linux `NAME_MAX`. Kinds come from the directory entry, and symbolic links are not
followed, as Go's `DirEntry.Type` does. The provider asks for a kind only when the host
reports it unknown. Entries are in host order. An entry created or removed during the
listing may or may not appear, as POSIX `readdir` allows.

`dir.list` reads every record and returns the entries sorted bytewise by name, as Go's
`os.ReadDir` does, so the result does not depend on the host's order. A name that is not
UTF-8 gives `Error(NotUtf8)`, because no `String` path can name it. The listing is not
silently shortened.

- **Authority.** `d` alone. A listing reveals the names directly in `d`'s root and
  nothing below them.
- **Failures.** `host.entries`: `Denied` when the root can no longer be read, and
  `Other(errno)`. `host.next_entries`: `Other(errno)`, after which `buf` is unchanged.
  `dir.list` adds `Capacity` and `NotUtf8`, and the entries built so far are cleaned and
  refunded (FAIL-5).
- **Charging.** The host operations admit nothing. Their bytes land in capacity the caller
  already holds. `dir.list` admits its record buffer (4096 bytes), the list and each name
  under the current limit, and refunds the buffer before it returns.
- **Replay.** `host.entries` passes the replay point at issue (`d`'s ordinal) and at
  delivery (the handle's ordinal). Each `host.next_entries` passes it at issue (the handle's
  ordinal and the spare capacity) and at delivery (the appended records). A replay
  supplies the recorded records, so it sees the recorded host order, and a replayed
  `dir.list` sorts them to the same result.

None of these operations waits after issue
([CONC-7](../../../docs/spec/tasks.md#conc-7-cancellation-points)).

## Metadata

```
pub type Stat { kind: Kind, size: u64, modified_ns: i64 }
pub fn host.stat(d: Dir, p: String) -> Result[Stat, IoError] waits
pub fn host.stat_file(mut f: HostFile) -> Result[Stat, IoError] waits
pub fn dir.stat(d: Dir, p: String) -> Result[Stat, IoError] waits
pub fn file.stat(mut f: HostFile) -> Result[Stat, IoError] waits
```

`stat` reports `p`'s kind, its size in bytes, and its modification time in nanoseconds
since 1970 (negative before 1970, as Go's `UnixNano`). It does not follow a final symbolic
link, the same as the kinds a listing reports. `host.stat_file` reports the open file
(`fstat`), so a `read_at` loop can learn the length of what it holds. The size of a
directory or a symbolic link is whatever the host reports. Permissions, ownership, inode
numbers and access or change times are not reported. Hirsel has no operation that acts on
them, and capability authority replaces permission bits as the access check.

- **Authority.** `d`, or the open file.
- **Failures.** `NotFound` when `p` is absent. `Denied` when `p` is not a valid path or
  leaves `d`. `Closed` for a closed handle. `Other(errno)`.
- **Charging.** Nothing is admitted.
- **Replay.** Issue: `d`'s or the file's ordinal, and `p`. Delivery: the three fields, or
  the error. A replay supplies the recorded times, so a replayed run sees the same
  `modified_ns`.
