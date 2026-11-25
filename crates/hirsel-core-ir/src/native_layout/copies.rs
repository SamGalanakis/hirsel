//! How a native emitter moves a whole fixed value with `Inst::Copy`: as integer
//! pieces (hirsel_core_layout's `holed_pieces` and `leaf_pieces`) or as one
//! byte copy. Cranelift and LLVM both lower each copy by [`split`].

use crate::{AddressBase, AddressId, Body, Loan, Module, Param, PlaceTy, Signature};
use hirsel_core_layout::{Layout, RecipeRow, Repr};

/// A copy split into naturally aligned integer pieces.
pub struct Split {
    /// (offset, width) of each piece, width at most 8.
    pub pieces: Vec<(u64, u64)>,
    /// The value's alignment; a piece is aligned to the lesser of this and
    /// its width.
    pub align: u64,
}

/// The integer pieces that `Copy { to, from, bytes }` in `body`, of signature
/// `sig` when the body is a function's own, moves in place of its bytes, with
/// `bytes` the copy's constant length; None copies the bytes. Only a whole
/// copy of one fixed own value splits.
///
/// A `take` parameter's place was written by the caller just before the
/// call, field by field, where this body cannot see it. A wider load than
/// those stores cannot forward from them and waits for them to retire, so
/// such a copy moves each leaf at its own width. Any other source keeps the
/// holed two-piece rule, which leaves forwarding of whole copies to the
/// optimizer that sees both sides.
pub fn split(
    module: &Module,
    recipes: &[RecipeRow],
    body: &Body,
    sig: Option<&Signature>,
    to: AddressId,
    from: AddressId,
    bytes: Option<u64>,
) -> Option<Split> {
    let PlaceTy::Declared(t) = body.addresses[to.index()].ty else {
        return None;
    };
    let Layout::Fixed(f) = &recipes[t.index()].layout else {
        return None;
    };
    if body.addresses[from.index()].ty != PlaceTy::Declared(t)
        || matches!(module.types[t.index()].repr, Repr::Resource | Repr::Region)
        || bytes != Some(f.size)
    {
        return None;
    }
    let row = hirsel_core_layout::TypeRef(t.0);
    let pieces = match sig.is_some_and(|sig| taken(body, sig, from)) {
        true => hirsel_core_layout::leaf_pieces(&module.types, recipes, row),
        false => None,
    }
    .or_else(|| hirsel_core_layout::holed_pieces(&module.types, recipes, row))?;
    Some(Split {
        pieces,
        align: f.align,
    })
}

/// Whether `from` is the whole place of a `take` parameter of `sig`. A loop
/// back to the entry block rebinds its parameters, so then none is.
fn taken(body: &Body, sig: &Signature, from: AddressId) -> bool {
    let source = &body.addresses[from.index()];
    let AddressBase::Value(v) = source.base else {
        return false;
    };
    if source
        .steps
        .get(&body.pools.steps)
        .is_some_and(|s| !s.is_empty())
    {
        return false;
    }
    let entry = body.blocks[0]
        .params
        .get(&body.pools.values)
        .unwrap_or_default();
    let Some(at) = entry.iter().position(|&e| e == v) else {
        return false;
    };
    let param = at
        .checked_sub(usize::from(sig.dest.is_some()))
        .and_then(|i| sig.params.get(i));
    matches!(
        param,
        Some(Param::Address {
            loan: Loan::Take,
            ..
        })
    ) && !body.edges.iter().any(|edge| edge.target.index() == 0)
}
