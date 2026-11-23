# CRC-64/NVME

[API overview](../API.md). [Core declaration](../../core/crc64nvme.hirsel). [CRC-32C](crc32c.md).

```hirsel
pub fn crc64nvme.update(crc: u64, input: Bytes) -> u64
pub fn crc64nvme.combine(a: u64, b: u64, len_b: u64) -> u64
```

Pass zero to start a CRC-64/NVME checksum or pass a previous result to
continue it. Empty input returns the previous result. The reflected polynomial
is `0x9A6C9329AC4BC9B5`. The check value for `"123456789"` is
`0xAE8B14860A799888`. Use `list.slice` to checksum a checked range without copying.

`combine` returns the checksum of the concatenation of two ranges from their
finalized checksums `a` and `b`, each computed starting at zero. Pass the byte
length of the second range as `len_b`. An empty second range has checksum zero
and leaves `a` unchanged. The operation accepts the full `u64` length range and
uses one step.

Neither operation allocates. Updating takes linear work and charges the call
step plus one step before each nonempty 16 KiB chunk. Native programs select
the update implementation on the first nonempty call. x86-64 folds input with
VPCLMULQDQ or PCLMULQDQ when available. AArch64 uses PMULL when available.
The fallback is portable slicing-by-8. Set `HIRSEL_CRC=portable` before starting
a native program to select the fallback for both CRC widths.
