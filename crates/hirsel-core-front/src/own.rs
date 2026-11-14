//! The ownership part of the checker state (ownership.md, Checker state): availability
//! and its join (OWN-5, OWN-16), places and their overlap (OWN-9), and the stack of
//! freezes and reservations (OWN-10, OWN-11, OWN-12). All of it lives in per-body
//! vectors indexed by binder id; nothing is keyed by name.

/// A slot's availability (OWN-5).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Avail {
    Uninit,
    Live,
    Moved,
    Maybe,
}

/// The join table of OWN-16.
pub(crate) fn join(a: Avail, b: Avail) -> Avail {
    use Avail::*;
    match (a, b) {
        (x, y) if x == y => x,
        (Uninit, Moved) | (Moved, Uninit) => Moved,
        _ => Maybe,
    }
}

/// How a slot was introduced: a parameter's mode (OWN-7) or a `let`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Origin {
    ViewParam,
    PatternView,
    MutParam,
    TakeParam,
    Let,
    LetMut,
}
impl Origin {
    /// Assigning, replacing or lending `mut` needs a writable slot (OWN-5).
    pub(crate) fn writable(self) -> bool {
        matches!(self, Origin::MutParam | Origin::TakeParam | Origin::LetMut)
    }
}

/// One step of a place, as overlap sees it (OWN-9): two different fields of a record
/// are disjoint; two indexes, or two variant payloads, of one value may not be.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PStep {
    Field(u32),
    /// The payload of a `Box`, `Rc` or `Arc` (OWN-8).
    Payload,
    Index {
        collection: crate::types::TyId,
        value: Option<hirsel_core_ir::ValueId>,
    },
    Any,
}

/// A slot and a range of steps in the body's step arena.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Place {
    pub slot: u32,
    pub start: u32,
    pub len: u32,
}
impl Place {
    pub(crate) fn whole(slot: u32) -> Self {
        Place {
            slot,
            start: 0,
            len: 0,
        }
    }
}

/// OWN-9: one place is a prefix of the other, or they differ first at an index or a
/// payload. Different fields of one record do not overlap.
pub(crate) fn overlap(steps: &[PStep], a: Place, b: Place) -> bool {
    if a.slot != b.slot {
        return false;
    }
    let sa = &steps[a.start as usize..(a.start + a.len) as usize];
    let sb = &steps[b.start as usize..(b.start + b.len) as usize];
    for (x, y) in sa.iter().zip(sb) {
        match (x, y) {
            (PStep::Field(i), PStep::Field(j)) if i != j => return false,
            (PStep::Field(_), PStep::Field(_)) => {}
            _ => return true,
        }
    }
    true
}

/// How long a view lasts (OWN-8).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Extent {
    /// To the end of its statement.
    Temp,
    /// To the end of the block at this depth.
    Block(u32),
}

/// What holds a place (OWN-10, OWN-11, OWN-12).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Hold {
    /// A live view freezes its anchor.
    View(Extent),
    /// A view argument of an open call: the call and the argument index.
    CallView(u32, u32),
    /// A `mut` argument of an open call, lent until the call completes.
    CallMut(u32),
    /// The place of an assignment, reserved while its right side is evaluated.
    Assign,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Freeze {
    pub place: Place,
    pub hold: Hold,
    /// The construct that froze it: an operation crossing the hold cites it (PROG-7).
    pub site: hirsel_core_syntax::NodeId,
}

/// What an operation does to a place, for the holds it may not cross.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Act {
    /// Read or view.
    Read,
    /// Move, lend `mut`, assign, replace, destructure or drop.
    Write,
}

/// The rule a hold breaks for `act`, if it does: within one call the arguments'
/// overlap (OWN-9), a view's freeze (OWN-10), an assignment's reservation (OWN-12).
pub(crate) fn breaks(hold: Hold, act: Act) -> Option<&'static str> {
    match (hold, act) {
        (Hold::CallMut(_), _) => Some("OWN-9"),
        (_, Act::Read) => None,
        (Hold::CallView(..), Act::Write) => Some("OWN-9"),
        (Hold::View(_), Act::Write) => Some("OWN-10"),
        (Hold::Assign, Act::Write) => Some("OWN-12"),
    }
}
