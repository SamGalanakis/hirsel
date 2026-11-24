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
