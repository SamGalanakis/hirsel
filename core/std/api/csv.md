# Csv

[API overview](../API.md). [Operations and contracts](../csv.hirsel).

`std.csv` reads and writes RFC 4180 comma-separated values. The reader copies nothing. It
finds one record at a time in a byte buffer the caller owns and reports each field as a span
of that buffer, so a field is copied only when the caller asks. It can resume, so a file is
read in chunks. The writer appends quoted fields to a caller-owned String. Both are strict.
Any input that RFC 4180 does not give a meaning, and on which Go and Python disagree, is an
error with a byte offset, not a guess. A record is a list of fields. Checking the field
count, headers and types is the caller's job.

## Types

```
pub type Dialect: key { delimiter: u8, crlf: bool }
pub type Field: key { span: http1.Span, quoted: bool }
pub type Next: key = Record(u64) | End | Need
pub type Error = Invalid(u64) | NotUtf8(u64)
pub opaque type Writer { d: Dialect, fields: u64, empty_first: bool }
```

- `Dialect.delimiter` is one ASCII byte other than `"`, CR and LF, such as `,` `;` `\t` or
  `|`. An invalid delimiter causes Fail. `crlf` chooses the writer's line end. The reader
  accepts LF and CRLF in either mode. Plain CSV is `Dialect(delimiter: 44, crlf: false)`.
- `Field.span` indexes the caller's buffer, using the same `Span` type as `std.http1`. For a
  quoted field the span covers the bytes between the quotes, with each `""` still doubled,
  and `quoted` is true. `field_text` undoubles them.
- `Record(next)` gives the offset just past the record's line end, or the buffer's length
  at the end of the text. `End` means no record starts at `at` and the text is over. `Need`
  means the record that starts at `at` may continue past the buffer.
- `Invalid(i)` is the byte offset of the first byte the grammar refuses. `NotUtf8(i)` is the
  offset, in the buffer, of the first byte that does not start a UTF-8 scalar.

## Grammar

This is RFC 4180 §2, with the reader's rulings where the RFC is silent.

- A record is fields separated by the delimiter and ended by LF, CRLF or the end of the text.
  The final line end is optional.
- An **unquoted field** is any run of bytes other than the delimiter, `"`, CR and LF. A `"`
  inside it is `Invalid` (Go `ErrBareQuote`). A CR that is not followed by LF is `Invalid`.
- A **quoted field** starts with `"` as its first byte and runs to the next `"` that is not
  doubled. Inside it, every byte stands for itself, including the delimiter, CR, LF and CRLF.
  Only `""` is special. After the closing quote, the next byte must be the delimiter, a line
  end or the end of the text. Anything else is `Invalid` at that byte. An unclosed quote at the
  end of the text is `Invalid` at the opening quote.
- An **empty line** is a record with zero fields. It is not skipped.
- The reader does not strip a byte-order mark, trim space, or treat `#` as a comment. A BOM
  is part of the first field's bytes, so a caller that expects one strips it from the buffer
  first.

Round trip: for any list of records of Strings, writing them and reading the result back
gives the same records. This holds for empty records, empty fields, and fields holding the
delimiter, quotes, CR, LF and CRLF. It is law C1.

## Decisions

- **Spans, not copies; ADAPTED from Goose `csv.each_record`** (Goose
  `stdlib/csv.goose:19-57`), which hands each record's fields to a block as slices of the
  text, leaves quotes doubled, and stops at the first malformed record. Hirsel has no block
  argument for a slice of borrowed views, so the record is a `List[Field]` of spans and the
  caller drives the loop. Goose skips nothing and treats a blank line as one empty field.
  Here it is zero fields, so the writer can tell `""` apart from an empty line. Goose's
  `format_field` (`csv.goose:60-71`) quotes on the same four bytes as this writer (ADOPTED).
- **Spans in the public API, extending HTTP-6.** HTTP-6 keeps `http1.Span` offsets inside
  std for the handler-facing `Request`. A CSV record cannot follow it. A record is a list of
  fields, and under OWN-19 a view cannot be a collection element. Also, the caller appends
  to the buffer after `Need`, which a live view's frozen anchor would forbid. HTTP-6 is
  unchanged.
- **Resumable windows; ADAPTED from csv-core** (csv-core 0.1.13 `src/reader.rs`), which is a
  push parser over input chunks with no allocation. csv-core keeps a DFA state between
  chunks. Here the caller re-offers the same `at`, so no hidden state crosses calls.
- **Go `encoding/csv` semantics where they are strict** (go 46b62495
  `src/encoding/csv/reader.go:297-460`): a bare quote is an error, a stray byte after a
  closing quote is an error, and so is an unterminated quote. These are ADOPTED. Where Go
  is lenient or lossy, this design REPLACES it, and each case is a deviation the tests list:
  - Go skips empty lines (`reader.go:302-313`). Here an empty line is a zero-field record, as in
    Python `csv.reader`.
  - Go turns CRLF inside a quoted field into LF (`readLine`, `reader.go:275`, test
    `CRLFInQuotedField`). Here the bytes are kept, as Python and RFC 4180 §2.6 do.
  - Go keeps a lone CR in an unquoted field as data (test `BareCR`). Python ends the line
    there. RFC 4180 allows CR only before LF or inside quotes. The two readers disagree, so
    this is `Invalid`.
  - Go drops a trailing CR at the end of the text (`reader.go:268`, test `TrailingCR`). That is the same lone
    CR, so it is also `Invalid`.
  - Go's CRLF writer inserts CR before embedded LF and drops embedded CR
    (`writer.go:86-112`). Here the record ending alone changes, so field data round trips.
  - Go writes a sole empty field as an empty line (`writer.go:150-161`). Here it is
    quoted to distinguish it from a zero-field record.
  - Go's writer quotes `\.` (for Postgres) and fields that start with a space
    (`writer.go:160-184`). Python does neither, and neither is needed for a round trip.
    These are NOT TAKEN.
  - Go's `Comment`, `LazyQuotes`, `TrimLeadingSpace` and `FieldsPerRecord` options are NOT
    TAKEN. Each one makes a second dialect of the same bytes.
- **No delimiter detection or `Sniffer`** (Python). The caller states the dialect.

## Left out

Comments, lazy quotes, space trimming, field-count enforcement, header maps, typed decoding
(parse each field with `int.parse` or `std.time`), BOM handling, non-ASCII delimiters, writing
non-UTF-8 bytes, `QUOTE_ALL` and other quoting styles, and escape characters other than `""`.

## Laws

- **C1 Round trip.** For about 40 hand-written records, `field`/`end` then `record` reading
  gives the same records. The records cover zero fields, one empty field, `["", ""]`, a field
  of only `"`, a field with CRLF, the delimiter `;` and `\t`, and a 1 KiB field. Oracle: the
  invariant. Bug caught: the writer writes a sole empty field as nothing, so it reads back as
  a zero-field record.
- **C2 Python reader agreement.** For each input in a table of about 30 texts that contain no
  bare quote and no lone CR, `record` reading, with `field_text` applied, gives exactly what
  Python 3.12 `list(csv.reader(io.StringIO(t, newline=""), strict=True))` gives. That
  includes `[]` for empty lines and errors where Python raises `csv.Error`. The expected
  rows are produced by `core/tools/csv_reference.py`, which runs Python's csv module, and
  are pasted into the test with the script's command. Oracle: CPython's `_csv`. Bug caught:
  an empty line skipped (Go's rule), or CRLF inside quotes normalized to LF.
- **C3 Python writer agreement.** For the same records as C1, the writer's output equals
  Python `csv.writer(f, lineterminator="\n")` (and `"\r\n"` with `crlf`). Includes
  `[[""]] -> "\"\"\n"`, `[[]] -> "\n"`, and `[["a b", " a", "\\."]]` written unquoted.
  Oracle: CPython, through the same script. Bug caught: quoting a leading space or `\.` (Go's
  rule), or quoting every empty field.
- **C4 Go reader table.** Go `readTests` (`reader_test.go:40`, 68 rows) that set no option
  other than `Comma`. Input `§` and `¶` markers are Go's position markers, stripped before use.
  Each row whose Go output differs because of a deviation above (`BlankLine`, `BareCR`,
  `TrailingCR`, `CRLFInQuotedField`, `FieldCR*`, `MultipleCRLF`) is kept with this page's
  answer and a comment naming the deviation. Go's line and column errors become byte offsets,
  derived by hand. Oracle: Go's table and the grammar above. Bug caught: `""` at the end of a
  quoted field read as a close followed by a bare quote (`EvenQuotes`), or `"a"b` accepted.
- **C5 Go writer table.** Go `writeTests` (`writer_test.go:14`) with this writer's output,
  with the deviations listed above. Rows with non-UTF-8 fields are outside the String API.
  Oracle: Go's table. Bug caught: a field containing only CR left unquoted.
- **C6 Resumption.** For every split point k of a 200-byte text that has quoted CRLF, an
  empty line and a final record with no line end, feeding `text[0..k]` with `eof = false` and
  then the full text with `eof = true` gives the same records as one call with all of it.
  Oracle: the one-buffer read. Bug caught: a record ending in CR at the buffer's end
  reported as `Invalid` (a lone CR) before the LF arrives.
- **C7 Offsets.** `a"b` gives `Invalid(1)`. `"a"b` gives `Invalid(3)`. `"ab` with eof gives
  `Invalid(0)`. `a\rb` gives `Invalid(1)`. Oracle: hand-derived from the grammar. Bug caught:
  the offset of the field's start reported in place of the bad byte.
- **C8 Atomic field_text.** `field_text` of a quoted field holding `"é"` doubled, with a
  `0xff` byte after it, leaves `out` unchanged and gives `NotUtf8` at the `0xff`. Oracle: the
  contract. Bug caught: undoubling into `out` before validating.
