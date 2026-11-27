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

mod apple_records;
pub mod charge;
mod continuation;
pub mod cross;
pub mod event;
pub mod flow;
mod foreign;
pub mod isa;
pub mod law;
pub mod map_hash;
pub mod map_index;
mod pieces;
mod recipe;
mod shape;
pub mod show;
pub mod source_positions;
mod target;

pub use continuation::{Frames, Point, frames, spare_words};
pub use foreign::{foreign_cleanup, foreign_kind, next_foreign_handle};
pub use pieces::{LEAF_PIECES, holed_pieces, leaf_pieces};
pub use target::{
    AppleRecord, Arch, CAbi, CExtension, ObjectFormat, Os, OutputKind, Target, c_extension,
};

/// Reserve a shape before filling its references, using the caller's type identity.
pub fn intern_shape<K: Eq + std::hash::Hash, S: std::hash::BuildHasher>(
    shapes: &mut Vec<Row>,
    rows: &mut std::collections::HashMap<K, TypeRef, S>,
    key: K,
    shape: Row,
) -> (TypeRef, bool) {
    match rows.entry(key) {
        std::collections::hash_map::Entry::Occupied(entry) => (*entry.get(), false),
        std::collections::hash_map::Entry::Vacant(entry) => {
            let row = TypeRef(shapes.len() as u32);
            shapes.push(shape);
            entry.insert(row);
            (row, true)
        }
    }
}

pub use recipe::{
    CField, CLOSURE_CARRIER, CaseOps, Cleanup, Compact, CompactSet, DropRecipe, Encoding,
    FieldDrop, Fixed, Layout, LayoutError, MapEntry, Niche, POINTER, Pos, Recipe, RecipeRow,
    RecipeScratch, SEQ_CARRIER, Size, Tag, case_of, case_word, cleanup_class, column_strides,
    columns, compact_extent, compact_starts, compute, extend, field_niche, field_size, held_size,
    map_entry, read_case, storage_free, tag_width, terminal, word,
};
pub use shape::{
    Body, Carried, Case, Field, FieldTy, FuncRef, Handle, Repr, Row, Scalar, Shape, Ty, TypeRef,
    Vector, Width,
};

#[cfg(test)]
mod tests;

/// Physical bytes available for the first storage of an eligible local list.
pub const FRAME_LIST_BYTES: u64 = 128;
/// A loaned list's frame slot also holds its allocation link and original payer.
pub const FRAME_LIST_LINK_BYTES: u64 = 24;

/// FrozenBytes metadata for uncharged static data. The low bit marks static
/// storage; the length is stored in the capacity word.
pub const FROZEN_STATIC_UNCHARGED: u64 = 1;
