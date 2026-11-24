# SemVer

`std.semver.parse` accepts the [SemVer 2.0.0 grammar](https://semver.org/spec/v2.0.0.html).
`std.semver.format` returns the complete version, including build metadata.
`std.semver.ordered` compares precedence and ignores build metadata. Numeric
components are compared as decimal text, so precedence works beyond u64.

`std.semver.requirement` accepts `^`, `~`, `=`, `>`, `>=`, `<` and `<=`.
A bare version is a caret requirement. Caret holds the prefix through the
first nonzero component fixed, or all supplied components when they are zero.
Tilde holds the major and, if supplied, the minor fixed. Both follow
[Cargo's version requirements](https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html#version-requirement-syntax).
For example, `^0.2.3` admits `0.2.9` and excludes `0.3.0`, while `~1.2`
admits `1.2.9` and excludes `1.3.0`.

Commas join comparators within an alternative. `||` joins alternatives.
Spaces and tabs may surround operators and separators. A pre-release can
match only if its alternative includes a comparator with a pre-release at
the same major, minor and patch. Comparator build metadata is ignored.

Each input is bounded to 4096 bytes, with no nesting. Parsers return `SemVerTooLong`
above the byte bound or `SemVerSyntax` with a byte offset for malformed input.
