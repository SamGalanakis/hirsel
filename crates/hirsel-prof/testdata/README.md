# Saved profile PCs

- `llvm-inline.debug` is the detached DWARF from the LLVM profile fixture
  in the 2026-11-28 profiling audit. The inline ranges at `0xe8460` and
  `0xe8504` have empty names and refer to `hot` and `cold` abstract DIEs.
- `libc-ifunc.elf` is an x86-64 test image with a zero-filled executable
  section, the saved glibc build ID, and a GNU debuglink. Its detached file
  keeps only the original build ID and the two local text symbols from
  glibc build `8cebda42d140fc2b09639f8b13a5d5f2e617ef09`. The executable
  section contains no glibc code. The saved paired capture sampled
  `0x18891d` in `__memcmp_avx2_movbe` and `0x189024` in
  `__memmove_avx_unaligned_erms`, which aliases `__memcpy_avx_unaligned_erms`.
