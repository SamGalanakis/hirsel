//! Generator declaration selection and the failure a demanded generator
//! scope reports before the final immutable front is captured.
use super::*;

/// Why a demanded generator scope refused before source publication.
pub enum FrontRegistrationBootstrapError {
    Admission(QueryError),
    Runtime {
        reference: hirsel_engine_store::Reference,
        error: Option<QueryError>,
        sources: SourceMap,
        owner: Arc<FrontRegistrationQueries>,
    },
    Acceptance(FrontRegistrationFailure),
}
impl std::fmt::Debug for FrontRegistrationBootstrapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Admission(error) => f.debug_tuple("Admission").field(error).finish(),
            Self::Runtime {
                reference, error, ..
            } => f
                .debug_struct("Runtime")
                .field("reference", reference)
                .field("error", error)
                .finish_non_exhaustive(),
            Self::Acceptance(error) => f.debug_tuple("Acceptance").field(error).finish(),
        }
    }
}
impl QueryEngine {
    /// Select written generator declarations without executing or checking a
    /// body. The caller keeps this order as the source publication prefix.
    pub fn front_generator_roots(
        &mut self,
        definitions: &[DefPath],
    ) -> Result<Vec<DefPath>, QueryError> {
        let mut roots = Vec::new();
        for path in definitions {
            if self
                .source_facts(path)
                .is_some_and(|facts| !facts.generator)
            {
                continue;
            }
            self.materialize(path)?;
            if generator(self.snapshot(path)?.as_ref()).is_some() {
                roots.push(path.clone());
            }
        }
        Ok(roots)
    }
}
