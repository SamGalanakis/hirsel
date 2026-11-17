# Path

[API overview](../API.md). Specified ahead of its module: the build lane moves each operation
block below into `core/std/path.hirsel` as that function's `///` contract and cuts this page
down to the module prose.

`std.path` is a set of pure functions over the path text that a `Dir` accepts. It grants no
authority and does no I/O. A path means the same thing to it as to the host: core's **valid
path** ([core.md](../../../docs/spec/core.md), Host) is one or more components joined by
`/`, where no component is empty, `.` or `..`, and no byte is NUL. Such a path is never
absolute and never names the `Dir` itself. Confinement is the provider's job. Every Host
operation refuses an invalid path or an escape with `Denied` and has no effect. This module
lets a program check, build and take apart names before it asks. Its typical use is to check
a name from a request (an upload's file name, a key from a URL) before the program builds a
path from it. Operations allocate nothing except `join`, which grows a caller-owned String.

## Types

```
pub type Error: key = Invalid(u64) | Capacity(Capacity)
```

`Invalid(i)` gives the byte offset in the argument where the path stops being valid: the start
of an empty, `.` or `..` component, or a NUL byte. For an empty argument it is 0.

## Operations

`is_valid(p: String) -> bool`
: `true` exactly when `p` is a valid path. Cost: O(len). Ex:
  `if !std.path.is_valid(key) { return Error(BadRequest) }`

`check(p: String) -> Result[(), Error]`
: `is_valid` with the offset of the first fault: `Invalid(i)` as above. Cost: O(len).

`is_name(c: String) -> bool`
: `true` exactly when `c` is a valid path of one component: not empty, not `.` or `..`, and
  free of `/` and NUL. This is the check for a file name that arrives from outside the program.
  It is the same test `host.mkdtemp` applies to its name (core.md, Host).

`join(mut out: String, p: String) -> Result[(), Error]`
: Appends `p` to the path in `out`. It writes `/` between them when `out` is not empty, and
  writes `p` alone when `out` is empty. `p` may have several components, and each must be
  valid. Fails: `Invalid(i)` (offset in `p`) when `p` is not a valid path, or
  `Capacity(c)`. Either way `out` is unchanged, because the check and the exact `reserve`
  both come before the first byte is written. Alloc: GROWTH of `out`. Cost: O(len p). `join`
  does not check `out`. A path built only by `join`s from an empty String is valid by
  construction. Ex: `std.path.join(mut p, user)?; std.path.join(mut p, "inbox")?`

`parent(p: String) -> Option[View[String]] in p`
: The bytes before the last `/`, or `None` when `p` has no `/`. Ex: the parent of `a/b/c` is
  `a/b`, and `c` has no parent.

`name(p: String) -> View[String] in p`
: The bytes after the last `/`, or all of `p`. Ex: `a/b.txt` gives `b.txt`.

`extension(p: String) -> Option[View[String]] in p`
: The bytes of `name(p)` after its last `.`. `None` when the name has no `.`, or when its only
  `.` is the first byte. Ex: `a.tar.gz` gives `gz`, `.bashrc` gives `None`, `a.` gives the
  empty view, and `.config.toml` gives `toml` (Rust `Path::extension`).

`stem(p: String) -> View[String] in p`
: `name(p)` without the `.` and extension, or the whole name when `extension` is `None`. Ex:
  `a.tar.gz` gives `a.tar`, `.bashrc` gives `.bashrc`, and `a.` gives `a` (Rust
  `Path::file_stem`).

`strip_base(p: String, base: String) -> Option[View[String]] in p`
: The rest of `p` after `base` and one `/`, compared by whole components. `None` when `base`
  is not a leading run of `p`'s components, or when `p` equals `base` (no valid rest).
  Ex: `strip_base("a/b/c", "a")` gives `b/c`, and `strip_base("ab/c", "a")` gives `None`,
  where `string.strip_prefix` would give `b/c`.

The text functions `parent`, `name`, `extension`, `stem` and `strip_base` read bytes and do not
check validity. On a valid path they give valid paths or names. On other text they still
follow the byte rule above and never fail. Components are walked with
`string.next_split(p, at, "/")`. This module adds no second walker.

## Decisions

- **The host's grammar, not a general path model.** Go `path` (go 46b62495
  `src/path/path.go`) and Rust `std::path` both model absolute paths, `.` and `..`, and
  repeated separators. Rust also models platform prefixes. Hirsel paths have none of these.
  A program builds names inside a `Dir`, and the rule is core's. Rust's
  `Path::extension`/`file_stem` dot rules are ADOPTED unchanged
  (`library/std/src/path.rs:334 rsplit_file_at_dot`, rust 5f4effd5), and their doc examples
  are the oracle. Go's `path.Ext` (which gives `.bashrc` as the extension of `.bashrc`) is NOT
  TAKEN. A leading dot marks a hidden file, not an extension.
- **No `clean`.** Go's `path.Clean` (`path.go:72`) removes `a/..` lexically. That is only
  equal to traversal when `a` is not a symbolic link. Inside a `Dir`, `..` is not valid
  anyway, so a cleaner could only turn a path the host would refuse into a different path
  that it accepts. That is NOT TAKEN. cap-std (b211211 `cap-primitives/src/fs/manually/open.rs`)
  resolves `..` during traversal against the real tree, which is what the provider's
  `openat2(RESOLVE_BENEATH)` already does.
- **`join` refuses rather than escapes.** Rust `PathBuf::push` with an absolute argument
  replaces the whole buffer. That makes `base.join(user_input)` a well-known traversal bug.
  Go `path.Join` cleans `..` away silently. `join` here is atomic and refuses any component
  the host would refuse (REPLACED).
- **Goose** has no path module. "One function per job" (Goose `docs/design/stdlib_design.md`
  §1 "Small") is why components are walked with `string.next_split` and there is no
  `components` iterator here (ADOPTED).

## Left out

Absolute paths, `..` resolution, normalization (`clean`), Windows separators and prefixes,
globbing, `is_hidden`, `with_extension` (`stem` and two `string.push`es), and any OS-path
conversion. A path to a file outside every `Dir` is not a value a program can use.

## Laws (planned for the build lane, `core/std/tests/path.hirsel`)

- **P1 Host differential.** For each row in a table of about 30 strings, `is_valid(p)` is
  true exactly when `file.write(dir, p, ..)` (with parents created) does not return `Denied`.
  The table holds the empty string, `/`, `a/`, `/a`, `a//b`, `.`, `..`, `a/./b`, `a/../b`,
  `...`, `.a`, `a.`, a NUL in the middle, `é`, and a 255-byte component. Oracle: the
  provider (the host's own `Denied`), run as an `f(dir)` test. Bug caught: `...` refused as
  if it were `..`, or a trailing `/` accepted.
- **P2 Fault offsets.** `check` gives `Invalid(0)` for `""` and `/a`, `Invalid(2)` for
  `a//b` and `a/./b`, `Invalid(2)` for `a/..`, and the NUL's offset for a NUL. Oracle:
  hand-derived from the grammar. Bug caught: the offset points at the `/` before the bad
  component rather than at its start.
- **P3 Atomic join.** `join` into `out = "x"` with `p` = `a/../b` leaves `out == "x"`. The
  same with a capacity-limited allocator at exactly one byte short also leaves `out == "x"`.
  Oracle: the contract. Bug caught: writing the separator before validating `p`.
- **P4 Rust dot rules.** The examples in Rust's `Path::extension`, `file_stem` and
  `file_prefix` docs (path.rs:2880-2975) that apply, plus `a.`, `.a.b` and `..a`. Oracle: the
  Rust doc text. Bug caught: `.bashrc` given extension `bashrc` (Go's rule), or a `rsplit`
  at the first dot.
- **P5 Go split tables.** Go `splittests`, `basetests` and `dirtests` (`path_test.go:100,168,191`),
  limited to rows that are valid paths. They are mapped to `parent` and `name`, and the
  deviations are listed in the test comment (Go's `Dir("a")` is `.`; here it is `None`).
  Oracle: Go's tables. Bug caught: `parent` returning the trailing `/`.
- **P6 Component-wise strip.** `strip_base("ab/c", "a")` is `None`, `strip_base("a/b", "a/b")`
  is `None`, and `strip_base("a/b/c", "a/b")` is `c`. Oracle: hand-derived. Bug caught: a
  byte-prefix implementation.

Prior vectors: `771885fb^:stdlib/std@1/path.hir` and
`c5690b37^:conformance/SEM/SEM-3-path-go-run.hir` and `SEM-3-pathtime-path-run.hir`. The old
module modelled Go's `Clean`. Keep its rows for `name`, `parent` and `extension` where the
input is a valid path.
