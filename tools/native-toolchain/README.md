# native-toolchain

The pinned LLVM for native code is the 23.1.5 release-branch snapshot at
`a738f2737189bcc1de75c25e8cd1d4b466c9526e`, packaged on 2026-10-28.
Its debs and matching source archive are retained at
https://github.com/SamGalanakis/hirsel-lang/releases/tag/toolchain-llvm-23.1.5.

- `llvm-23.lock`: the pin, the only place it is written. LLVM 23.1.5 from the
  `llvm-toolchain-noble-23` packages at one exact version, with each `.deb`'s SHA-256 and
  size; the mirror release tag, upstream commit and source tarball's SHA-256; and
  llvm-sys 231.0.1 with its crates.io checksum.
- `install`: prints the install command and pinned asset URLs; `--run` fetches and verifies
  the debs, installs those local files as root and holds the packages; `--unpack DIR`
  verifies every `.deb` and unpacks it under DIR without root, then prints the
  `LLVM_SYS_231_PREFIX` and loader path that select it; `--source` prints the pinned source
  build with a shared libLLVM.
  Both paths fetch only the retained GitHub assets, with no apt.llvm.org repository or
  pool URL. The hashes and sizes match its signed Packages and Sources indexes.
- `check [--prefix DIR]`: passes only on the exact pin. It rejects a wrong `llvm-config`
  major, minor, patch or suffix, a static-only install (no `libLLVM-23.so`), missing
  headers, a missing X86 or AArch64 target, a clang or lld of another version, an installed
  package at another version (system prefix only), and a Cargo.lock llvm-sys other than the
  pinned version and checksum. It then links `fixtures/version.c` against the shared
  libLLVM and requires it to report 23.1.5 at run time. It never searches PATH or falls back.
- `fixtures/`: `version.c` for `check`; `smoke.ll`, `smoke_harness.c` and `smoke.expected`,
  the FFI smoke that `crates/hirsel-llvm/tests/it/native.rs` compiles, links and runs.

- `build-static-lld DIR`, `static-lld-elf-only.patch`: build the toolchain image's
  `bin/ld.lld` (the lock's `static_lld` record). See below.
- `build-libllvm DIR ARTIFACTS`, `engine_llvm.py`: build the compiler's engine libLLVM
  (the lock's `engine_llvm` record) and guard it. See below.

## Static ld.lld

The toolchain image's `bin/ld.lld` is not the deb's. It is built once per pin
from the locked source archive by `build-static-lld`: the ELF port only
(`static-lld-elf-only.patch`), targets X86 and AArch64, Release without assertions,
linked `-static-pie` and stripped. It loads no library, so each link skips
the dynamic loader's work for libLLVM (about 18 M instructions per run). It is a static
PIE rather than plain `-static` because a `-static` binary's start-up registers
`.eh_frame` and libgcc then classifies every FDE (about 10 M instructions). The
product runs it as a child exactly as it ran the deb's `ld.lld`, and the toolchain
identity hashes it by content like every other recipe file.

To rebuild it after a pin change, run `build-static-lld DIR` on an x86_64 Ubuntu 24.04
host. It needs 4G and two jobs under `tools/capped`, and takes about an hour. Upload
`DIR/ld.lld` to the `deb_pool` release under the printed asset name, then replace the
`static_lld` record in `llvm-23.lock`. `tools/buck2/bootstrap_llvm.py` fetches that
asset, verifies its SHA-256 and size, and places it as `bin/ld.lld`. Nothing rebuilds
it per land.

The former LLVM 21 GitHub binary archive was static-only. `check` continues to
require a shared libLLVM; binary archives are not a fallback.

## Engine libLLVM

`build-libllvm` builds a libLLVM 23.1.5 for the compiler alone from the locked source
archive: profile-guided and ThinLTO, X86 and AArch64, zlib only. Its optimised builds
cost about 0.64 of the deb library's instructions warm and load none of z3, ICU,
libxml2, libedit, libffi or zstd. It runs three stages: an IR-instrumented library;
`engine_llvm.py train`, which builds the training programs through it (the LangArena
suite, every M1 program and the documents layout family, all generated from the
repository's own input helpers); and the profile-guided build.

Its guard, `engine_llvm.py guard`, builds the training programs and four held-out ones
(the graph and json layout families, the M2b table and dispatch_cross_package workloads)
through the deb library and the new one with the same compiler. It fails unless every
executable's `.text` is byte-identical and the warm build cost geomean is below the deb
library's and no worse than the lock's previous `engine_llvm_guard` ratio. `check` is the
install pin check above; this guard is separate because it needs a built compiler.

Run `build-libllvm DIR ARTIFACTS` once per pin change on an x86_64 Ubuntu 24.04 host.
It takes about 2.7 CPU-hours at 12 jobs. Then upload `DIR/libLLVM.so.23.1` under the
printed asset name and write both printed records into `llvm-23.lock`. Nothing rebuilds
it per land. `build-libllvm DIR ARTIFACTS plain` builds the same configuration without
profile or ThinLTO (about 0.88 warm): the fallback if a profile build fails its guard.

`tools/buck2/bootstrap_llvm.py` places the locked library as the tree's
`engine/libLLVM.so.23.1`. First-party binaries search `engine/` before `lib/` (their
RPATH, `tools/buck2/defs.bzl`), so the compiler loads it while clang and the other tools
keep the deb library, which libclang-cpp needs. The library carries no search path of
its own: the executable's RPATH finds its libz. `hirsel std-product --install` copies it, and
nothing else of the tree, into the prefix's `lib/`.

## Pool execution inputs

`BUCK` downloads LLVM's locked Debian packages and their runtime libraries,
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
