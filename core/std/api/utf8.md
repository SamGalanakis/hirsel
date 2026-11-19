# UTF-8

[API overview](../API.md). [Operations and contracts](../utf8.hirsel).

`std.utf8.validate(bytes)` checks a byte span without allocating. An error's
`valid_up_to` is the byte offset of the first invalid or unfinished scalar.
`error_len` is `Some(n)` for an invalid scalar. The length includes its lead
byte and any valid continuation bytes before the failure. `None` means the
span ends in a valid but unfinished scalar. A streaming caller can retain
the suffix at `valid_up_to` and resume validation with the next fragment.

The scalar ranges come from [RFC 3629 §4](https://www.rfc-editor.org/rfc/rfc3629#section-4).
The error fields follow [Rust's `Utf8Error`](https://doc.rust-lang.org/std/str/struct.Utf8Error.html).
