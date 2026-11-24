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
first, so a listing takes no path. (Group 2 replaces this rule: a listing takes
`at: Option[String]`, see [Listing below a path](#listing-below-a-path).) `HostEntries` is a `send` mechanical resource, like
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
  `dir.list` also returns `NotUtf8` for an invalid name. A refused allocation
  raises `Capacity`. Cleanup closes the listing and refunds its allocations (FAIL-5).
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

## Listing below a path

*Specified 2026-11-24 (std Group 2), not yet implemented. It replaces the listing
signatures above.*

```
pub fn host.entries(d: Dir, at: Option[String]) -> Result[HostEntries, IoError] waits
pub fn dir.entries(d: Dir, at: Option[String]) -> Result[HostEntries, IoError] waits
pub fn dir.list(d: Dir, at: Option[String]) -> Result[List[Entry], file.Error] waits
```

`None` lists `d`'s root, as today. `Some(p)` lists the existing directory `p` below `d`, with
`host.open_dir`'s confinement and refusals: `NotFound` when `p` is absent, and `Denied` when
`p` is not a valid path, leaves `d`, or names something other than a directory (a final
symbolic link is not followed). The listing's descriptor belongs to the `HostEntries` and
closes with it. That is the reason for the change. Narrowing to each directory first
would leave one run-owned descriptor open per directory (Narrowing, above), and a walk of
a large tree would exhaust the process's descriptors. Authority, charging and replay are
the listing's. Replay adds `p` to the issue record, and the provider's `DirOp` becomes
`Entries(dir: u64, path: u64)`, where a path length of 0 means `None`, because a valid path
is never empty.

## Walk

*Specified 2026-11-24 (std Group 2), not yet implemented.*

```
pub opaque type Walker: send
pub type WalkError { path: String, error: file.Error }
pub fn dir.walk(d: Dir, at: Option[String]) -> Walker
pub fn dir.next(mut w: Walker) -> Result[Option[Entry], WalkError] waits
pub fn dir.skip(mut w: Walker)
```

```hirsel
let mut w = std.dir.walk(d, Some("src"))
loop {
    match std.dir.next(mut w) {
        Ok(Some(e)) => if e.kind == Directory && std.path.name(e.name) == ".git" { std.dir.skip(mut w) } else { visit(e) },
        Ok(None) => break,
        Error(problem) => report(problem),
    }
}
```

A walk visits every entry below `at` (or below `d`'s root), depth first and in preorder:
a directory is returned before its contents. The entries of each directory come sorted
bytewise by name, as `dir.list` returns them. This is Go's `fs.WalkDir` order: `a`,
`a/x`, `a.txt`. It is a total order fixed by the tree's names, so it does not depend on the
host's listing order. `Entry.name` is the path relative to `d`, with `at` as its first
components, so it can be passed straight to `file.open(d, e.name, ..)`. The starting
directory itself is not returned.

- **Pull, not callback.** `next` returns one entry per call, like `next_entries`, so the
  caller's loop body may wait, return early or keep state with no callback rules.
  `walk` makes no host call. The first `next` lists the start.
- **Descending.** When `next` returns a `Directory`, the following `next` lists it and
  continues inside it, unless the caller called `skip` in between. `skip` after any other
  kind, or twice, changes nothing. `skip` makes no host call, takes O(1) steps, and refunds
  the skipped directory's buffered entries, if any were listed.
- **Symbolic links.** A walk never follows one. A link is returned with kind `Symlink`
  and never descended, whatever it points to, so a walk cannot loop and cannot leave `d`.
  (A listing's kinds do not follow links, Listing above.) A directory replaced by a link
  between being returned and being listed gives `WalkError(path, Io(Denied))`, because
  `host.entries` does not follow a final link.
- **Errors.** A directory whose listing fails returns `Error(WalkError(path, e))`, where
  `path` is that directory's name relative to `d` and `e` is the listing's error,
  including `NotUtf8` for a name that is not UTF-8. None of its entries is returned, and
  the next `next` goes on with its next sibling. So one unreadable directory is reported
  and the rest of the tree is still walked, as with Go's `WalkDir` given a callback that
  returns nil. A failure of the start itself is returned once, and then `next` returns
  `Ok(None)`. Nothing is skipped silently.
- **Concurrent change.** The tree is not a snapshot. An entry created or removed while the
  walk runs may or may not appear. A directory removed after it was returned gives
  `WalkError(path, Io(NotFound))` when its turn comes.

- **Authority.** `d` alone. A walk reveals names below `at` and nothing outside `d`.
- **Failures.** As above. A refused allocation raises `Capacity`, and cleanup closes any
  open listing and refunds the walker's storage (FAIL-5).
- **Charging.** The walker holds the path of the directory it is in and, for each
  directory on that path, the sorted entries not yet returned. Each directory is listed
  whole (as `dir.list` does) and its listing closed before any entry is returned, so at
  most one `HostEntries` and one 4096-byte record buffer are live at a time. Memory is
  admitted under the limit current at each `next` call, and is at most the sum, over the
  directories on the current path, of the bytes of their entries. It is refunded as each
  directory is finished. Steps: listing a directory of k entries and b name bytes takes
  O(b + k log k) (the sort), and each `next` takes O(1) more, so a whole walk of n entries
  takes O(total name bytes + n log n), plus one step per host call (MEM-30).
- **Replay.** Each listing's host calls pass their replay points. A replay supplies the
  recorded records, and the sort gives the same order.

## Glob

*Specified 2026-11-24 (std Group 2), not yet implemented.*

```
pub type GlobError = Pattern(path.PatternError) | Walk(WalkError)
pub fn dir.glob(d: Dir, pattern: String) -> Result[List[String], GlobError] waits
```

`dir.glob(d, pattern)` returns the paths below `d` that `path.matches(pattern, p)` accepts
([Path, Patterns](path.md#patterns)), in walk order. It matches files, directories and
symbolic links alike, as Go's `filepath.Glob` does.

- **Pattern first.** The pattern is checked whole before any host call, and a bad one is
  `Pattern(e)`.
- **Where it walks.** The leading components with no metacharacter form the base. A
  pattern with no metacharacter at all is a `dir.stat` of that path, giving `[pattern]`
  or `[]`. Otherwise glob walks from the base and skips (with `skip`) every directory
  below which no path can match. Without `**`, that is any directory as deep as the
  pattern has components. With `**`, it is any directory whose path cannot match a prefix
  of the pattern's components. So `src/*.rs` lists one directory, and `**/x` walks the
  whole tree.
- **Errors.** A base that does not exist gives `Ok([])`. Any other walk error ends the
  glob with `Walk(e)`. A partial list is never returned as if complete.

- **Authority.** `d` alone.
- **Charging.** The result list and its paths are admitted under the current limit,
  plus the walk's own storage. Steps: the walk's, plus one `path.matches` per entry
  visited, which takes O(len(pattern) × len(entry path)) (Path, Patterns).
- **Replay.** The walk's.

### Prior art

- Go `io/fs.WalkDir` (`/tmp/ref-go`, go 8eaf19e, `src/io/fs/walk.go`) walks in lexical
  order and has `SkipDir` (`walk.go:15`). ADOPTED: the order, preorder, not following
  links, and per-directory errors that do not stop the walk. REPLACED: the callback with
  a pull iterator, and `SkipDir` as a returned sentinel with `skip`.
- Rust `walkdir` (`/tmp/ref-walkdir`, walkdir c00aa73, `src/lib.rs`): `follow_links`
  (`lib.rs:348`), `sort_by` (`lib.rs:419`), `contents_first` (`lib.rs:519`) and
  `skip_current_dir` (`lib.rs:820`). ADOPTED: the iterator with `skip_current_dir`.
  NOT TAKEN: following links, `max_depth` (a caller calls `skip`) and caller-chosen sort
  orders. The order is fixed, so every program walks a tree the same way.
- Python `os.walk` (`/tmp/ref-cpython`, cpython 32b51e3, `Lib/os.py:318`) yields per
  directory, in host order unless the caller sorts. REPLACED: entries come one at a time
  and in a fixed order.
- Python `glob.glob` (`Lib/glob.py:16`) and Rust `glob` (`/tmp/ref-rustglob`, glob
  057cef2): see [Path, Patterns](path.md#patterns) for the pattern language. Go's
  `filepath.Glob` ignores I/O errors. REPLACED: an error here ends the glob, because a
  silently shortened list is a wrong answer.
- Goose (`/tmp/ref-goose`, goose d5ae5bb, `stdlib/os.goose`) has `list_dir`, sorted
  bytewise, and no walk or glob. ADOPTED: the bytewise order.

### Open

- **`host.entries` takes a path.** This changes a core primitive and its four callers,
  plus `dir.list`'s five. The alternative was a closeable narrowed `Dir`. A `Dir` is
  plain and copyable, so it has no single owner to close it, and the change would reach
  CAP-7. The change is reversible until a release pins the signature.
