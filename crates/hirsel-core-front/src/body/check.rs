use super::*;

/// Work one body check did, for the cost measurements (core/MEASURE.md).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Counts {
    /// Expression, statement and pattern nodes visited, and steps deciding whether an
    /// expression synthesises its type.
    pub nodes: u64,
    /// Names read.
    pub uses: u64,
    /// Place reads and writes of `let mut` slots.
    pub places: u64,
    pub calls: u64,
    /// Type comparisons and type nodes lowered.
    pub types: u64,
    /// Bytes of the emitted body's tables.
    pub bytes: u64,
}
impl std::ops::AddAssign for Counts {
    fn add_assign(&mut self, o: Self) {
        self.nodes += o.nodes;
        self.uses += o.uses;
        self.places += o.places;
        self.calls += o.calls;
        self.types += o.types;
        self.bytes += o.bytes;
    }
}

/// The outcome of checking and lowering one definition.
#[derive(Debug, Default)]
pub struct CoreCheck {
    pub refusals: Vec<Refusal>,
    pub resource_refusal: Option<crate::ResourceRefusal>,
    pub gaps: Vec<Gap>,
    pub counts: Counts,
    /// The function row whose body was emitted; None when the body was refused, reached
    /// a gap, or the definition has no body.
    pub function: Option<FuncId>,
    /// Layout/type relocations for a rigid generic body, checked once.
    pub template: Option<GenericTemplate>,
    pub flow: flow::Facts,
    /// The types this body's contextual constructors resolved to.
    pub constructor_types: std::collections::BTreeSet<DefPath>,
    /// A contextual constructor resolved by searching its whole candidate set
    /// (no expected type), so every candidate of that name took part.
    pub constructor_candidates: bool,
    /// The computed constants this body reads (LANG-20): their values enter its
    /// instances' record keys and its groups' constant slots, never its body.
    pub computed: std::collections::BTreeSet<DefPath>,
}

impl CoreCheck {
    pub(super) fn merge_refusals(
        &mut self,
        refusals: Vec<Refusal>,
        resource: Option<crate::ResourceRefusal>,
    ) -> bool {
        let refused = !refusals.is_empty() || resource.is_some();
        self.refusals.extend(refusals);
        self.resource_refusal = self.resource_refusal.take().or(resource);
        refused
    }
}
