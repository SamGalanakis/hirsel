# UTF-8 source

`rust_utf8_admission` adapts `from_utf8_error` and `test_str_from_utf8`
from [Rust's string tests](https://github.com/rust-lang/rust/blob/a7c37f9f0ee5e93396106e9de20f4a86b4154e63/library/alloctests/tests/str.rs),
commit `a7c37f9f0ee5e93396106e9de20f4a86b4154e63`, MIT.
`string.from_utf8` checks admission and returns the original bytes on
refusal. Rust's cases exercise malformed prefixes of different lengths.
