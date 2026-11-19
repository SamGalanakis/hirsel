# Regular expressions

`std.regex` implements the syntax of Rust's `regex` crate for UTF-8 strings. Compile patterns with `new`. Each search borrows a separate mutable cache. Use `find` for the first span, `cursor` and `next` for iteration, or `groups` and `captures` for capture spans. Searches need no byte admission after the cache and groups are made.

[Regex specification](../../../docs/spec/regex.md)
