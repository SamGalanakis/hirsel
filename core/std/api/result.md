# Result

[API overview](../API.md). [Operations and contracts](../result.hirsel).

This module extends core's Result operations. `map_err` converts only the error
through a scoped callback and passes Ok through unchanged. Error propagation
requires the function's exact error type; `?=>` and `match` convert the rest.
