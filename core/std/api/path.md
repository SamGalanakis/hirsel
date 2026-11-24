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

## Patterns

*Specified 2026-11-24 (std Group 2), not yet implemented.*

```
pub type PatternError = Syntax(u64)
pub fn path.matches(pattern: String, p: String) -> Result[bool, PatternError]
```

`matches` reports whether the path `p` matches `pattern`. It is pure, like the rest of this
module. [`dir.glob`](dir.md#glob) applies it to a walk.

**Grammar.** A pattern is components joined by `/`, as a valid path is. Within a
component:

| Pattern | Matches |
|---|---|
| `*` | any run of bytes, possibly empty, within one component |
| `?` | exactly one Unicode scalar, never `/` |
| `[abc]`, `[a-z]` | one scalar in the class. Ranges compare scalar values. |
| `[!abc]`, `[^abc]` | one scalar not in the class |
| `\c` | the character `c` itself, including `*`, `?`, `[`, `]` and `\` |
| any other character | itself |

A component that is exactly `**` matches zero or more whole components, so `a/**/b`
matches `a/b` and `a/x/y/b`, and `**` alone matches every path. Metacharacters match a
leading `.` like any other byte, as in Go and Rust `glob`'s default.

`Syntax(i)` gives the byte offset where the pattern stops being valid:

- an empty, `.` or `..` component, or a NUL, because no valid path could match it;
- `**` that is not a whole component (`a**`, `**b`);
- an unclosed `[`, a range whose end is below its start, or a `/` inside a class. A `]`
  right after `[`, `[!` or `[^` is a member, not the end, as POSIX has it and as Go's
  matcher does (`match.go:148`), so `[]a]` is valid and no class is empty;
- a `\` at the end of the pattern.

The whole pattern is checked before any matching, so a bad pattern is an error whatever
the path, as Go's `path.Match` also ensures (`match.go:78`). `matches` does not check `p`. It matches bytes component by component, so a `p` that is
not a valid path matches only as its bytes do.

**Algorithm and cost.** Components are matched by a position-set simulation (Thompson's
construction, as `std.regex` runs), with no backtracking. Within a component, the set of
live pattern positions advances once per scalar of the path. Across components, the set of
live pattern components advances once per path component, and `**` keeps its position
live. Each byte of `p` is therefore met at most once per pattern position, and steps are
O(len(pattern) × len(p)) whatever the arrangement of `*` and `**`. The usual worst case for
a backtracking matcher, such as `a*a*a*a*b` against `aaaa…`, stays linear in each factor.
`matches` admits two position sets of O(len(pattern)) bytes under the current limit and
refunds them before it returns.

**Prior art.** Go `path.Match` (`/tmp/ref-go`, go 8eaf19e, `src/path/match.go:37`,
`ErrBadPattern` at `match.go:14`) is the base grammar: `*`, `?`, classes with `^`, and `\`
escapes. ADOPTED. Python's `fnmatch` (`/tmp/ref-cpython`, cpython 32b51e3,
`Lib/fnmatch.py`) negates with `!`. ADAPTED: both `!` and `^` negate, so a shell user's and
a Go user's patterns both work. Rust `glob` (`/tmp/ref-rustglob`, glob 057cef2,
`src/lib.rs:532`) gives `**` its whole-component meaning and refuses `**` inside a
component (`lib.rs:603-604`). ADOPTED. Python `glob(..., include_hidden=False)`
(`Lib/glob.py:16-17`) hides dot files from wildcards. NOT TAKEN: Go and Rust match them,
and a rule about leading dots belongs to `.gitignore`-style packages. Go's `path.Match`
takes the leftmost match of each chunk after a `*` and never revisits it. That is
correct within one component, but `**` across components needs alternatives kept open,
and a backtracking matcher for them can be exponential. REPLACED by the simulation above
for both levels. Goose (`/tmp/ref-goose`, d5ae5bb) has no path
pattern matcher; its nearest is `matches` in `stdlib/regex.goose:1027`, a regex match.
NOT TAKEN: a regex is not a path pattern, and a glob must not cross `/` except by `**`.

## Left out

Absolute paths, `..` resolution, normalization (`clean`), Windows separators and prefixes,
`.gitignore` semantics (negation, anchoring, directory-only rules), brace expansion
(`{a,b}`), `is_hidden`, `with_extension` (`stem` and two `string.push`es), and any OS-path
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
