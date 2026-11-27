# Distribution compiler experiments

`product.toml` declares the published hosts and their LLVM library paths, the
supported runtime targets and feature variants, and the installed helper
entries and socket names. To render the standalone installer from this checkout,
run `python3 tools/toolchain-dist/product.py installer > install.sh`.

The `//tools/buck2:dist` platform and Cargo `--profile dist` use fat LTO and
one codegen unit. The CLI already uses mimalloc v3 (`mimalloc 0.1.52`,
`libmimalloc-sys 0.1.49`, Buck2's v3/static.c fixup). Developer dev/release,
test tools, build scripts, proc macros and native runtime profiles stay unchanged.
No target CPU, panic policy or hot/cold linker grouping is changed.

PGO instruments Rust code, including third-party Rust libraries. The C mimalloc
fixup stays unchanged. Both passes use the same dist platform, resource requests,
fat LTO, strip setting and crate metadata: Buck2's `_metadata` keys symbols by
target and configuration hash. `hirsel.dist_pgo=generate|use` changes action
flags and inputs, not that platform. `profile.profdata` is a declared, ignored
Buck2 source, so its content participates in remote action keys. A checkout lock
serializes profile writers. Do not edit the tree between PGO passes.

Training counts differ from run to run (about 0.02%, from the compiler's own
scheduling), so two builds that each train produce different compilers. A
release therefore never trains. `build --train-profile DIR` instruments, trains
and merges once and writes `DIR/<sha256>.profdata`; `build --pgo
DIR/<sha256>.profdata` takes that file as a pinned input and refuses one whose
bytes do not hash to its name. `--pgo` without a profile is refused: a build
that trains cannot be reproduced, so it is not offered.

Training checks the retained native hello source and builds it with Cranelift and LLVM. Each command uses tools/capped and must return zero with a nonempty `.profraw`. The default CPU is 8; timing cores 12-15 and 28-31 are refused. The program cache is disabled. The matching profile tools come from the pinned Rust llvm-tools archive. `--llvm-profdata` can select an installed matching tool.

The PGO pipeline retains the existing tsrs training structure: instrument, run ordinary source commands, require nonempty profiles, merge, and rebuild with matching symbol metadata.

Set `RELEASE_TAG` for bundle builds and profile training. Use an explicit
unpublished tag, such as `v0.0.0-local`, for local builds.

`tools/toolchain-dist/build --pgo DIR/<sha256>.profdata --bolt DIR/<sha256>.fdata`
rewrites the PGO-use compiler with the same BOLT options and release workloads
as the engine library, from a pinned BOLT profile. BOLT's training counts
differ from run to run like PGO's (about 0.008%), so, as with PGO, a release
never trains: `--train-profile DIR` writes the BOLT profile beside the PGO one,
trained on the PGO-use compiler that profile builds, and `--bolt` refuses bytes
that do not hash to the name. A BOLT profile is valid only for the exact binary
it was trained on, so `--bolt` requires `--pgo` and the two must come from one
`--train-profile` run at the same source and `RELEASE_TAG` (the version is
compiled in). Training and every build print `dist: BOLT input sha256`; the
digests match when the profile fits. `hirsel-build.json` records
`profdata_sha256` and `fdata_sha256`. Install the pinned tools with
`tools/native-toolchain/install --bolt-tools DIR` and put `DIR/bin` on PATH.
The bundle builds its installed package products after the rewrite, using the
resulting compiler and its build ID. The compiler's DWARF is decompressed for
the rewrite and compressed again for the distribution. The work directory
(`--work`, default `OUTPUT-work`) keeps `bolt/hirsel` (BOLT's input) and
`bolt/hirsel.bolt` (its output). Before the rewrite is kept, BOLT's input and
output each build every M1 program, which the profile never sees (it trains on
the release std and cold LangArena only); the build fails unless each program's
`.text` is byte-identical, and both sides' user CPU time is printed, ungated.
The nightly board builds with `DIST_BOLT=off`, so it never measures this
compiler; the benchmarks page in the separate hirsel-site repository says so. The
compiler is rewritten with `--bolt-info=0`: BOLT's note records its command line, which names temporary
directories. Given one input and one profile, the rewrite is byte-identical.

The release workflow starts with a fast `plan` job that resolves the version and
host targets, then runs `build-linux` and `build-darwin` in parallel.
`build-linux` runs `--train-profile` once, then immediately runs `--pgo` and
`--bolt` with those pinned profile hashes inside the same warmed container, runs
the glibc and fresh-root gates, and publishes `<sha256>.profdata` and
`<sha256>.fdata` beside the archive; `release-validate.yml` (`repro`) separately
rebuilds from those published profiles in clean checkouts at different paths to
verify bit-for-bit reproducibility. A `workflow_dispatch` run builds a
`v0.0.0-dispatch.<run id>` version and publishes nothing unless dispatched on a
`v*` tag with `publish`.

`NOTICES.md` at the repository root is the distribution's notice inventory. It is
generated, never edited: `tools/toolchain-dist/notices.py --write [--bundle DIR]`
derives it from Cargo.lock, Cargo metadata, the Buck link graph,
`tools/native-toolchain/llvm-23.lock`, the locked LLVM source archive and an
installation (by default it builds `//:hirsel-toolchain`). The archive is
`HIRSEL_LLVM_SOURCE_ARCHIVE`, or the copy under `$XDG_CACHE_HOME/hirsel/llvm-source`,
downloaded from `source_url` and checked against `source_sha256` when absent.
`notice-sources.json` classifies shipped files in `files`. The compiler and
runtime archives name their workspace packages and link roots. The LLVM library,
linker and archive tools name their native pins. A native pin's notices are the
LLVM source tree's license and copyright-bearing files (`native_text_rule`) and the texts of the C and C++
runtimes it links statically. The manylinux_2_28 build host's AlmaLinux
runtime notices and the pinned link kit's source notices are in `licenses/`.

`//:gate-distribution-notices` fails when a crate Cargo.lock
reaches from the shipped workspace packages has no row, when a native row is
not its llvm-23.lock pin, when a vendored text changed, or when the built
installation has a file no `files` class covers or a native file that is not
its pin. A dependency change therefore lands with `notices.py --write`.

Release packaging writes `notices/manifest.json`, `notices/crates/`,
`notices/rust/`, `notices/native/`, `notices/adapted-source/` and a copy of
`NOTICES.md` beside the compiler. Copy or archive the **whole output
directory**, including `notices/`. The GitHub release workflow archives that
directory and publishes it with `SHA256SUMS` and `install.sh`. The installer
puts Hirsel in `~/.hirsel` and exposes a single `hirsel` symlink on `PATH`.
The bare Buck `hirsel-toolchain` target is a build artifact; use
`tools/toolchain-dist/build` for the redistributable output. Packaging recomputes
the whole inventory, refuses when `NOTICES.md` differs, and refuses a bundle
`check_bundle` does not cover.

`notices.py` walks the compiler's and the runtime's configured Buck link edges
and identifies crate archives from source edges, then joins them to locked Cargo
metadata. It excludes build scripts and proc-macro executables, and inventories
linked inputs before dead-code elimination. It reads license-file inputs and all
upstream LICENSE/COPYING/COPYRIGHT/NOTICE files (including nested C and assembly
licenses), plus full text files with embedded copyright or SPDX notices,
from Cargo's cached archives after checking their Cargo.lock SHA-256.
Run Cargo metadata to populate an absent cache. Missing archives, declared
license files, required native texts or Rust notices fail packaging and remove
the incomplete output. No license text is synthesized from a package name or
SPDX identifier. All supplied alternatives are retained without electing an
alternative; a missing Cargo license expression is reported as a manifest gap.
`required_texts` names the explicitly required mimalloc v3 and
ring/BoringSSL/fiat/once_cell texts.

For listed Wasmtime workspace crates that omit their license, packaging fetches
the upstream root license at the exact Git revision recorded in the locked
archive. It checks the crate's source path and declared license, then verifies
the license bytes against the pinned SHA-256. Run
`python3 tools/toolchain-dist/notices.py --check-crates` after Buck preparation
to check both release link graphs without building the bundles.

The manifest records the pinned archive provenance, license expressions,
upstream text paths and hashes, source/build-input hashes, and every bundled
file's hash. It has no timestamps, checkout paths or host identifiers. Notice
collection checks that existing file bytes did not change; compiler identity,
ordinary std products remain untouched. `hirsel-build.json` records the
notice manifest hash and a canonical digest of the `core-packages` file map.
The host loader, libc, libm, libgcc_s, libstdc++ and libz the compiler and
libLLVM load are system dependencies and are not shipped.

Every packaged build renames the prefix and uses the compiler there to create
a project, compile a std-using program with both native backends, and check
both executables' stdout. It restores the prefix after both runs pass and
removes the bundle on failure.

Run `tools/toolchain-dist/build --check-fresh PREFIX` on a host with `readelf`
and `bwrap`. The gate checks all bundled ELF version requirements against the
installer's glibc 2.28 floor. It then mounts the bundle read-only in an empty
Linux root with just the loader and runtime libraries. From that root it runs
`hirsel new demo && cd demo && hirsel test && hirsel run && hirsel build`, then
executes `./program` and checks the package. The installation's link kit provides
the CRT and link-time libraries.

## The macOS bundle

Buck2 does not run on macOS. On an Apple Silicon Mac with rustup, from a checkout
of the commit:

```sh
RELEASE_TAG=v0.0.0-local tools/toolchain-dist/build --host aarch64-apple-darwin --output /tmp/hirsel-darwin --work /tmp/hirsel-darwin-work
```

`darwin_host.py` builds with Cargo what Buck2 builds on Linux, from the same pins:
the compiler in Cargo's `dist` profile against the pinned LLVM tree, the standalone
and embed runtime archives (fat-LTO staticlibs of `hirsel-core-rt` against the trace-only std, with
`tools/buck2/profile.bzl`'s runtime flags), and the Darwin link kit's text stubs. The
bundle ships `hirsel`, the linker and archive tools, and `lib/libLLVM.23.1.dylib` from the pinned
tree, the runtimes and link kit for `aarch64-apple-darwin` only, the core and std
images, the toolchain packages and the helpers in `libexec/hirsel/`. The last two
come from `toolchain_programs.py`, which Buck2's bundle runs too. Every Mach-O is
built with `MACOSX_DEPLOYMENT_TARGET` from `link_kit.DARWIN_FLOOR` and carries an
ad-hoc signature. The bundle includes `notices/` and `hirsel-build.json` with the
compiler, core package and notice digests. Its crate notices cover the compiler
and runtime's Darwin link inputs.

Release smoke builds with the Mac bundle's own runtime. For the byte comparison,
it also links the same demo using the Darwin runtime archive from the Linux
bundle and compares that image with the Linux cross build.

The build then checks the bundle, as
`tools/toolchain-dist/build --host aarch64-apple-darwin --check-fresh PREFIX` does
for any unpacked bundle. Each Mach-O's load commands must match its file class:

| File | Linked libraries | LC_RPATH | Other |
|---|---|---|---|
| `hirsel` | libSystem, CoreFoundation, Security, libiconv | none | |
| `bin/ld.lld` | libSystem, libc++, libz, `@rpath/libLLVM.23.1.dylib` | `@loader_path/../lib` | same bytes as the pinned tree |
| `bin/llvm-ar`, `bin/llvm-objcopy` | libSystem, libc++, `@rpath/libLLVM.23.1.dylib` | `@loader_path/../lib` | same bytes as the pinned tree |
| `lib/libLLVM.23.1.dylib` | libSystem, libc++, libz | none | id `@rpath/libLLVM.23.1.dylib`, same bytes as the pinned tree |
| `libexec/hirsel/*` and the programs the check builds | a subset of the link kit's libraries | none | no initializer entries |
| `<program>.dSYM/.../DWARF/<program>` beside those | nothing | none | MH_DSYM with its program's UUID, unsigned |
| `core-packages/<std>/native/o/**` | nothing | none | MH_OBJECT, unsigned; the final link sets its platform version |

Every signed image is a thin arm64 file with LC_UUID, minos at the
floor, no segment whose maximum protection is both writable and executable, and a
signature that passes `codesign -v --strict`; a Mach-O of no class fails.
Without `MACOSX_DEPLOYMENT_TARGET`, rustc 1.99 stamps minos 11.0. The check then packs the bundle,
unpacks it into an empty read-only prefix and runs the fresh-machine sequence above
under `env -i` (PATH holding only the prefix, `/usr/bin` and `/bin`), then builds
the relocation program on both backends and checks every program it built.

On Rust toolchain or LLVM bumps, run
`RELEASE_TAG=v0.0.0-local tools/toolchain-dist/check-repro --output /tmp/hirsel-repro` from the commit
to qualify. It builds that commit without PGO (or with `--pgo` and `--bolt`,
as a release is built) in two fresh local checkouts at different paths, with
separate Buck2 state, and keeps both bundles in the output directory.
`--pgo PROFILE [--bolt FDATA]` builds both from those pinned profiles; bare
`--pgo [--bolt]` first trains both in the first checkout and keeps them under
`OUTPUT/profile/`.
It compares every file and reports the first differing byte offset in each
changed file, starting at zero. It then builds the x86_64 release std with the
first bundle at 1, 2 and 8 jobs and compares the three stores the same way:
programs key their parts on the release std's bytes, so they must not depend on
`HIRSEL_JOBS`.
`check-repro --output DIR --std-jobs PREFIX` runs only that comparison, on
an installed bundle.

Prior art: Rust release packaging at rust-lang/rust
`a7c37f9f0ee5e93396106e9de20f4a86b4154e63`,
`src/bootstrap/src/core/build_steps/dist.rs:678-697`, installs generated HTML
copyright inventories and the accompanying REUSE license directory. We retain
the pinned toolchain's `COPYRIGHT-library.html` and its complete `licenses/`
directory for the linked Rust libraries, instead of the compiler-wide inventory
for an unshipped rustc executable. Crate text selection follows the upstream
archive and its Cargo license-file declaration; ring 0.17.14 `LICENSE:1-9`
explicitly points to BoringSSL, ISC and once_cell texts. This differs from a
single chosen-license report: keeping all upstream texts preserves multiple
licenses and nested native notices without making a legal-policy choice.

Focused checks:

```sh
.buck2/bin/buck2 test //:gate-distribution-notices
python3 tools/tests/native_toolchain.py
```

Three published Wasmtime archives omit their license file: assembler-x64 0.136.3,
bitset 0.136.3 and internal-core 49.0.3. Explicit per-version fallbacks retain
root `LICENSE:1-219` from wasmtime `f96eabaa11a06086e4fb779ed1c1688481067ab9`,
using its identical copy in the locked cranelift-codegen archive. Both archives'
VCS revisions and the donor text hash must match. The manifest reports each
archive omission, donor checksum and upstream URL. This is a verified upstream
text, not a replacement chosen from its SPDX expression. License declarations
are in that revision's `cranelift/assembler-x64/Cargo.toml:5`,
`cranelift/bitset/Cargo.toml:6` and `crates/core/Cargo.toml:5`.
