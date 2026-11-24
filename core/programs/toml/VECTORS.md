# TOML 1.0.0 vectors

`vectors.hirsel` embeds the 717 TOML documents from
[`tests/files-toml-1.0.0`](https://github.com/toml-lang/toml-test/blob/96fd088448ec0f4d9f061c730ed7854399c46faf/tests/files-toml-1.0.0)
and the 208 corresponding tagged JSON expectations. Inputs preserve the original
bytes, including malformed UTF-8 and line endings. The JSON expectations are
minified. Numbers use integer values or binary64 bits, booleans use JSON
booleans, and dates, times and offsets use arrays of numeric components.
Python's JSON and binary64 conversion produced these expectations from the
upstream JSON. The other version's vectors and the upstream runner are omitted.

Upstream distributes the vectors under the MIT license in `LICENSE-vectors`.
