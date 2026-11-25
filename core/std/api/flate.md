# Flate, zlib and gzip

[API overview](../API.md). [Operations and contracts](../flate.hirsel). [CRC-32](crc32.md).
[Zstandard](zstd.md).

`std.flate` is DEFLATE (RFC 1951) in three framings, `pub type Format = Raw | Zlib | Gzip`,
for zlib (RFC 1950) and gzip (RFC 1952). Go splits these into `compress/flate`,
`compress/zlib` and `compress/gzip`. Here one module and one `Format` serve all three,
because they share every block and differ only in header and trailer.

The decoder uses a 32 KiB history ring, with a bit reservoir that retains a
partial symbol across input calls. The same engine serves whole buffers and
push calls. Gzip header fields and header CRCs are included in the framing checks.

## Whole buffers

```
pub type FlateError = Truncated | Corrupt(u64) | Header | Checksum | TooLarge | TrailingData(u64)
pub type Error = Data(FlateError)
pub fn flate.decode(format: Format, data: Bytes, max_output: u64) -> Result[Bytes, Error]
pub fn flate.encode(format: Format, data: Bytes, level: u8) -> Bytes
```

`decode` reads exactly one stream: one gzip member, one zlib stream, or one raw stream up
to its final block. `max_output` bounds the decoded size, and passing it gives `TooLarge`.
That bound is the defence against decompression bombs, and a caller states it. Bytes after
the stream give `TrailingData(offset)`, where Go would ignore them. A gzip file of several
members is decoded one member at a time with the streaming decoder. `Corrupt(offset)`
gives the input offset of the bad unit. `Checksum` is a CRC-32 (gzip) or Adler-32 (zlib)
mismatch, and `Header` is an invalid header or reserved flag.

`encode` writes one stream at `level` 0 to 9. Level 0 writes stored blocks, 1 is the
fastest, 6 is the usual default, and 9 is the smallest, as in zlib and Go. A gzip header has
no name, comment or modification time (MTIME 0) and OS 255, so the output depends only on
`data` and `level`. The contract is that `decode` returns `data`. The exact bytes are this
implementation's and may change between releases. They are not a format to compare
against.

## Streaming

```
pub type Step { consumed: u64, produced: u64, ended: bool }
pub fn flate.decoder(format: Format) -> Decoder
pub fn flate.decode_step(mut d: Decoder, input: Bytes, lo: u64, hi: u64, mut out: Bytes) -> Result[Step, FlateError]
pub fn flate.encoder(format: Format, level: u8) -> Encoder
pub fn flate.encode_step(mut e: Encoder, input: Bytes, lo: u64, hi: u64, mut out: Bytes) -> Step
pub fn flate.finish_step(mut e: Encoder, mut out: Bytes) -> Step
```

The streaming operations are push-style, like zlib's `inflate`/`deflate` and Rust
`flate2`'s `Decompress::decompress`. Each call consumes from `input[lo, hi)` and appends at
most the spare capacity of `out`, so the caller owns every buffer and chooses its sizes.
`ended` reports the stream's end. After that, `decode_step` consumes nothing, and the
caller reads `lo + consumed` to find what follows (the next gzip member, a protocol frame).
A decoder that runs out of input keeps its partial unit and resumes on the next call.
`finish_step` flushes the final block and trailer and returns `ended` once all of them
are written. The caller calls it again while `ended` is false.

- **Authority.** None. Compression is pure computation over bytes the caller holds.
- **Charging.** `decoder` admits its 32 KiB window plus its code tables, and `encoder` its
  window, hash chains and block buffer (level-dependent, at most 512 KiB at level 9). Both
  are under the current limit, and the state is refunded when it is dropped. The step
  operations admit nothing. The whole-buffer operations admit their state and their
  output, which grows by GROWTH, and on failure they clean and refund it.
  Steps are O(input + output).
- **Failures.** A refused allocation raises `Capacity`. A decoder that reports a
  data error returns that error on every later call.
- **Replay.** These operations pass no replay point. A replay recomputes them from the
  bytes that reached the run.
