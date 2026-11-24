# Nano ID

[API overview](../API.md). [Operations and contracts](../nanoid.hirsel).

`std.nanoid.generate` returns URL-safe bytes using the [Nano ID alphabet](https://github.com/ai/nanoid/blob/main/url-alphabet/index.js) (MIT).
`std.nanoid.custom` accepts 2 through 256 distinct byte symbols and emits one
byte per symbol, so a custom identifier can contain arbitrary binary data.

Both functions consume caller-supplied random bytes in order. Each byte's
low bits select a position in the smallest power of two that covers the
alphabet. Positions outside the alphabet are discarded, so each symbol has
the same chance when the supplied bytes are uniform. Use fresh entropy from
`host.random` for identifiers that must be unpredictable.

The output limit is 64 KiB and the supplied randomness limit is 1 MiB. If
the random bytes run out before the requested length, the function returns
`RandomExhausted` and discards the partial identifier. The caller can retry
with a longer random buffer.
