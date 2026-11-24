# JPEG codec

This software is based in part on the work of the Independent JPEG Group.

`image_jpeg@1` links the libjpeg API and x86-64 SIMD from libjpeg-turbo
3.2.1. [build.sh](build.sh) pins the source tarball and checks that the
relocatable archive exports only `hjpg_*` and contains no initializers,
indirect functions or dynamic loader imports. The linked implementation
is under the [IJG license](README.ijg), with the SIMD code under the zlib
terms described in the [upstream license](LICENSE.md).

Decode takes owned JPEG bytes and borrows them only for individual calls.
The header probe enforces byte and source pixel limits before native
allocation. The decoder accepts 8-bit grayscale or three-component Huffman
DCT JPEGs, including progressive files, and outputs packed RGB. The target
size selects a DCT scale before decoding. Call `start_decode` until the
output dimensions are available, then call `decode_strip` until `End`.
`Pending` means the decoder needs another call without having written rows.

Encode accepts packed RGB in strips of up to 16 rows and writes to an
initialized, caller-supplied buffer. Reoffer the unread rows when a call
consumes fewer than supplied. Select sequential or progressive and 4:4:4
or 4:2:0 sampling. `finish_progressive` runs the remaining whole-image
passes on a helper, so it is a `waits` call. `finish_encode` finishes a
sequential stream on the host stack.

Hirsel charges the owned buffers and one step per native call. The wrapper
caps each handle's libjpeg allocations separately under FFI-7 and refuses
backing-store use.

To compare with direct C, set `JPEG_REFERENCE_DIR` to an absolute path
when running `build.sh`, then pass that directory to `test-vectors.sh`.

## Measured cost

Stock libjpeg processes one interleaved minimum coded unit (iMCU) row at
a time. Suspension limits the compressed bytes offered per call. The
codec still has to process across the row, so call duration depends on
image width, sampling, and entropy.

A shielded x86-64 run used 200 repetitions per width of a high-entropy,
37-row, sequential 4:4:4 fixture. At 2048 px, the longest strip calls
were 128.2 µs for decoder input and 95.6 µs for encoder input, or 62.6
and 46.7 µs per 1000 px respectively. Decode exceeded 50 µs at every
sampled width from 256 px through 2048 px, while 224 px took 36.7 µs.
Encode exceeded 50 µs from 384 px through 2048 px, while 256 px took
47.0 µs. These crossings are specific to the measured fixture.

A 50 MP JPEG shrunk at 2/8 DCT scale and resized to 800 × 400 by a
streaming Lanczos3 consumer took **122 ms and 13.7 MiB peak RSS**
against the PLAN targets of <10 ms and <10 MiB. This measurement used
the native adapter and a C resizer, with the 9.81 MiB compressed input
included in RSS. Timing started after reading the compressed input.

In 60 matched pairs per mode on CPU 16, the release wrapper used 1.199×
direct C cycles for sequential decode, 1.026× for sequential encode,
1.077× for progressive decode, and 1.049× for progressive encode.
