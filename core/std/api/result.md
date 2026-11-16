# Result

[API overview](../API.md). [Operations and contracts](../result.hirsel).

This module extends core's Result operations. `map_err` converts only the error
through a scoped callback and passes Ok through unchanged. `capacity_only` handles
the capacity conversion named in its source contract. Error propagation requires
the function's exact error type; other conversions remain explicit.
