//! Engine publication and settled views of immutable definition summaries.
use crate::*;
use hirsel_span::Span;
use hirsel_syntax::DefId;
#[cfg(test)]
use hirsel_types::TypeKind;
use hirsel_types::{GenericArgument, TypeId, TypeInterner};
mod facet;
pub(crate) use facet::SettledSummary;
pub use facet::SummaryView;

/// The compact published input of CT-5, staged selection and GEN-5. Its reads
/// share the memo's allocation; retaining it never retains a checked body.
/// Durable encodings use the same canonical type table as the other outputs.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PublishedSummary {
    /// Query identity; generated instances can retain another lexical owner.
    pub definition: DefId,
    pub summary: DefinitionSummary,
    pub growth: ParameterGrowth,
    /// Source-only compact equations. V reconstructs its own executable graph.
    #[serde(default)]
    pub storage: Option<std::sync::Arc<hirsel_types::storage::graph::StorageDefinition>>,
    #[serde(default)]
    pub checked: Option<std::sync::Arc<CheckedSummary>>,
    pub reads: std::sync::Arc<hirsel_query_products::reads::Reads>,
    /// The owner's body and package-manifest key, as for a body memo.
    pub text: hirsel_syntax::ContentHash,
}

impl PublishedSummary {
    pub(crate) fn share_checked_storage(&mut self) {
        if let Some(checked) = &self.checked {
            if self.summary.frames == checked.summary.frames {
                self.summary.frames = checked.summary.frames.clone();
            }
            if self.summary.calls == checked.summary.calls {
                self.summary.calls = checked.summary.calls.clone();
            }
            if self.summary.staged == checked.summary.staged {
                self.summary.staged = checked.summary.staged.clone();
            }
            if self.growth.edges == checked.growth.edges {
                self.growth.edges = checked.growth.edges.clone();
            }
        }
    }
}
