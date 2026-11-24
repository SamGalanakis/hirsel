# Regular expressions

`std.regex` implements the syntax of Rust's `regex` crate for UTF-8 strings. Compile patterns with `new`. Each search borrows a separate mutable cache. Use `find` for the first span, `cursor` and `next` for iteration, or `groups` and `captures` for capture spans. Searches need no byte admission after the cache and groups are made.

Use `unicode_property_message(pattern, e)` to report an unsupported Unicode
property by name. Pass the same pattern used to compile the regex.

[Regex specification](../../../docs/spec/regex.md)

`replace(re, mut cache, text, replacement)` replaces all matches in cursor
order. Use `$0` for the entire match, `$1` for a numbered capture, `$name` for
a named capture, and `$$` for a literal dollar sign. Unbraced references consume
the longest run of ASCII letters, digits and underscores. Use braces to end a
reference explicitly or to reference a Unicode name. An unknown or unmatched
capture contributes an empty string.

`split(re, mut cache, text)` returns the owned pieces between matches. It
includes empty pieces at the ends and between adjacent matches. Both operations
use the cursor's UTF-8 boundary and empty-match rules. They allocate output
under the current byte limit and can raise Capacity.
