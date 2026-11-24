# tz

`tz.zone.embedded()` opens the pinned IANA database. Keep the returned
`Database` to look up multiple zones by name with `tz.zone.lookup(db, name)`.
`tz.zone.version()` reports the pin. To use other data, call
`tz.zone.parse(bytes)` or `tz.zone.load(dir, name)` with a zoneinfo directory
capability. All conversion and parsing code is Hirsel. The optional loader
uses the replayable `std.file` operations and inherits the directory's path
policy.

`tz.zone.to_local(zone, utc)` returns a `std.time.Civil` and
`tz.zone.offset_at(zone, utc)` returns the offset in seconds east of UTC.
`tz.zone.from_local(zone, civil)` returns `Unique`, `Ambiguous(early, late)`
or `Gap`. These operations preserve nanoseconds. They return `Range` if a
result falls outside the calendar supported by `std.time`. A file with
unusually many backward changes can produce more than two matches, which
returns `MultipleMatches`.

The parser uses the 64-bit block in TZif v2/v3. A POSIX footer governs the
tail of a zone, including signed or extended v3 rule times. For files with
transitions but an empty or absent footer, the tail is unspecified starting
at the last transition, as defined in RFC 9636 §3.2. The designation `-00`
also marks unspecified local time. DST abbreviations without rules return
`MissingRules`, since POSIX leaves those rules to the implementation.
For files with a leap table, transition times are normalized to POSIX
seconds. The leap second itself has no `std.time.Civil` representation.

The parser accepts at most 256 KiB, 16,384 transitions, 256 types, 256 leap
records, 4,096 designation bytes and 255 footer bytes. Parsing is linear in
the bounded data and grammar depth is fixed. Reverse conversion examines at
most 258 offsets, each with a binary search of the transition table or a
fixed number of footer calculations. Zone names are at most 255 bytes and
16 path components.

## Updating the database

The pin is IANA tzdata 2026f. The tzdata and the Kathmandu conformance
fixture derived from it are public domain under the IANA release's LICENSE.
The embedded database includes canonical names and IANA aliases from the
standard release source set. Byte-identical TZif records share one entry.
A sorted index points into the zlib-compressed archive, and lookup parses
only the selected record.

Update `VERSION` and `SHA256` in `update_data.py` to the new release and run
`python3 core/programs/tz/update_data.py`. The script checks the archive and
uses `zic -b slim`. The checked-in data was generated with glibc zic 2.39
and Python 3.12. Pass `--archive` to use a local copy of the pinned download.
After reviewing the conformance rows against the new release, run
`hirsel laws write core/programs/tz --libraries core`.

[IANA tzdata 2026f](https://data.iana.org/time-zones/releases/tzdata2026f.tar.gz) · [RFC 9636](https://www.rfc-editor.org/rfc/rfc9636.html)
