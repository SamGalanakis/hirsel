# Cron

`std.cron.parse` reads five fields separated by spaces or tabs.

| Field | Values |
|---|---|
| Minute | 0–59 |
| Hour | 0–23 |
| Day of month | 1–31 |
| Month | 1–12 or Jan–Dec |
| Day of week | 0–7 or Sun–Sat. Both 0 and 7 mean Sunday. |

Each field accepts comma-separated entries, inclusive ascending ranges and
`*`. A positive `/step` selects entries starting from the beginning of a
range. A number followed by `/step` extends through the field maximum. Use
three-letter names in any ASCII case. The supported aliases are `@yearly`,
`@monthly`, `@weekly`, `@daily` and `@hourly`.

`std.cron.matches` checks the containing civil minute. When neither day field
starts with `*`, either day field can match. When either starts with `*`,
both must match, including when the wildcard has a step. The month, hour and
minute always have to match.

`std.cron.next_after` uses UTC and returns the first matching minute boundary
strictly after the input. It skips disallowed months, hours and minutes with
bit scans. The Gregorian calendar repeats every 400 years, so the search tests
at most 4801 months and up to 31 dates per visited month. `None` means no
occurrence within the supported calendar or an input outside `std.time`'s
years 0–9999.

The parser accepts up to 4096 bytes with no nesting. It returns `CronTooLong`
above that size or `CronSyntax` with a byte offset for malformed input.
