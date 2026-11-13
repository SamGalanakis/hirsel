//! Layout recipes for declared shapes and the logical charge policy. Own and
//! resource records and variants, their drop recipes and `charge-v1` are FORM v1a.
//! FORM v1b adds, frozen: inline arrays (SEM-9), stable and scoped records, compact
//! records and active-size variants (MEM-9, MEM-11), `Text` and `FixedSpan` (MEM-12),
//! links (MEM-10), closure environments and the standalone and span-builder charges;
//! at spec 6ef88514, `SeqSpan` and `IndexedSpan` of packed elements (MEM-12), the
//! packed encoding of plain records, locators and the `seq.index_in` charge. See
//! the `hirsel-core-ir` crate docs.
//!
//! Types and recipe computation only: no IR, no code generation, no checking of
//! programs beyond the representation refusals a declaration's shape implies
//! (MEM-9, MEM-13, MEM-17). The checker computes recipes once per module; both
//! native emitters, the evaluator and drop-glue generation read them.

#[cfg(test)]
hirsel_guard::install!();

pub mod charge;
mod recipe;
mod shape;

pub use recipe::{
    CField, CLOSURE_CARRIER, Compact, CompactSet, DropRecipe, FieldDrop, Fixed, Layout,
    LayoutError, MapEntry, POINTER, Pos, Recipe, SEQ_CARRIER, Size, Tag, compact_extent,
    compact_starts, compute, extend, field_size, map_entry, storage_free, tag_width,
};
pub use shape::{
    Body, Carried, Case, Field, FieldTy, FuncRef, Handle, Repr, Scalar, Shape, Ty, TypeRef, Width,
};

#[cfg(test)]
mod tests;
