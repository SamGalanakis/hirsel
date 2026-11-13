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

The PGO pipeline retains the existing tsrs training structure: instrument, run ordinary source commands, require nonempty profiles, merge, and rebuild with matching symbol metadata. Its training corpus is temporary until the new core has native programs. No compiler performance claim follows from this deletion.

Release packaging writes `notices/manifest.json`, `notices/crates/`,
`notices/rust/` and `notices/adapted-source/` beside the compiler. Copy or archive
the **whole output directory**, including `notices/`; there is no user-directory
installation step. The bare Buck `hirsel-toolchain` target is a build artifact;
use `tools/toolchain-dist/build` for the redistributable output.

`notices.py` walks the compiler's configured Buck link edges and identifies
crate archives from source edges, then joins them to locked Cargo metadata.
It excludes build scripts and proc-macro executables, and inventories linked
inputs before dead-code elimination. It reads license-file inputs and all
upstream LICENSE/COPYING/COPYRIGHT/NOTICE files (including nested C and assembly
licenses), plus full text files with embedded copyright or SPDX notices,
from Cargo's cached archives after checking their Cargo.lock SHA-256.
Run Cargo metadata to populate an absent cache. Missing archives, declared
license files, required native texts or Rust notices fail packaging and remove
the incomplete output. No license text is synthesized from a package name or
SPDX identifier. All supplied alternatives are retained without electing an
alternative; a missing Cargo license expression is reported as a manifest gap.
`notice-sources.json` records the bundle boundary and the explicitly required
mimalloc v3 and ring/BoringSSL/fiat/once_cell texts. Changing the bundle contents
requires updating that boundary; unexpected top-level files are refused.

The manifest records the pinned archive provenance, license expressions,
upstream text paths and hashes, source/build-input hashes, and every bundled
file's hash. It has no timestamps, checkout paths or host identifiers. Notice
collection checks that existing file bytes did not change; compiler identity,
ordinary std products remain untouched. `hirsel-build.json` records the
notice manifest hash and a canonical digest of the `hirsel-std.products` file map.

The standard bundle does not copy native runtime archives, the link kit, LLVM
shared libraries, clang or host system libraries. Their external status and
pin paths are explicit in the manifest. Native checks still select those
inputs through the existing environment. `tools/native-toolchain/install
--unpack DIR` retains the pinned Debian packages' `usr/share/doc/*/copyright`
through its full `dpkg -x` extraction. Extending the release to carry any of
those code objects requires extending the notice inventory too; this collector
does not claim to cover an added native toolchain tree.

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
