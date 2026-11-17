# Time

[API overview](../API.md). Specified ahead of its module: the build lane moves each operation
block below into `core/std/time.hirsel` as that function's `///` contract and cuts this page
down to the module prose.

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
pub type Error: key = Invalid(u64) | Range | Capacity(Capacity)
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
  or a duration past `i64`.

## Dates

`date(year: u16, month: u8, day: u8) -> Option[Date]`
: The date, or `None` when `year > 9999`, `month` is not 1-12, or `day` is past the month's
  end (February has 29 days in years divisible by 4, except centuries not divisible by 400).

`year(d: Date) -> u16`, `month(d: Date) -> u8`, `day(d: Date) -> u8`
: The fields.

`days(d: Date) -> i64`
: Days since 1970-01-01, which is negative before it. 0000-01-01 is -719528 and
  9999-12-31 is 2932896. Uses Hinnant's `days_from_civil`. Cost: O(1).

`from_days(n: i64) -> Option[Date]`
: The inverse of `days`, or `None` outside -719528 to 2932896. Uses Hinnant's
  `civil_from_days`.

`weekday(d: Date) -> Weekday`
: 1970-01-01 is a Thursday.

`push_date(mut out: String, d: Date) -> Result[(), Capacity]` and
`parse_date(s: String) -> Result[Date, Error]`
: The RFC 3339 `full-date`, `YYYY-MM-DD`, with exactly 4, 2 and 2 digits. `parse_date`
  refuses anything else with `Invalid(i)`, and a well-formed but nonexistent date with
  `Range`. `push_date` reserves 10 bytes first and is atomic.

## Instants

`from_unix(secs: i64, nanos: u32) -> Option[Timestamp]`
: `None` when `nanos >= 10^9` or the instant is outside the range above.

`from_unix_ms(ms: u64) -> Option[Timestamp]`
: The bridge from `host.wall_ms` and `CallInfo.at`. `None` above 253402300799999.
  Ex: `let now = std.time.from_unix_ms(host.wall_ms(c))?`

`unix_secs(t: Timestamp) -> i64`, `subsec_nanos(t: Timestamp) -> u32`
: The fields.

`unix_ms(t: Timestamp) -> i64`
: Milliseconds since the epoch, rounded toward negative infinity, so
  `unix_ms(from_unix(-1, 999_999_999)) == -1`.

`add(t: Timestamp, d: i64) -> Option[Timestamp]`
: `t` plus `d` nanoseconds, or `None` outside the range.

`between(from: Timestamp, to: Timestamp) -> Option[i64]`
: `to - from` in nanoseconds, or `None` when that is outside `i64` (more than about 292
  years apart). It is negative when `to` is earlier.

## Civil time

`at(d: Date, hour: u8, minute: u8, second: u8, nanos: u32) -> Option[Civil]`
: `None` when a field is out of range as above.

`date_of(c: Civil) -> Date`, `hour(c)`, `minute(c)`, `second(c)`, `nanos(c)`
: The fields.

`civil(t: Timestamp, offset: i32) -> Option[Civil]`
: The wall-clock reading at `offset` minutes east of UTC. `None` when that reading falls
  outside years 0-9999. This can happen only within 24 hours of either end. Fails: Bounds
  when `offset` is out of range.

`timestamp(c: Civil, offset: i32) -> Option[Timestamp]`
: The instant at which a clock at `offset` reads `c`. `None` outside the range. Fails:
  Bounds as `civil`. `timestamp(civil(t, o)?, o) == Some(t)` for every `t` and `o`.

## RFC 3339

`parse_rfc3339(s: String) -> Result[(Timestamp, i32), Error]`
: RFC 3339 §5.6 `date-time`: `YYYY-MM-DD`, then `T` or `t`, then `hh:mm:ss`, an optional
  `.` with one or more digits, then `Z`, `z` or `±hh:mm`. The offset is returned with the
  instant so the text can be written back as it was. Rulings where RFC 3339 is loose:
  - Fraction digits past the ninth are read and dropped (Go).
  - Any `ss` of `60` reads as `59` and keeps its fraction (jiff), at any hour and offset,
    because the time line here has no leap seconds. RFC 3339 §5.7 places a leap second at
    23:59:60 UTC, which can be any local hour. This rule does not check that.
  - `-00:00` reads as offset 0. RFC 3339 §4.3 gives it the meaning "offset unknown", and this
    type has no way to hold that.
  - A space in place of `T` (RFC 3339 §5.6 NOTE, and ISO 8601's other forms) is `Invalid`.
  Fails: `Invalid(i)` for a byte the grammar refuses, a field with the wrong number of
  digits included. `Range` for a well-formed field out of its range: a nonexistent date,
  `hh > 23`, `mm > 59`, `ss > 60`, an offset hour above 23 or minute above 59, or an
  instant outside the range after the offset is applied. Cost: O(len).
  Ex: `let (t, off) = std.time.parse_rfc3339(s)?`

`push_rfc3339(mut out: String, t: Timestamp, offset: i32, digits: u8) -> Result[(), Error]`
: Writes `YYYY-MM-DDThh:mm:ss`, then exactly `digits` fraction digits after a `.` (none and
  no `.` when `digits` is 0; it truncates, never rounds), then `Z` for offset 0 or `±hh:mm`.
  Fixed width means the text sorts in time order for one offset. Fails: `Range` when
  `civil(t, offset)` is `None`, `Capacity(c)`, Bounds when `digits > 9` or the offset is out
  of range. Atomic: reserves the exact length first.
  Ex: `std.time.push_rfc3339(mut line, now, 0, 3)?` gives `2026-11-18T09:30:00.250Z`.

## Durations

`parse_duration(s: String) -> Result[i64, Error]`
: Go `time.ParseDuration` exactly: an optional sign, then one or more numbers each followed
  by a unit `ns`, `us`, `µs` (U+00B5), `μs` (U+03BC), `ms`, `s`, `m` or `h`. Numbers may have
  a fraction (`1.5h`, `.5s`, `5.s`). `0`, `+0` and `-0` alone need no unit. The fraction is
  converted with Go's binary64 expression, `truncate(f * (unit / scale))` (go 46b62495
  `src/time/format.go:1634-1731`, with `leadingFraction` at `:1580` dropping digits once
  the fraction would overflow and freezing `scale`). Every step is one correctly rounded IEEE operation, so
  the result is the same on every target. That is why `0.3333333333333333333h` is exactly
  20m, as in Go's table. Fails: `Invalid(i)` for a missing or unknown unit or a stray byte,
  and `Range` when the total leaves `i64`. `-9223372036854775808ns` is accepted.

`push_duration(mut out: String, d: i64) -> Result[(), Capacity]`
: Go `Duration.String` exactly (`time.go:947`): `0s`; below one second the largest of `ns`,
  `µs` (U+00B5) and `ms` that keeps the integer part nonzero, with a fraction stripped of
  trailing zeros (`1.5µs`); otherwise `h`, `m` and `s` fields with leading zero fields
  omitted (`1h2m3.004s`, `-2m3.4s`, `72h0m0s`). Atomic.

## Decisions

- **Durations: Go, ADOPTED unchanged**, grammar, float fraction expression and output
  included. Go's text is what operators write in configs (`30s`, `1h30m`). Matching Go makes
  its 43-row parse table, its error table and its `String` table complete oracles.
  An exact-rational fraction was considered and is NOT TAKEN: it differs from Go only past
  15 significant digits, and there it gives `0.3333333333333333333h` as `19m59.999999999s`.
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

## Laws (planned for the build lane, `core/std/tests/time.hirsel`)

- **T1 Days.** For about 40 dates (0000-01-01, 0000-02-29, 0000-12-31, 0001-01-01,
  1582-10-15, 1899-12-31, 1900-02-28, 1900-03-01, 1969-12-31, 1970-01-01, 2000-02-29,
  2024-02-29, 2100-03-01, 9999-12-31, and each era and century boundary), `days` equals
  Python `date(y, m, d).toordinal() - 719163` for years 1 and up. Year 0 is hand-derived
  from 0000-01-01 = -719528, 366 days long. `from_days(days(d)) == Some(d)` holds for
  them all. Oracle: CPython's `datetime`, produced by `core/tools/time_reference.py`, plus
  the hand derivation. Bug caught: year 0 treated as common, or 1900 as leap.
- **T2 Weekday.** 1970-01-01 Thursday, 2000-01-01 Saturday, 0000-01-01 Saturday,
  9999-12-31 Friday. Oracle: Python `weekday()` (years ≥ 1) and hand count. Bug caught:
  `%` on a negative day count giving a negative index.
- **T3 RFC 3339 parse.** The five RFC 3339 §5.8 examples, the `RFC3339` rows of Go
  `parseTests` (`format_test.go:274,283`) and `parseErrorTests` (`format_test.go:652-674`),
  and jiff's leap-second rows give the instant (or refusal) and offset each source states.
  Rejections with `Invalid(i)` offsets: a space separator, a 2-digit year, a missing seconds
  field, `.` with no digits, and a trailing byte. Rejections with `Range`: `24:00:00`,
  `+24:00`, Go's `21:00:67`, and `2023-02-29`. `12:00:60+05:00` reads as `12:00:59+05:00`.
  Oracle: the RFC text and the cited tables. Bug caught: the offset added rather than
  subtracted, so `1996-12-19T16:39:57-08:00` reads 16 hours off.
- **T4 RFC 3339 write.** Go `rfc3339Formats` (`format_test.go:77`, 3 rows) with `digits`
  0. Then `from_unix(0, 999_999_999)` at digits 3 is `1970-01-01T00:00:00.999Z`, at digits 0
  is `1970-01-01T00:00:00Z`, and at offset -1 is `1969-12-31T23:59:00.999-00:01`.
  `push_rfc3339` of the two range ends at offset 0 round-trips through `parse_rfc3339`.
  Oracle: Go's table and hand derivation. Bug caught: rounding the fraction (giving
  `00:00:01.000`), or using the UTC date at a non-zero offset.
- **T5 Duration parse.** Go `parseDurationTests` (`time_test.go:945`, 43 rows) and
  `parseDurationErrorTests` (`time_test.go:1016`). Rows holding invalid UTF-8 cannot be a
  `String` and are dropped. Error rows get hand-derived offsets. Oracle: Go's tables. Bug
  caught: exact fraction arithmetic (`0.3333333333333333333h`), or an overflow check that
  refuses `-9223372036854775808ns`.
- **T6 Duration write.** Go `durationTests` (`time_test.go:614`, 11 rows, used by
  `TestDurationString`), including `-1 << 63` and `1<<63 - 1`. Oracle: Go's table. Bug caught: `us` in place
  of U+00B5, or negating the minimum value.
- **T7 Bridges.** `from_unix_ms(253402300799999)` is the last millisecond and
  `from_unix_ms(253402300800000)` is `None`. `unix_ms` rounds toward negative infinity
  before the epoch. `between` of the two range ends is `None`. Oracle: hand-derived from the
  range above. Bug caught: `unix_ms` truncating toward zero, so a millisecond before the
  epoch maps to 0.
- **T8 Civil round trip.** For offsets -1439, -1, 0, 1 and 1439 and instants at both range
  ends, at day ends and in leap February, `timestamp(civil(t, o)?, o) == Some(t)`, and
  `civil` is `None` exactly where the reading leaves years 0-9999. Oracle: the invariant and
  hand derivation at the ends. Bug caught: floor division done as truncation for negative
  seconds, which gives 1969-12-31T23:59:59 as 1970-01-01T00:00:-1.

Prior vectors: `cc2376ee^:stdlib/std@1/{time,time_civil,date_time,date,duration,time_format}.hir`
and `c5690b37^:conformance/SEM/SEM-1-duration-go{,-errors}-run.hir`,
`SEM-1-time-jiff-temporal-run.hir`, `SEM-1-time-date-*-run.hir` and
`SEM-1-time-offset-run.hir`. The old duration module used i128 nanoseconds, so Go's seven
overflow rows passed there. Here they are `Range` again, as in Go. The old strtime rows are
out of scope.
