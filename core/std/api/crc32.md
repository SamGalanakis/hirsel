# CRC-32

[API overview](../API.md). [Operations and contracts](../crc32.hirsel). [Flate](flate.md).

```
pub fn crc32.update(crc: u32, bytes: Bytes, lo: u64, hi: u64) -> u32
pub fn crc32.hash(bytes: Bytes) -> u32
```

`std.crc32` is the IEEE 802.3 CRC-32 (reflected polynomial `0xEDB88320`) that gzip, zip and
PNG use. `hash(b)` is `update(0, b, 0, len(b))`, and `update` continues a running value, as
Go's `crc32.Update` and `crc32.ChecksumIEEE` do. The check value of `"123456789"` is
`0xCBF43926`. Ranges have `list.slice`'s checked semantics.

The portable implementation is slicing-by-8 over a static table that is passed by address
(runtime-gzip-658 removed a per-call 8 KiB table copy). A carry-less-multiply path (x86-64
PCLMULQDQ, ARMv8 PMULL) is the planned speed-up. If it lands, it must give the same values,
steps and charges on every CPU, as the SHA-256 core path does. Both operations admit
nothing, take O(n) work, and pass no replay point.
