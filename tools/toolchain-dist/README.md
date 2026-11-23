# Distribution compiler experiments

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

Training checks the retained native hello source and builds it with Cranelift and LLVM. Each command uses tools/capped and must return zero with a nonempty `.profraw`. The default CPU is 8; timing cores 12-15 and 28-31 are refused. The program cache is disabled. The matching profile tools come from the pinned Rust llvm-tools archive. `--profdata` can select an installed matching tool.

The PGO pipeline retains the existing tsrs training structure: instrument, run ordinary source commands, require nonempty profiles, merge, and rebuild with matching symbol metadata.

`NOTICES.md` at the repository root is the distribution's notice inventory. It is
generated, never edited: `tools/toolchain-dist/notices.py --write [--bundle DIR]`
derives it from Cargo.lock, Cargo metadata, the Buck link graph,
`tools/native-toolchain/llvm-23.lock`, the locked LLVM source archive and an
installation (by default it builds `//:hirsel-toolchain`). The archive is
`HIRSEL_LLVM_SOURCE_ARCHIVE`, or the copy under `$XDG_CACHE_HOME/hirsel/llvm-source`,
downloaded from `source_url` and checked against `source_sha256` when absent.
`notice-sources.json` classifies every file an installation ships (`files`):
first-party, linked crates (the compiler and the runtime archive, by their
workspace package and link root) or a native pin (`lib/libLLVM.so.23.1`,
`bin/ld.lld`). A native pin's notices are the LLVM source tree's license and
copyright-bearing files (`native_text_rule`) and the texts of the C and C++
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

On Rust toolchain or LLVM bumps, run
`tools/toolchain-dist/check-repro --output /tmp/hirsel-repro` from the commit
to qualify. It builds that commit without PGO in two fresh local checkouts
with separate Buck2 state and keeps both bundles in the output directory.
It compares every file and reports the first differing byte offset in each
changed file, starting at zero.

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
