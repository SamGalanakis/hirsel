use super::{PlaceTy, ValueId};
use hirsel_core_layout::{Cleanup, FieldTy, RecipeRow, Row, TypeRef, cleanup_class};
use std::sync::Arc;

/// What an `Alloc` makes. The block's bookkeeping (its limit record,
/// and for a resource its cause cell and grant) is the engine's.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq, Hash)]
pub enum AllocKind {
    /// Own storage for the owning edge that will hold it (crate docs, "Cleanup
    /// classes"): `box tN`, `seq E`, `map K V`, or
    /// `rc tN` / `arc tN`, a counted node: the count (u64, starting at 1) at offset 0,
    /// the payload at 8. `arc` makes a node whose every later `share`, `release`
    /// and `try_unwrap` is an event (MEM-15); its operations name the same kind. The edge's [`hirsel_core_layout::cleanup_class`] fixes the block's
    /// native prefix.
    Own { edge: Arc<FieldTy> },
    /// An `own fn` environment (`alloc env tN`, crate docs "Callable
    /// environments"): word 0 names row `tN`'s fixed environment descriptor and
    /// the captures follow from offset 8, laid out as row `tN`. The alloc writes
    /// word 0 (an engine writes the descriptor's address); it is never charged
    /// (MEM-24). Its class is Fixed: the drain cleans it from that descriptor.
    Environment { ty: TypeRef },
    /// Region storage (MEM-1): freed when its region closes, never by an edge. Only
    /// `in` a region, and every region allocation is stable.
    Region,
    /// A resource block: `charge` covers its storage, its cause cell and `grant`.
    Resource { grant: u64 },
}

impl AllocKind {
    /// Put the allocation edge out of line so other instructions stay small.
    pub fn own(edge: FieldTy) -> Self {
        Self::Own {
            edge: Arc::new(edge),
        }
    }

    /// The count kind of a counted node (an `rc` or `arc` edge).
    pub fn counted(&self) -> Option<Counted> {
        let Self::Own { edge } = self else {
            return None;
        };
        match edge.as_ref() {
            FieldTy::Rc(_) => Some(Counted::Rc),
            FieldTy::Arc(_) => Some(Counted::Arc),
            _ => None,
        }
    }
    /// The cleanup class of the block (crate docs, "Cleanup classes"); `None` for
    /// region storage, which no edge cleans.
    pub fn cleanup(&self, shapes: &[Row], recipes: &[RecipeRow]) -> Option<Cleanup> {
        match self {
            AllocKind::Own { edge } => cleanup_class(shapes, recipes, edge),
            AllocKind::Environment { .. } => Some(Cleanup::Fixed),
            AllocKind::Region => None,
            AllocKind::Resource { .. } => Some(Cleanup::Resource),
        }
    }
    /// Whether the block is terminal: the 8-byte payer prefix, freed with nothing
    /// to drop (crate docs, "Cleanup classes").
    pub fn terminal(&self, shapes: &[Row], recipes: &[RecipeRow]) -> bool {
        self.cleanup(shapes, recipes) == Some(Cleanup::Terminal)
    }
}

/// The owning `seq E` edge of a sequence carrier place, which `grow` replaces the
/// storage of (crate docs, "Growth"): a row whose only field is that edge, as
/// `List[T]` (`items`) and `String` (`bytes`) are. None for any other place.
pub fn sequence_edge(shapes: &[Row], place: PlaceTy) -> Option<&FieldTy> {
    let PlaceTy::Declared(t) = place else {
        return None;
    };
    match &shapes.get(t.index())?.body {
        hirsel_core_layout::Body::Record(fields) => match fields.as_slice() {
            [f] if matches!(f.ty, FieldTy::Seq(_)) => Some(&f.ty),
            _ => None,
        },
        hirsel_core_layout::Body::Variant(_) => None,
    }
}

/// Which limit an `Alloc` is charged to.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum AllocPlace {
    /// The current limit (MEM-19).
    Current,
    /// Like allocation uses the current limit, as does growth (MEM-26).
    Like(ValueId),
    /// FORM v1b: in this region, charged to the current limit until it closes (MEM-1).
    Region(ValueId),
    /// Frame storage with a live allocation record understood by lent callees.
    /// The aligned slot includes FRAME_LIST_LINK_BYTES after its payload.
    LentFrame(ValueId),
    /// The current limit. A short byte buffer may reserve disjoint tail backing.
    ByteHead,
    /// The current limit. A nonnull pointer is data exclusively lent from a
    /// nonempty owning Bytes carrier, which may have an unused tail.
    ByteTail(ValueId),
    /// First aligned byte sequence of a pair for fixed file I/O.
    FileBuffer,
    /// Partner of an exclusively lent first sequence, paid independently.
    FilePartner(ValueId),
}

impl AllocPlace {
    /// Runtime entry and optional first-buffer operand for a paired byte allocation.
    pub fn byte_allocator(self) -> Option<(&'static str, Option<ValueId>)> {
        Some(match self {
            Self::ByteHead => ("hirsel_core_alloc_byte_head", None),
            Self::ByteTail(head) => ("hirsel_core_alloc_byte_tail", Some(head)),
            Self::FileBuffer => ("hirsel_core_alloc_file_buffer", None),
            Self::FilePartner(head) => ("hirsel_core_alloc_file_partner", Some(head)),
            _ => return None,
        })
    }
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Counted {
    Rc,
    /// Every operation is an event in the run's order (MEM-15).
    Arc,
}
