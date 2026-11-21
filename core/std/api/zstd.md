# Zstandard

[API overview](../API.md). [Operations and contracts](../zstd.hirsel). [Flate](flate.md).

`std.zstd` reads and writes RFC 8878 Zstandard frames.

The decoder buffers one compressed block for the reverse bitstream. It
retains entropy tables and match history across blocks in a frame. It checks
each input access against its section and verifies the frame's declared
content size and checksum.

```
pub type ZstdError = Truncated | BadMagic | ReservedBit | Corrupt | BlockOversize | Checksum | ContentSize | WindowTooLarge(u64) | TooLarge | TrailingData(u64)
pub type Error = Data(ZstdError) | Capacity(Capacity)
pub fn zstd.dictionary(data: Bytes) -> Result[Dictionary, Error]
pub fn zstd.decode(data: Bytes, max_output: u64, max_window: u64) -> Result[Bytes, Error]
pub fn zstd.decode_with(d: Dictionary, data: Bytes, max_output: u64, max_window: u64) -> Result[Bytes, Error]
pub fn zstd.decoder(max_window: u64) -> Result[Decoder, Capacity]
pub fn zstd.decode_step(mut d: Decoder, input: Bytes, lo: u64, hi: u64, mut out: Bytes) -> Result[flate.Step, ZstdError]
pub fn zstd.encode(data: Bytes, level: u8) -> Result[Bytes, Capacity]
```

**Decoding.** `decode` reads one or more concatenated frames, and skips skippable frames,
as `zstd -d` does. `max_output` bounds the total output (`TooLarge`). `max_window` bounds
the window a frame may demand (`WindowTooLarge(asked)`), because the window is memory the
decoder must hold. RFC 8878 recommends that decoders support 8 MiB, which suits
`max_window`. A frame whose header states a content size must produce exactly that many
bytes (`ContentSize`). A frame with a checksum must match the low 32 bits of its XXH64
(`Checksum`). XXH64 is private to this module. Bytes that are not a frame give
`TrailingData(offset)`. `Dictionary` is a validated dictionary or raw-content prefix,
reused across frames. Its frames must name its ID. `decode_step` is `flate.decode_step`'s
push protocol, and it reports `ended` after each frame.

**Encoding.** `encode` writes one frame at `level` 1 to 19, with the content size and an
XXH64 checksum, and no dictionary. Its contract is that `decode` returns `data` and that
`zstd -d` accepts the frame. The compression ratio and the exact bytes are not part of the
contract, so the first implementation may emit raw and RLE blocks and add compressed blocks
later without changing callers. Levels follow libzstd's ordering: a higher level is never
faster.

- **Authority.** None.
- **Charging.** A decoder admits its window (at most `max_window`) and its tables, and an
  encoder admits its level's match state. Both are under the current limit. Whole-buffer
  outputs grow by GROWTH and are refunded on failure. The step operations admit nothing.
- **Failures.** As listed. A failed decoder repeats its error.
- **Replay.** These operations pass no replay point and are recomputed on replay.
