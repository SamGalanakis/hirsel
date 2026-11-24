# Path pattern compilation

[Path pattern grammar and matching](path.md#patterns). [Glob](dir.md#glob).

`compile` validates a pattern and records the positions used by the matcher.
`base` returns the byte length of the leading literal path in the source.
`test` reports whether a path matches and whether a descendant can match.
`path.matches` and `dir.glob` share these operations.
