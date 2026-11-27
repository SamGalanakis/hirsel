# Release compile memory (implementation)

This is an implementation document, not part of the language specification. It states how an
optimised LLVM build keeps its peak memory bounded. The build's two steps, publication of a
release recipe and assembly from it, are specified in
[launch](../spec/launch.md#release-recipe-and-host-assembly). The 2 GiB measured-process
reservation and the `HIRSEL_MEMORY_LIMIT` data limit ([compiler limits](compiler-limits.md)) are
fixed. The compiler fits under them; nothing raises them, and no work moves out of the measured
process to fit.

## End state

A release build passes through three phases. Each phase frees the previous phase's state before
it allocates its own:

1. **Check and publish.** The checker produces the checked product. Reach walks it from the
   entry and records only compact reach. Publication writes the recipe's bodies, groups, annexes
   and values to the code store, and keeps the encoded bodies (`UnitModule::Encoded`) and the
   recipe in memory.
2. **Hand off.** The checked product and its reach are dropped once every recipe the request
   needs is published. Assembly takes the publication, the reach counters and the library
   image's store path, and nothing else from the frontend. The frontend's definition frames,
   instruction order, layout interning and the checked bodies are freed before any Core IR is
   decoded.
3. **Assemble by partition.** Assembly cuts the recipe's units into release partitions of about
   `partition_instructions()` Core instructions (250,000 by default, the bound
   `emit_release_unit` already lowers and optimises by). For partition *k* it decodes those
   units' bodies, joins them with the std side's lent declarations, lowers, optimises and emits
   the partition's objects to the content store, and then frees the decoded Core IR and the LLVM
   module. Only then does it decode partition *k+1*. The link reads objects from the store.

The peak bound is

    peak ≈ R + E + max_k (D_k + L_k)

where *R* is the publication (the recipe and the encoded bodies, a few bytes per Core
instruction), *E* is the emitted objects held for the link (or zero once objects go straight to
the store), *D_k* is partition *k*'s decoded Core IR and *L_k* is its LLVM module through the
optimisation pipeline. *D_k* and *L_k* scale with the largest partition, not with the package,
and the partition bound caps them. Checking state never adds to the assembly peak, so the
checker's peak and the assembly's peak are separate maxima, not a sum.

On Linux the data limit counts committed memory, not resident memory. mimalloc decommits freed
pages with `MADV_DONTNEED`, which lowers the resident set but leaves the mapping writable and
counted. The data-limit peak is therefore the larger of the checker's committed high-water and
whatever assembly commits beyond the pages the checker freed. The bound holds under the data
limit only when the checker's own peak fits and assembly's live peak fits inside the pages the
checker returned. Calling `mi_collect(true)` after the hand-off does not change this: the pages
are already returned, and the counted mapping stays.

A request that builds several tests in one process (`test --link-only`) publishes them all
before the hand-off. *R* is then the sum of their publications. Each publication shares the
encoded bodies it reached, so *R* grows with the reached code, not with the test count times the
package. A test session that runs tests as it builds them keeps its checked product for later
tests and so does not get the hand-off yet (next slices).

## What other compilers do

| Compiler | Unit of backend work | What is freed before or during the backend | Taken here |
| --- | --- | --- | --- |
| rustc | Codegen units: a crate splits into up to 16 (256 when incremental) LLVM modules fixed at monomorphization collection, code-generated in parallel ([rustc book, `codegen-units`](https://doc.rust-lang.org/rustc/codegen-options/index.html#codegen-units); [rustc dev guide, code generation](https://rustc-dev-guide.rust-lang.org/backend/codegen.html)) | Each CGU's LLVM module is built, optimised and emitted, then dropped | ADAPTED: release partitions are the CGUs. Hirsel cuts by instruction count after reach, so a partition's size is bounded, where a CGU count is fixed per crate |
| Go | One package per compiler run. The backend compiles functions one at a time, longest first, on `-c` workers. Downstream packages read export data (unified IR, decoded lazily through an index) instead of rechecking ([cmd/compile README](https://go.dev/src/cmd/compile/README); [gc/compile.go](https://go.dev/src/cmd/compile/internal/gc/compile.go)) | Typechecking finishes before the function queue drains | ADOPTED: check, then hand a compact form to the backend. The encoded recipe body plays the part of export data and is decoded only when its partition is emitted |
| GCC LTO (WHOPR) | LGEN compiles each file to IR with summaries. WPA reads only summaries, decides, and splits the program into partitions "of roughly the same size". LTRANS then compiles the partitions in parallel ([GCC internals, LTO overview](https://gcc.gnu.org/onlinedocs/gccint/LTO-Overview.html)) | WPA "avoids the need to load the whole program into memory" | ADOPTED: partitions of bounded size, each loaded only when compiled. The whole-program decision (std side selection, lent bodies) runs on the recipe, not on decoded bodies |
| LLVM ThinLTO | The thin link merges per-module summaries serially. Backends optimise each module in parallel and import what they need. Monolithic LTO "is not scalable in time or memory" ([Clang ThinLTO](https://clang.llvm.org/docs/ThinLTO.html)) | Each backend holds only its module and its imports | ADAPTED: a partition decodes only its own units plus the std side's lent declarations, the counterpart of ThinLTO's imports |

All four separate a whole-program step that reads compact data from a backend that loads one
bounded unit at a time. Hirsel already has the compact data (the recipe and its encoded bodies)
and the bounded unit (`emit_release_unit`'s partitions). What it lacks is the ordering:
before slice 1 the checked product was still alive during assembly, and all units are still
decoded and joined (`join_modules`) before the first partition is lowered.

## Slices

1. **Hand off before assembly in `test --link-only`** (landed with this document). The command
   configures and publishes every selected test, drops the checked product, and then assembles
   each publication. Before, every test was assembled while the product was alive. For
   platform's `test_local_creates_to_max_places` on Linux ELF, peak resident memory fell from
   1.93 GiB (refused) to 1.59 GiB, and assembly now runs at 1.2 GiB resident. The data limit
   still refuses it at 2 GiB committed: the checker's high-water (about 1.85 GB) plus about
   250 MB that assembly commits beyond it. `hirsel build --release` and `run --release` already
   hand off (commit 5b680336).
2. **Decode by partition.** `Publication::std_side` decodes every unit, and `release_objects`
   joins all modules, before `emit_release_unit` cuts partitions. Choose the partitions from the
   recipe (encoded sizes and the call graph), then decode, join, lower and emit one partition at
   a time. This removes the about 560 MiB of decoded Core IR that 3172's profile showed, so
   assembly's live peak fits inside the pages the checker returned.
3. **Hand off in test sessions.** `TestSession` keeps its checked product for later tests. Give
   each release unit a published recipe at preparation and assemble from it, so the session drops
   the product once every launch is published.
4. **Bound the publication.** Account *R* per request, and for many-test requests assemble from
   the stored recipe root (`Publication::open`) instead of holding every publication. Today
   `open` decodes all bodies eagerly and gives no memo hits, so it needs slice 2 first.
5. **Lower the checker's peak.** The checker's committed high-water (about 1.85 GB for
   platform) is then the data-limit peak. 3172's profile puts 227 MB in frontend definition
   framing and 747 MB under `check_lower`. Free each module's lowering inputs once its bodies are
   encoded, so the checker's peak is bounded by its largest module plus the encoded package.
