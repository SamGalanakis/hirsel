# Trace

[API overview](../API.md). [Operations and contracts](../trace.hirsel).

`std.trace` records spans in a caller-owned `Recorder` and encodes them as OTLP/JSON
(opentelemetry-proto 09769935, `ExportTraceServiceRequest`). There is no tracer, no ambient
context and no switch: a program that does not trace passes no recorder. Starting a span
takes the parent `Context`, the `Clock` and the id source explicitly. `otlp_json` is pure;
sending the document is the caller's business.

## Types

```
pub opaque type Context: key        // trace id (hi, lo) and span id, neither all zeros
pub type Error: key = ZeroTrace | ZeroSpan | OtherTrace | SelfParent
pub type SpanKind: key = Internal | Server | Client | Producer | Consumer
pub type Status: key = Unset | Succeeded | Failed
pub type Value = Str(String) | Bool(bool) | Int(i64) | Double(f64) | Blob(Bytes)
pub type Attribute { key: String, value: Value }
pub opaque type Recorder            // Clock anchor and a bounded log of ended spans
pub opaque type Span                // started, not yet ended
pub opaque type Ended               // as `finish` returns it
```

## Decisions

- **Time.** `recorder(c)` reads `host.wall_ms(c)` and `host.mono_ns(c)` once. A later
  reading `m` of `host.mono_ns(c)` is Unix time `wall_ms * 10^6 + (m - mono_at_anchor)`
  nanoseconds. This is the correlation `tools/obs_export.py` applies to original timing.
  Spans get nanosecond resolution and never run backwards when the wall clock is set. A
  `provide.clock` Clock gives exact, repeatable times. A Clock that reads below the anchor,
  such as a different Clock, fails with `Overflow` instead of wrapping.
- **Ids.** `start` draws from a `std.rand.Pcg`. A root draws trace hi, then trace lo, then its
  span id. A child draws only its span id and keeps its parent's trace id. A zero draw is
  drawn again. `start_with` takes caller-supplied ids, for example ids read from a request
  header. Ids that must not be guessed come from a `Pcg` seeded with `host.random`.
- **Refusals.** `context` refuses an all-zero trace id (`ZeroTrace`) and a zero span id
  (`ZeroSpan`), which OTLP calls invalid, so no `Context` holds one. `start_with` refuses a
  claimed parent that does not fit the span: a parent in another trace (`OtherTrace`), or
  the span itself (`SelfParent`). A child that claims a parent without a parent context
  cannot be written: the parent is an `Option[Context]`, and no `Context` is zero.
- **Attributes** are kept sorted by key bytes, one value per key, and setting a key again
  replaces its value. That is the order `obs_export.py`'s `attributes` produces. Resource
  attributes keep the caller's list order.
- **Encoding.** Members are written in key order with no spacing, so the document has
  the bytes Python's `json.dumps(sort_keys=True, separators=(",", ":"))` gives for ASCII
  text. It is the shape of `obs_export.py`'s `span_record` and `request_body`: hex ids,
  decimal-string times and `intValue`, `kind` and `status.code` as numbers, `links` always
  present, and `parentSpanId` only for a child. `core/std/tests/trace.hirsel` and
  `tools/tests/obs_export.py` check one hand-written document from both sides. Two spellings
  have no `obs_export.py` counterpart. `doubleValue` uses std.json's shortest spelling,
  which differs from Python's repr outside 1e-4 to 1e16, and nonfinite values become
  `"NaN"`, `"Infinity"` or `"-Infinity"`. `bytesValue` is padded base64.
- **Links** carry only the linked context. Status codes carry no message, as in
  `obs_export.py`.

## Logs

Leveled, structured log lines belong to the same module as spans and follow the same shape.
State is caller-owned, there is no global logger, and the encoder is pure. Writing a line
is a thin wrapper over one `stderr.write_bytes` call. A program that does not log passes no
logger.

```
pub type Level: key = Debug | Info | Warn | Error
pub opaque type Logger              // Clock anchor, minimum level, reusable line buffer
pub fn logger(c: Clock, min: Level) -> Logger
pub fn enabled(l: Logger, level: Level) -> bool
pub fn encode_line(mut out: Bytes, unix_ns: u64, level: Level, ctx: Option[Context], msg: String, take fields: List[Attribute])
pub fn log(mut l: Logger, c: Clock, err: Stderr, level: Level, ctx: Option[Context], msg: String, take fields: List[Attribute]) -> Result[(), WriteError] waits
```

```hirsel
let mut lg = std.trace.logger(c, level)
let mut fields: List[std.trace.Attribute] = list.new()
list.push(mut fields, std.trace.Attribute(key: "bytes", value: Int(n)))
std.trace.log(mut lg, c, err, Info, Some(std.trace.ids(span)), "fetched", take fields)?
```

- **Levels.** `Debug < Info < Warn < Error`, in increasing severity.
  `enabled(l, level)` checks whether the level meets the minimum. Below the minimum,
  `log` returns `Ok(())` without reading the clock or writing to stderr. Check `enabled`
  before building expensive fields. The level usually comes from `config.log_level_or`
  (config.md, Flags).
- **Time.** `logger(c, min)` anchors to `c` exactly as `recorder(c)` does (Decisions, Time
  above), so a log line and a span read from the same Clock share one time scale. A
  `provide.clock` Clock gives exact, repeatable times.
- **Line format.** `encode_line` appends one JSON object and a `\n` to `out`, which grows by
  GROWTH. Members come in this order: `time`, `level`, `msg`, then `trace_id` and `span_id`
  (lower-case hex, as in spans) when `ctx` is `Some`, then `attributes` when `fields` is
  nonempty. `time` uses the same RFC 3339 UTC format as `time.push_rfc3339` with nine
  fraction digits. `level` is `DEBUG`, `INFO`, `WARN` or `ERROR`. `attributes` is an object whose
  members are sorted by key bytes, with one value per key, and a later field replaces an
  earlier one, the same rule spans use. The JSON is written with no spacing, and strings
  are escaped as std.json escapes them, so a message holding a newline cannot forge a
  second line. Values: `Str` a string, `Bool` a literal, `Int` a JSON number (exact as
  text, though a reader that parses into a double loses precision past 2^53), `Double`
  std.json's shortest form, or `"NaN"`, `"Infinity"` or `"-Infinity"`, and `Blob` padded
  base64. Fields sit under `attributes`, so no field can collide with `time`, `level` or
  `msg`. The output depends only on the arguments, so a replay writes the same bytes.
- **Writing.** `log` encodes into the logger's own buffer, which it clears first, and
  writes it with one `stderr.write_bytes(err, line)`. A `WriteError` is returned as it is.
  Its `written` bytes, a prefix of the line, stay written. A cancellation at the write may
  likewise leave a prefix. Lines from one logger appear in call order. This module promises
  no order between tasks or between loggers, and no atomicity of a line against another
  task's writes.
- **Secrets.** `Value` has no variant that holds a `Secret`, and program code never gets
  a secret's bytes, so no log line can hold one. A program that wants to record
  that a credential was used logs `Str(secret.render(s))`, which is `[secret]`.

- **Authority.** `logger` and `log` read `c`. `log` writes through `err`. `encode_line`
  needs none.
- **Failures.** A refused allocation raises `Capacity`. `log` returns the stream's
  `WriteError`. `logger` fails with `Overflow` as `recorder` does, and so does `log` when
  `c` reads below the anchor.
- **Charging.** `logger` admits nothing until the first line. The line buffer is admitted
  under the limit current at that `log` call, grows by GROWTH to the longest line, and is
  refunded when the logger is dropped. The encoder takes ownership of the fields and
  sorts the list in place with a stable sort. Its temporary storage is refunded before
  the write. Encoding otherwise admits only the output buffer's growth. The `enabled`
  check admits nothing and takes O(1) steps. Sorting takes O(k log k) comparisons for
  k fields, and escaping takes O(bytes written).
- **Replay.** `log` reads the Clock and writes the stream, each passing its own replay
  point. The encoding passes none and is recomputed.

### Prior art

- Go `log/slog` (`/tmp/ref-go`, go 8eaf19e): levels Debug −4, Info 0, Warn 4, Error 8
  (`src/log/slog/level.go:44-47`) and the `time`, `level` and `msg` keys
  (`src/log/slog/handler.go:179-185`). ADOPTED: the four levels, the key names and their
  order, and the JSON-lines handler as the one sink. REPLACED: slog's numeric gaps for
  custom levels. A closed `key` type keeps a filter total. Also REPLACED: slog's default
  logger, which is ambient global state. NOT TAKEN: the text (logfmt) handler,
  `With` groups and context propagation. A caller builds fields once and passes them on.
- Python `logging` (`/tmp/ref-cpython`, cpython 32b51e3, `Lib/logging/__init__.py:95-101`):
  DEBUG 10 to CRITICAL 50. NOT TAKEN: CRITICAL, which no program here can act on
  differently from `Error`, and the logger hierarchy and handler registry, which are
  global configuration.
- Goose (`/tmp/ref-goose`, goose d5ae5bb) has `write_stderr` (`stdlib/os.goose`) and no
  leveled logging. ADOPTED: the plain stderr sink, with no buffering layer of its own.

## Left out

Span events, link attributes, status messages, trace state and flags, propagation
formats, sampling, OTLP transport, the metrics signal, OTLP log records (logs are JSON lines
on stderr; an OTLP log exporter is a package), log rotation and files, and sampling or rate
limits for logs.
