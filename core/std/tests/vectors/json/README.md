# JSON sources

- `test_parsing/` contains every input unchanged from Nicolas Seriot's
  [JSONTestSuite, test_parsing/](https://github.com/nst/JSONTestSuite/tree/1ef36fa01286573e846ac449e8683f8833c5b26a/test_parsing),
  commit `1ef36fa01286573e846ac449e8683f8833c5b26a`, MIT.
- The `serde_json_numbers` table in `../../json.hirsel` adapts
  `test_parse_number_errors`, `test_parse_i64`, `test_parse_u64`,
  `test_parse_negative_zero` and `test_parse_f64` from
  [serde_json, tests/test.rs](https://github.com/serde-rs/json/blob/f51fa15a7015c7d02817a46d2db44c899a0016e8/tests/test.rs),
  commit `f51fa15a7015c7d02817a46d2db44c899a0016e8`, MIT.
  Hirsel reports byte offsets. The adapted cases check number bits or error
  kind instead of serde_json's diagnostic strings.

## Implementation-defined cases

`generate.py` turns the upstream bytes into the tables used by the Hirsel law.
It makes these choices for the `i_` inputs:

- Both negative exponents that underflow and all three integers outside the
  64-bit integer range parse. `json.hirsel` documents finite f64 admission
  and preserves the source spelling in `Json.Num`.
- The five `i_number_` inputs with positive exponent overflow return
  `NotFinite`, as documented on `json.Error`.
- Lone, inverted, and mismatched escaped surrogates return `Syntax`, as
  documented on `json.Error`.
- Malformed UTF-8 and non-UTF-8 encodings are refused by `string.from_utf8`.
  `json.parse` takes a `String`, which admits only UTF-8 scalar values.
- A leading UTF-8 BOM returns `Syntax`. No JSON token starts with a BOM, and
  advancing past it as whitespace is optional under RFC 8259 section 8.1.
- The 500 nested arrays return `Depth` under the 128-container depth
  contract in [the JSON API](../../../api/json.md).

The two required refusals with a deep unclosed prefix also return `Depth`.
The generator checks that its repeated-prefix representation reconstructs
each of the three upstream nesting inputs byte for byte.
