//! Immutable checked front products and definition-summary production.
#[cfg(test)]
hirsel_guard::install!(test_alloc::Counting: test_alloc::Counting);

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

/// Cache domains keep outputs of different phases distinct (PROG-5).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Ord, PartialOrd)]
pub enum QueryKind {
    Lex,
    Format,
    Interpret,
    Parse,
    Resolve,
    Signature,
    Modes,
    Check,
    Lower,
    Summary,
    CheckedSummary,
    Registration,
    Evaluate,
    Generate,
    GeneratedRegistration,
    SourceAudit,
    SemanticBody,
    PrivateInference,
}
impl QueryKind {
    pub fn phase_name(self) -> &'static str {
        match self {
            Self::Lex => "lexing",
            Self::Parse => "parsing",
            Self::Resolve => "name resolution",
            Self::Signature => "signature normalization",
            Self::Check => "body checking",
            Self::Modes => "mode checking",
            Self::Lower => "TIR lowering",
            Self::Format => "formatting",
            Self::Interpret => "interpretation",
            Self::Summary => "definition summary",
            Self::CheckedSummary => "checked definition summary",
            Self::Registration => "staged declaration registration",
            Self::Evaluate => "compile-time evaluation",
            Self::Generate => "generated source evaluation",
            Self::GeneratedRegistration => "generated instance registration",
            Self::SourceAudit => "source allocation audit",
            Self::SemanticBody => "semantic source body",
            Self::PrivateInference => "private uniqueness inference",
        }
    }
}
/// Which executable bodies a compile lowers (and V validates). Lowering
/// reports source diagnostics only through compile-time evaluation (CT-1, CT-2,
/// CT-4), so both policies report the same diagnostics.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum LoweringPolicy {
    /// `check`: exactly the bodies compile-time evaluation executes or
    /// materializes into: constants, bodies containing or reading a compile-time
    /// value, the functions evaluation calls, and the generated instances CT-5
    /// selects.
    Comptime,
    /// `run`, `test` and `build`: every body of the program's own package and
    /// the dependency bodies it reaches.
    Reachable,
}

pub mod depth;
pub mod reads;
pub mod retained;
use hirsel_span::Span;
use hirsel_syntax::{ContentHash, DefId, DefPath};
use hirsel_types::SignatureDependency;
/// Reads survive errors and missing phases, rather than being lost in QueryError.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default)]
pub struct DependencyReads {
    pub operational_failure: Option<hirsel_types::OperationalFailure>,
    pub signatures: Vec<SignatureDependency>,
    pub failed_signatures: Vec<DefPath>,
    pub failed_signature_keys: Vec<(DefPath, Option<ContentHash>)>,
    pub lookups: Vec<(hirsel_types::ReadPath, bool)>,
    pub manifest_lookups: Vec<hirsel_types::SharedRead>,
    pub resolver: Arc<hirsel_types::SharedPath<hirsel_types::SharedRead>>,
    /// Other items' name sites a result cites, as read (`cite_name`).
    pub citations: Vec<(DefPath, Option<Span>)>,
    /// A compact header's semantic closure: the definitions its dropped
    /// signature and resolved lookup observations named.
    pub closure: Vec<DefId>,
}

impl DependencyReads {
    /// Every definition the semantic closure of this detail names.
    pub fn semantic_definitions(&self) -> impl Iterator<Item = DefId> + '_ {
        let reads = self.resolver.iter().chain(&self.manifest_lookups);
        self.signatures
            .iter()
            .map(|dependency| dependency.definition)
            .chain(reads.filter_map(|read| {
                match read.as_ref() {
                    hirsel_types::DependencyRead::Signature { definition, .. } => Some(*definition),
                    hirsel_types::DependencyRead::Lookup {
                        result: Ok(definition),
                        ..
                    } => Some(DefId::intern(definition.as_ref())),
                    hirsel_types::DependencyRead::ProviderLookup { output, .. } => output
                        .result
                        .as_ref()
                        .ok()
                        .map(|definition| DefId::intern(definition.as_ref())),
                    _ => None,
                }
            }))
            .chain(self.closure.iter().copied())
    }
}

#[cfg(test)]
use hirsel_engine_admission::test_alloc;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

/// Declaration observations needed by checking without retaining syntax.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SourceFacts {
    pub generator: bool,
    pub instance_bodied: bool,
    pub executable: bool,
    pub body: bool,
    pub constant: bool,
    pub execution: bool,
    pub specializes: bool,
    pub staged: bool,
    pub comptime: Vec<(u32, u32, u32)>,
    pub fact: bool,
    pub associations: Vec<(u8, u32, u32)>,
    pub arms: Vec<(String, u32, u32)>,
}
