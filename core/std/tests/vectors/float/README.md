# Float sources

- `more-test-cases.txt` is unchanged from Nigel Tao's
  [manually curated parse-number-fxx-test-data table](https://github.com/nigeltao/parse-number-fxx-test-data/blob/55d79b184b7d8fac2e143e89dc19b766ec4e54b8/data/more-test-cases.txt),
  Apache-2.0. All 60 inputs check the f32 and f64 columns. Hirsel has no f16
  parser. [The float contract](../../../float.hirsel#L23) returns
  `OutOfRange` where the upstream answer is signed infinity.
- `ryu_shortest64` adapts [Ryū's d2s tests](https://github.com/dtolnay/ryu/blob/75f9e707e77c89717ffb9c2a01fe2c28a016cc82/tests/d2s_test.rs),
  commit `75f9e707e77c89717ffb9c2a01fe2c28a016cc82`, Apache-2.0.
  `shortest64` returns the absolute coefficient and base-10 power, so the
  oracle normalized Ryū's expected decimal spelling to that pair.
