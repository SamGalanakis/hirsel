//! Internal staged evaluation and ordinary executable classification.
use super::*;
pub(crate) mod demand;
mod scope;
pub use scope::{FrontDemandAdmission, FrontDemandError, FrontDemandScope};
/// Whether an item has a body to lower: a function with one, a constant or a
/// finalizer. A declaration with implementation binders has none; its
/// specializations do (GEN-6).
pub(crate) fn executable(syntax: &ItemSyntax) -> bool {
    matches!(
        &syntax.item.kind,
        ItemKind::Function { body: Some(_), .. }
            | ItemKind::Const { .. }
            | ItemKind::Type { drop: Some(_), .. }
    ) && !abstract_family(syntax)
}
/// Whether an item declares implementation binders it does not bind (GEN-6).
pub(crate) fn abstract_family(syntax: &ItemSyntax) -> bool {
    matches!(
        &syntax.item.kind,
        ItemKind::Function { signature, .. } if signature.generics.iter().any(|g|
            matches!(g.kind, hirsel_syntax::GenericKind::Implementation { .. }))
    ) && syntax.item.implementation_bindings.is_none()
}
pub(in crate::queries) fn locate_failure(
    mut error: QueryError,
    sources: &SourceMap,
    site: Span,
) -> QueryError {
    if let QueryError::Operational(failure) = &mut error
        && let Some(location) = sources.location(site)
        && let Some(file) = sources.file(sources.resolve(site).file)
    {
        failure.message = format!(
            "{}:{}:{}: {}",
            file.logical_path().display(),
            location.line,
            location.column,
            failure.message
        );
    }
    error
}
mod instances;
