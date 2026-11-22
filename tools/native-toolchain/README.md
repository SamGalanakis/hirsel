# native-toolchain

The pinned LLVM for native code is the 23.1.5 release-branch snapshot at
`a738f2737189bcc1de75c25e8cd1d4b466c9526e`, packaged on 2026-10-28.
The matching source archive and manylinux_2_28 builds are retained at
https://github.com/SamGalanakis/hirsel-lang/releases/tag/toolchain-llvm-23.1.5.

- `llvm-23.lock`: the source archive and compiled assets, with SHA-256 and size
  checks, plus the llvm-sys 231.0.1 version and checksum.
- `install --unpack DIR`: unpack the verified tree without root and print the
  `LLVM_SYS_231_PREFIX` and loader path that select it. `install --source` prints
  the build commands for the pinned source.
- `check [--prefix DIR]`: verify the LLVM version, shared library, headers,
  X86 and AArch64 targets, clang and ld.lld versions, and the llvm-sys version
  and checksum in Cargo.lock. Link and execute `fixtures/version.c` with the
  pinned tools.

- `fixtures/`: `version.c` for `check`; `smoke.ll`, `smoke_harness.c` and `smoke.expected`,
  the FFI smoke that `crates/hirsel-llvm/tests/it/native.rs` compiles, links and runs.

- `build-static-lld DIR`, `static-lld-elf-only.patch`: build the toolchain image's
  `bin/ld.lld` (the lock's `static_lld` record). See below.
- `build-libllvm DIR ARTIFACTS`, `engine_llvm.py`: build the compiler's engine libLLVM
  (the lock's `engine_llvm` record) and guard it. See below.

## Static ld.lld

The toolchain image's `bin/ld.lld` is built once per pin
from the locked source archive by `build-static-lld`: the ELF port only
(`static-lld-elf-only.patch`), targets X86 and AArch64, Release without assertions,
linked `-static-pie` and stripped. It loads no library, so each link skips
the dynamic loader's work for libLLVM (about 18 M instructions per run). It is a static
PIE rather than plain `-static` because a `-static` binary's start-up registers
`.eh_frame` and libgcc then classifies every FDE (about 10 M instructions). The
product runs it as a child, and the toolchain
identity hashes it by content like every other recipe file.

To rebuild it, run `build-static-lld DIR` in the manylinux_2_28 x86-64 image
with its GCC Toolset and glibc-static installed. Upload `DIR/ld.lld` to the
`asset_pool` release under the printed asset name, then replace the
`static_lld` record in `llvm-23.lock`. `tools/buck2/bootstrap_llvm.py` fetches that
asset, verifies its SHA-256 and size, and places it as `bin/ld.lld`. Nothing rebuilds
it per land.

The former LLVM 21 GitHub binary archive was static-only. `check` continues to
require a shared libLLVM; binary archives are not a fallback.

## Engine libLLVM

`build-libllvm` builds a libLLVM 23.1.5 for the compiler alone from the locked source
archive with profile guidance, ThinLTO, X86, AArch64 and zlib. It runs three
stages: an IR-instrumented library;
`engine_llvm.py train`, which builds the training programs through it (the LangArena
suite, every M1 program and the documents layout family, all generated from the
repository's own input helpers); and the profile-guided build.

The guard builds the training programs and four held-out ones (the graph and json
layout families and the M2b table and dispatch_cross_package workloads). It compares
the locked apt.llvm.org deb with the new engine through the same compiler. It requires
byte-identical executable `.text` for all cases and a warm instruction geomean at or
below `engine_llvm_guard`. `engine_llvm_guard_source` pins the compiler and workloads
that set this ratio. Changes in how Hirsel builds and caches std affect the ratio.

Use the manylinux_2_28 x86-64 image with its GCC Toolset to build the engine.
`build-libllvm DIR ARTIFACTS bootstrap` builds the stock shared library, headers,
clang and LLVM tools. For `instr` and `opt`, set `CC` and `CXX` to the bootstrap
compilers and supply the same snapshot's lld, TableGen and compiler-rt profile runtime.

On Ubuntu 24.04, build the release compiler and shipping runtime at
`engine_llvm_guard_source` with `core/tools/measurement_artifacts.py`. Use its JSON as
`ARTIFACTS` and set `HIRSEL_LLVM_PROFILE_SOURCE` to that checkout. Run `train` on the
host with the instrumented library and the same snapshot's llvm-profdata. Place the
merged `llvm.profdata` in `DIR` for the container's `opt` stage. Run `guard` on the
host with the optimized library in `DIR`. It verifies the reference deb and enforces
a 2 GiB memory cgroup with no swap. Upload the qualified library under the printed
name and update `engine_llvm` in the lock.

The bootstrap places the stock library in the tree's `lib/` and the optimized
library in its `engine/`. The compiler's RPATH selects the engine while clang
and the LLVM tools select the stock library. The installation ships the
engine, the static linker, and the dynamic inputs of the pinned link kit.

## Pool execution inputs

`BUCK` downloads the locked LLVM bootstrap archive and static linker,
Arm GNU 14.3.Rel1 (including its Linux sysroot), Ubuntu's static AArch64 qemu
and Valgrind 3.22.0 with a matching libc 2.39 loader and debug symbols. `native.lock.json` pins the additional archives by SHA-256
and byte size. `python3 tools/buck2/sync.py` generates `pins.bzl` from these
locks; `--check` rejects stale pins. Nothing is installed on the host or in the
worker image. Only unpacking runs locally, using `dpkg-deb`; it resolves
symlinks before Buck2 hashes the output directories. Execution uses read-only
action inputs inside the pool's networkless sandbox, with its private `/tmp`.

## Link kit

`native.lock.json`'s `link_kit` pins the inputs of the link kit
(`//tools/buck2/native-toolchain:kit`, `../buck2/native-toolchain/link_kit.py`),
which every executable links with instead of a compiler driver:

- Debian's glibc 2.28-10+deb10u1 `libc6` and `libc6-dev` and Linux 4.19
  `linux-libc-dev` for amd64 and arm64 (archive.debian.org);
- LLVM 23.1.5's `libunwind` and `compiler-rt` from its combined source archive;
- Zig's `lib/libc/glibc/abilists` at `bff490a2`.

Per target the kit holds `Scrt1.o`, `crti.o`, `crtn.o` and `libc_nonshared.a`
from glibc 2.28 unchanged; stub DSOs for libc, libm, libpthread, libdl, librt,
libutil, libresolv and the loader generated (Zig's `.symver` technique) from
the 2.28 libraries' public versioned symbols with their kinds, sizes, bindings
and default versions, cross-checked against abilists; `crtbeginS.o`/`crtendS.o`
from compiler-rt's `crtbegin.c`/`crtend.c`; and a static `libunwind.a` built with
LLVM's own static configuration against the 2.28 headers. `manifest.json`
records each file's SHA-256, source and flags, the clang and lld versions and
the SHA-256 of `ld.lld` and libLLVM; `digest` (the manifest's SHA-256) is the
kit's identity and enters every build ID. The runtime archive's own
`compiler_builtins` provides every compiler builtin, so the kit carries no
compiler-rt builtins. `:glibc_floor` unpacks the same `libc6` packages as
`<triple>/` loader and library directories for running executables at the
floor through that loader (never a link input).

```sh
.buck2/bin/buck2 test //tools/buck2/native-toolchain:proof
```

The proof runs clang's version command, links and runs C with the pinned lld,
builds a static AArch64 executable and runs it directly with the input qemu
(no binfmt), then checks the C executable with the input Valgrind and
`VALGRIND_LIB`. It checks the pinned clang/lld versions, requires output markers from both
executables and an explicit zero-error Valgrind summary, then requires
Valgrind to refuse a planted 32-byte leak with exit 97. A skipped step fails.
Run it after a pin or unpacking change, so the change exercises the tools on a
worker.

Valgrind's declared libc/debug dependency is included in its input tree.
The proof's C executable selects that loader and library directory, and
Valgrind finds the matching symbols beside each ELF in its standard `.debug/`
directory, by the original debuglink and build ID. Absolute
Debian usrmerge links are resolved inside the archive namespace before copying;
the unpack host's loader can never satisfy one of those links.
