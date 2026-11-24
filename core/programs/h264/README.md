# h264

H.264 decode and encode through OpenH264 v2.7.0 (BSD-2, `LICENSE`), as a pinned `Foreign[I]` archive whose only global symbols are `hh264_*`.

## Source and patents

- Upstream: `cisco/openh264` tag `v2.7.0`, commit `1bca62311e447fd9c39fc93de2c4b96cd5ce971c`. The release tarball has SHA-256 `5178775c8b440073e70920e97d195efff721c0e56d0a78cc82c919e95d54924d`. `build.sh` checks it.
- **The archive is built from source.** Cisco's MPEG LA patent indemnity covers only the binaries Cisco itself distributes, so it does not apply to this archive. This package carries the ordinary H.264 patent status of a source build (Sam's ruling 2, Option A).
- Every linked file is BSD-2. No LGPL or GPL code is linked.

## Scope

- Decode takes progressive 8-bit 4:2:0 AVC as one Annex B NAL per call. Slices, SEI, SPS, PPS, AUD, end-of-sequence, end-of-stream and filler NALs are accepted. SVC/MVC NALs, interlaced, high bit-depth and non-4:2:0 streams are refused.
- Each SPS is parsed with `media_bits.h264` before the codec sees it. A coded size above the decoder's declared maximum is refused before any allocation.
- Decode runs single-threaded with error concealment off.
- Encode produces single-layer, single-slice Baseline (CAVLC) or Main (CABAC) from packed I420.
- Every native call, including create and destroy, is `waits` and runs on a helper (FFI-7). No coded size keeps a call under the 50 µs host-stack budget. Measured directly against OpenH264 on one core, a 16x16 CABAC stream peaks at 148 µs per decode call, QCIF at 594 µs and 1080p at 35 ms. Decoder creation takes 160 µs, and encoder teardown at 1280x720 takes 534 µs.
- Each session's native heap is capped at 256 MiB by wrapped `malloc`/`calloc`/`free`. The `CodecHeap` resource reserves the same 256 MiB (MEM-27) before the session is created.

## Build

`LLVM_SYS_231_PREFIX=... HIRSEL=... ./build.sh`, with `OPENH264_TARBALL` set to use a local copy of the tarball. `OPENH264_REFERENCE_DIR` also exports the unwrapped `libopenh264.a` and API headers.

- `decoder-allocation.patch` makes `CWelsDecoder` check one unchecked `new[]`, so a refused allocation fails cleanly.
- The x86 assembly is built with the host `nasm`, the one unpinned build input (2.16.01 for the pinned archive).
- The archive is checked for `.init_array`/`.ctors`, IFUNC, `IRELATIVE` and `dlopen`.

## Tests

- `tests/limits` (`bounds.hirsel`) covers SPS bombs, crops and hidden second start codes against the coded-size limit, short output buffers, and the heap reservation.
- `test-vectors.sh WORK REFERENCE_DIR TARBALL` runs `tests/conformance` over every in-scope stream in upstream `test/api/decoder_test.cpp`. That is 50 of 51; `sps_subsetsps_bothVUI.264` carries SVC NAL 15. The law also encodes two upstream `res/` YUV clips in Baseline and Main.
- Every decoded picture and access unit must match `vectors/reference.c` calling the same `libopenh264.a` directly. That C output in turn matches each upstream SHA-1.
