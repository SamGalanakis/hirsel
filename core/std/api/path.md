# Path

[API overview](../API.md). [Operations and contracts](../path.hirsel).

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
pub type Error = Invalid(u64)
```

`Invalid(i)` gives the byte offset in the argument where the path stops being valid: the start
of an empty, `.` or `..` component, or a NUL byte. For an empty argument it is 0.

The text functions `parent`, `name`, `extension`, `stem` and `strip_base` read bytes and do not
check validity. On other text they still follow the same byte rules and never fail.
Validation walks components with `string.next_split(p, at, "/")`.

## Decisions

- **The host's grammar, not a general path model.** Go `path` (go 46b62495
  `src/path/path.go`) and Rust `std::path` both model absolute paths, `.` and `..`, and
  repeated separators. Rust also models platform prefixes. Hirsel paths have none of these.
  A program builds names inside a `Dir`, and the rule is core's. Rust's
  `Path::extension`/`file_stem` dot rules are ADOPTED for valid paths
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

## Laws

[Contract tests](../tests/path.hirsel).

- **P1 Host differential.** For each row in a table of about 30 strings, `is_valid(p)` is
  true exactly when `file.write(sub, p, ..)` (with parents created) does not return
  `Denied`. `sub` is a fresh subdirectory per row, so no row meets an earlier row's file.
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
