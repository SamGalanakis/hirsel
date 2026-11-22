# Content-defined chunking

`std.cdc.chunker(min, avg, max)` starts FastCDC v2016 at normalization level 1.
Sizes are in bytes. The minimum is 64 to 1,048,576, the average is 256 to
4,194,304, and the maximum is 1,024 to 16,777,216. The sizes must be in
ascending order, with ties allowed.

Pass each block to `std.cdc.feed`. The callback receives the length of each
completed chunk. A cut at a byte being tested starts the next chunk at that
byte. The chunker retains a hash and length across calls. The caller owns
the input bytes and can sum the lengths to find absolute offsets.
`std.cdc.finish` consumes the chunker and emits any nonempty tail.

The masks and fixed gear table match the
[fastcdc Rust crate's v2016 implementation](https://github.com/nlfiedler/fastcdc-rs),
based on [Xia et al., USENIX ATC 2016](https://www.usenix.org/conference/atc16/technical-sessions/presentation/xia).
The reference tables are distributed under the [MIT license](cdc-LICENSE-MIT.txt).
