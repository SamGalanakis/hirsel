# Numeric kernel family

The three layouts run the same checked u64 stencil, deterministic indexed gather
and checked reduction. Fixed uses 64 inline elements. AoS stores value/index pairs;
SoA stores those fields in separate lists. All use the same scratch stencil.
`generate.py` writes the fixed seed42 test and run configurations and independent
integer checksum oracles. `malformed.json` has n1 and must exit nonzero.

Use the shared correctness entry with module `layout/numeric/pack` and the exact
`Layout::Numeric::{Fixed,AoS,SoA}` name. `--helper-laws` checks the zero-budget
Capacity, a growth refusal after two admitted values, the retained prefix, checked
overflow, and agreement between layouts. These controls run on both native engines.

`numeric.rs` is the safe crate-free matched twin. Compile it with the fixed flags
in core/MEASURE.md and edition2024. Its default variant checks arithmetic; a separate
`--cfg wrapping` variant wraps the stencil/reduction additions. Report those two
Rust variants separately. Matching finite outputs do not make their overflow
policies interchangeable. No store-energy or interpreter-step mode exists in the
new language. Those retired modes are outside this family.

Logical storage at n elements is 24n bytes for AoS and SoA, in three independently
rounded allocations for SoA and two for AoS. All buffers stay live through gather
and reduction and are released at kernel return. The inline64 variant allocates
nothing. Runtime charged/retained/slack instrumentation is pending; logical source
sizes are not measurements of physical RSS or runtime allocation peaks.

Prior art: Goose docs/design/soa.md gives the AoS/SoA comparison; Rust slices and
fixed arrays give the safe indexed loops. Adapted to checked Hirsel arithmetic,
portable capacity admission and an explicit failure-prefix law. No ISA-specific
optimization, alias assumption or floating contraction is used.
