# Int

[API overview](../API.md). [Operations and contracts](../int.hirsel).

This module extends core's integer operations. Radix parsers operate at u64
and i64, with bases 2 through 36; callers use `int.try_narrow` for narrower values.
Parsing rejects signs where disallowed, prefixes, separators and out-of-range
digits. The std parsers report the first invalid or overflowing digit. Core
`int.parse` checks the whole grammar before reporting OutOfRange.
The parsers take O(input length) time and skip per-digit overflow checks when
the decimal digit count guarantees that the value fits. They and the ASCII
helpers allocate nothing.
