# Int

[API overview](../API.md). [Operations and contracts](../int.hirsel).

This module extends core's integer operations. Radix parsers operate at u64
and i64, with bases 2 through 36; callers use `int.try_narrow` for narrower values.
Parsing rejects signs where disallowed, prefixes, separators and out-of-range
digits. Overflow reports OutOfRange at the first overflowing digit.
The ASCII helpers and parsers allocate nothing.
