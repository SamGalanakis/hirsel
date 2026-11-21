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
pub opaque type Recorder            // Clock anchor and a Log of ended spans (MEM-29)
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
  the span itself (`SelfParent`).
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

## Left out

Span events, link attributes, status messages, trace state and flags, propagation
formats, sampling, OTLP transport, and the metrics and logs signals.
