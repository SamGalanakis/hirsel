//! Immutable checked front products and definition-summary production.
#[cfg(test)]
hirsel_guard::install!();

use hirsel_syntax::ItemSyntax;
use hirsel_types::{CheckedBody, NameSite, TypeId};
use std::sync::Arc;

/// H's complete definition-local result. Every TypeId is a session interner row.
#[derive(Clone, Debug)]
pub struct TypedBody {
    pub syntax: Arc<ItemSyntax>,
    pub checked: CheckedBody,
    pub plans: hirsel_check_protocol::body::Plans,
    pub unit: TypeId,
    pub boolean: TypeId,
}
/// J/I's complete result, refined together and cached as one artifact.
#[derive(Clone, Debug)]
pub struct AnalyzedBody {
    pub typed: Arc<TypedBody>,
    pub ownership: hirsel_types::OwnershipFacts,
    pub results: Vec<(NameSite, TypeId)>,
    pub call_allocations: Vec<(hirsel_syntax::ExprId, hirsel_types::Allocation)>,
}
/// The compile-time products lowering reads beside a borrowed analyzed body.
#[derive(Clone, Debug, Default)]
pub struct Comptime {
    /// Evaluated subexpressions, which lowering emits as values.
    pub materialized: Vec<(hirsel_syntax::ExprId, hirsel_tir::Materialized)>,
    /// Shared reception types of hidden size-generic constants.
    pub staged: Vec<(hirsel_syntax::ExprId, TypeId)>,
}

mod summary;
pub use summary::{
    CallClass, CheckedSummary, DefinitionSummary, Frame, FrameKind, GenericParameterRef,
    GrowthEdge, ParameterGrowth, SummaryCall, growing_sites, parameters, source_site, summarize,
};
