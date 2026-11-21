# Config

[API overview](../API.md). [Types and operation contracts](../config.hirsel).

Use `std.config` to read startup configuration from an explicit
[`Env`](env.md). Read keys in declaration order, then call `finish` before
using the values. A failed read records the requested key and its problem, so
the remaining getters can run in the same pass.

```hirsel
let mut settings = std.config.new(e, 3)
let address = std.config.string_or(mut settings, "APP_ADDRESS", "127.0.0.1")
let port = std.config.port(mut settings, "APP_PORT")
let timeout = std.config.duration_ms_or(mut settings, "APP_TIMEOUT_MS", 5000)
match std.config.finish(take settings) {
    Ok(()) => start(address, port, timeout),
    Error(errors) => report(std.config.format_errors(errors)),
}
```

Pass the number of getter calls to `new` to retain details for every read
that fails. With a smaller bound, the reader keeps the first errors and
counts the rest. `finish` consumes the reader and puts one `More(n)` entry
after the retained errors when the count is positive. It returns `Ok(())`
exactly when no read failed, including when the bound is zero.

## Values and defaults

| Getter | Accepted value | Placeholder on error |
|---|---|---|
| `string` | UTF-8, including an empty value | Empty string |
| `u64` | Unsigned 64-bit decimal integer | `0` |
| `i64` | Signed 64-bit decimal integer, with an optional leading minus | `0` |
| `bool` | Exactly `true` or `false` | `false` |
| `port` | Decimal integer from 1 through 65535, returned as `u16` | `0` |
| `duration_ms` | Unsigned 64-bit decimal millisecond count | `0` |

Numeric input is parsed as written. Whitespace, a leading plus, prefixes,
separators and unit suffixes are invalid. Each `_or` getter returns its
caller-supplied fallback only for an authorized but absent key. Denied,
non-UTF-8 and invalid reads still record an error and return the placeholder.

## Reporting and secrets

`KeyError(key, problem)` distinguishes `Missing`, `Denied`, `NonUtf8` and
`Invalid(String)`. The invalid diagnostic describes the expected format.
`format_errors` joins the key and diagnostic for each error with newlines in
getter call order. It renders `More(n)` as `and N more` and includes no
environment values.

Keys are read directly through `Env`. Keep credentials in `Secret` grants,
which the launcher prevents from overlapping with an `Env` grant.
[Contract tests](../tests/config.hirsel).
