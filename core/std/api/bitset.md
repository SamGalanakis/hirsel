# BitSet

[API overview](../API.md). [Operations and contracts](../bitset.hirsel).

A BitSet is an opaque owner of ceil(len / 64) u64 words. Bit positions below len
may be set or clear; bits past len are always zero, including the tail bits of the
last word. Growth appends zero words or zero tail bits under List's growth rule.
Shrinking truncates whole words and masks the surviving tail word, so a later growth
never exposes stale bits. Storage is charged as a List of u64 words, an eighth of the
storage of one bool per bit.
