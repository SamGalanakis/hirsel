# CRC-32C

[API overview](../API.md). [Core declaration](../../core/crc32c.hirsel). [IEEE CRC-32](crc32.md). [CRC-64/NVME](crc64nvme.md).

```hirsel
pub fn crc32c.update(crc: u32, input: Bytes) -> u32
pub fn crc32c.combine(a: u32, b: u32, len_b: u64) -> u32
```

Pass zero to start a CRC-32C checksum, or pass a previous result to continue it.
An empty input returns the previous result. The reflected Castagnoli polynomial
is `0x82F63B78`. The check value for `"123456789"` is `0xE3069283`.
Use `list.slice` to checksum a checked range without copying it.

The operation admits no memory and takes linear work. It charges the call step
and one step before each nonempty 16 KiB chunk. Chunk boundaries and results are
the same on every engine and CPU. Native programs select the implementation on
the first nonempty call and reuse it for the process. x86-64 uses SSE4.2 CRC
instructions and folds long inputs with PCLMULQDQ when available. AArch64 uses
the CRC extension when available. Other CPUs use the portable table.

`combine` returns the checksum of two concatenated ranges from their finalized
checksums, each computed starting at zero, and the byte length of the second
range. An empty second range has checksum zero and leaves `a` unchanged.
The operation accepts the full `u64` length range, allocates nothing, and uses one
step. Set `HIRSEL_CRC=portable` before starting a native program to force the
portable update implementation.
