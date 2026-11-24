# WebP codec

`image_webp@1` uses libwebp 1.6.0 to decode stills and animations to RGBA
and encode RGBA as lossy or lossless WebP. Callers supply the output buffers.

Still decode uses the host stack when both the source and output have at
most 16 pixels and the file is at most 256 bytes. This also applies to crop
and scale because the decoder can still process the source bitstream.
Animation decode uses the stack for at most 4 canvas pixels summed across
all frames and at most 256 input bytes. The byte ceiling bounds native
container scanning even for tiny images. Larger decodes and every encode
use `waits` to run on a helper while the calling task parks.

A shielded x86-64 sweep measured warmed native calls with flat, patterned
and high-entropy images. The longest samples eligible for the stack were
24.716 µs for still decode, 37.540 µs with crop and scale, and 36.618 µs
for animation. A single-pixel encode already reached 104.375 µs.
