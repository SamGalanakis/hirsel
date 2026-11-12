//! Layout recipes for declared shapes and the logical charge policy. Own and
//! resource records and variants, their drop recipes and `charge-v1` are FORM v1a
//! (frozen); stable, scoped and compact representations, spans, text, links and
//! closure environments are FORM v1b (draft). See the `hirsel-core-ir` crate docs.
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
    LayoutError, POINTER, Pos, Recipe, SEQ_CARRIER, Size, Tag, compact_extent, compact_starts,
    compute, tag_width,
};
pub use shape::{Body, Case, Field, FieldTy, FuncRef, Handle, Repr, Scalar, Shape, TypeRef, Width};

#[cfg(test)]
mod tests;
