# Time

[API overview](../API.md). [Operations and contracts](../time.hirsel).

`std.time` is calendar arithmetic and timestamp text: instants on the UTC time line, civil
dates and times at a fixed UTC offset, RFC 3339 text, and Go's duration text. It has no
capability and reads no clock. An instant comes from `host.wall_ms(c)` or `CallInfo.at`
through `from_unix_ms`, or from parsed text. It knows no time zones. An offset is a number
of minutes that the caller supplies or that the text carries. Every operation is pure and
allocates nothing except the `push_*` appenders, which grow a caller-owned String.

## Types

```
pub opaque type Timestamp: key { secs: i64, nanos: u32 }
pub opaque type Date: key { year: u16, month: u8, day: u8 }
pub opaque type Civil: key { date: Date, hour: u8, minute: u8, second: u8, nanos: u32 }
pub type Weekday: key = Monday | Tuesday | Wednesday | Thursday | Friday | Saturday | Sunday
pub type Error = Invalid(u64) | Range
```

- `Timestamp` is an instant: seconds since 1970-01-01T00:00:00Z, ignoring leap seconds as
  POSIX time does, plus `nanos < 10^9`. Its range is **0000-01-01T00:00:00Z through
  9999-12-31T23:59:59.999999999Z**, which is `secs` from -62167219200 to 253402300799. That
  is the range RFC 3339 text can write. The derived order is time order.
- `Date` is a proleptic Gregorian date with year 0 to 9999. Year 0 is 1 BCE, as in ISO 8601
  and RFC 3339. Every value is a real date, so there is no 2023-02-29. Order is
  chronological.
- `Civil` is a date and a time of day: `hour < 24`, `minute < 60`, `second < 60`,
  `nanos < 10^9`. It has no offset. It means a reading on some wall clock, and becomes an
  instant only with one.
- A **duration** is an `i64` count of nanoseconds, as Go's `time.Duration`: about ±292
  years. No wrapper type is used. Arithmetic on durations is `int`'s checked arithmetic.
- An **offset** is an `i32` count of minutes east of UTC, `-1439 ..= 1439`, as RFC 3339
  writes `±hh:mm`. Functions that take one fail Bounds outside that range: an offset is
  either parsed (so already in range) or written by the program.
- `Invalid(i)` gives the byte offset of the first byte the grammar refuses. `Range` means
  the text is well formed but names a value outside its type, such as 2023-02-29, hour 24,
  or a duration past `i64`. A refused allocation raises Capacity rather than returning it.

## Decisions

- **Durations: Go, ADAPTED.** The grammar, float fraction expression and output
  follow Go. Go's text is what operators write in configs (`30s`, `1h30m`). Matching Go makes
  its 43-row parse table, its error table and its `String` table complete oracles.
  An exact-rational fraction was considered and is NOT TAKEN: it differs from Go only past
  15 significant digits, and there it gives `0.3333333333333333333h` as `19m59.999999999s`.
  The sum checks the remaining magnitude before adding. This rejects a pair of
  2^63-nanosecond terms with `Range`, where Go's unsigned sum wraps to zero.
  A syntax error after an overflowing number or term still reports `Invalid(i)`,
  since Hirsel reserves `Range` for well-formed text.
- **RFC 3339 parse: Go's grammar, jiff's leap second.** Go `parseRFC3339`
  (`format_rfc3339.go:82-153`) refuses second 60. That makes a real RFC 3339 §5.8 example,
  `1990-12-31T23:59:60Z`, unreadable. jiff (0.2.43 `src/fmt/temporal/parser.rs:813-827`)
  clamps 60 to 59. That is ADAPTED; jiff also accepts many Temporal forms (expanded years,
  basic format, annotations) that are NOT TAKEN. Go's truncation of fraction digits past 9
  is ADOPTED. jiff refuses them, which fails input from systems that write picoseconds.
- **Day arithmetic: Hinnant's algorithms ADOPTED** (H. Hinnant, "chrono-Compatible
  Low-Level Date Algorithms", `days_from_civil` and `civil_from_days`). These are what jiff
  and libc++ use: O(1), and branch-free over 400-year eras.
- **The range is the RFC 3339 range.** Go's `Time` spans ±292 billion years. jiff spans years
  -9999 to 9999. Here every `Timestamp` has a text form and every text form has a
  `Timestamp`. No `Range` error can come from `push_rfc3339` except through an offset at the
  very ends. Years 10000 and above are refused (REPLACED).
- **No time zones.** IANA zones need a tzdb that changes several times a year. That is data
  plus a capability, not a pure library. Programs store UTC and an offset, and render with
  `civil` (NOT TAKEN; a later zone library builds on `Civil`).
- **Goose** has no time module. The caller-owned-output convention is followed (ADOPTED).

## Left out

Time zones and the tzdb, month and year arithmetic (`add_months`: the day-overflow rule is a
policy, not a fact), strftime-style patterns, HTTP-date (RFC 9110 §5.6.7, which belongs to
`http1`), ISO 8601 durations (`PT1H`), ISO week dates, ordinal dates, a monotonic-clock type
(`host.mono_ns` is already a `u64` of nanoseconds), and leap-second tables.
