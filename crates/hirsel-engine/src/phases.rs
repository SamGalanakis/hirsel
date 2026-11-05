//! Front phases outside the engine's dependency set: signature normalization,
//! body checking, modes and lowering. hirsel-driver implements [`Phases`] once;
//! the engine reaches those crates only through this trait object, so none of
//! their code is instantiated here.
use crate::*;
use hirsel_check_protocol::header::{Input, NormalizedDeclaration};
use hirsel_span::Span;
use std::sync::Arc;

/// The compiler's front phases. [`PhaseAdapters::compiler`] installs them beside
/// the engine's own lexer, parser, resolver, formatter and interpreter adapters.
pub trait Phases: Send + Sync {
    /// Normalizes one written header and its private defining shape (LANG-6).
    fn normalize_signature(
        &self,
        input: Input<'_>,
        provider: &mut dyn SignatureProvider,
    ) -> Result<NormalizedDeclaration, QueryError>;
    /// Validates a `loop_source` association even when no loop uses its type.
    fn validate_loop_source(
        &self,
        t: &hirsel_types::TypeInterner,
        m: &hirsel_types::TypeMemo,
        signature: &Signature,
        span: Span,
        provider: &mut dyn SignatureProvider,
    ) -> Result<(), QueryError>;
    /// Validates a `format` association even when no f-string formats its type.
    fn validate_format(
        &self,
        t: &hirsel_types::TypeInterner,
        m: &hirsel_types::TypeMemo,
        signature: &Signature,
        span: Span,
        provider: &mut dyn SignatureProvider,
    ) -> Result<(), QueryError>;
    /// Validates a `serializable(check = f)` association even when nothing decodes it.
    fn validate_serializable_check(
        &self,
        t: &hirsel_types::TypeInterner,
        m: &hirsel_types::TypeMemo,
        signature: &Signature,
        span: Span,
        provider: &mut dyn SignatureProvider,
    ) -> Result<(), QueryError>;
    fn checker(&self) -> Arc<dyn CheckerPhase>;
    /// Modes publish diagnostic-stage products with their query result.
    fn modes(&self) -> Arc<dyn ModesPhase>;
    fn lowering(&self) -> Arc<dyn LoweringPhase>;
}

/// A compiler without front phases: the engine's own adapters, for unit tests
/// that only parse or resolve.
#[cfg(test)]
pub(crate) fn engine_only_adapters() -> PhaseAdapters {
    struct Absent;
    impl Phases for Absent {
        fn normalize_signature(
            &self,
            _: Input<'_>,
            _: &mut dyn SignatureProvider,
        ) -> Result<NormalizedDeclaration, QueryError> {
            Err(QueryError::NotImplemented)
        }
        fn validate_loop_source(
            &self,
            _: &hirsel_types::TypeInterner,
            _: &hirsel_types::TypeMemo,
            _: &Signature,
            _: Span,
            _: &mut dyn SignatureProvider,
        ) -> Result<(), QueryError> {
            Err(QueryError::NotImplemented)
        }
        fn validate_format(
            &self,
            _: &hirsel_types::TypeInterner,
            _: &hirsel_types::TypeMemo,
            _: &Signature,
            _: Span,
            _: &mut dyn SignatureProvider,
        ) -> Result<(), QueryError> {
            Err(QueryError::NotImplemented)
        }
        fn validate_serializable_check(
            &self,
            _: &hirsel_types::TypeInterner,
            _: &hirsel_types::TypeMemo,
            _: &Signature,
            _: Span,
            _: &mut dyn SignatureProvider,
        ) -> Result<(), QueryError> {
            Err(QueryError::NotImplemented)
        }
        fn checker(&self) -> Arc<dyn CheckerPhase> {
            unreachable!("engine_only_adapters installs no checker")
        }
        fn modes(&self) -> Arc<dyn ModesPhase> {
            unreachable!("engine_only_adapters installs no modes")
        }
        fn lowering(&self) -> Arc<dyn LoweringPhase> {
            unreachable!("engine_only_adapters installs no lowering")
        }
    }
    let phases: Arc<dyn Phases> = Arc::new(Absent);
    PhaseAdapters {
        lexer: Some(Arc::new(LexerAdapter)),
        parser: Some(Arc::new(ParserAdapter)),
        resolver: Some(Arc::new(ResolverAdapter)),
        signature: Some(Arc::new(SignatureAdapter {
            phases: phases.clone(),
        })),
        formatter: Some(Arc::new(FormatterAdapter)),
        phases: Some(phases),
        ..Default::default()
    }
}
