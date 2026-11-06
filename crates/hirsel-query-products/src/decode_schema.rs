//! Consumer allocation bounds for compact summaries and depth evidence.
use crate::summary::*;
use hirsel_product_codec::decode_schema::{Scan, Walk};
use hirsel_product_codec::{decode_structure as structure, decode_variants as variants};
variants!(GenericParameterRef { Type(f0), Size(f0) });
variants!(FrameKind {
    Item,
    LocalFunction,
    Closure,
    ComptimeRegion
});
variants!(CallClass {
    Runtime,
    ComptimeExecuted,
    StaticSelection
});
structure!(Frame {
    site,
    parent,
    kind,
    generic,
    size_bindings
});
structure!(SummaryCall {
    callee,
    binder,
    arguments,
    owning_frame,
    target_frame,
    class,
    site
});
structure!(DefinitionSummary {
    owner,
    frames,
    calls,
    staged
});
structure!(ParameterGrowth { edges });
structure!(CheckedSummary { summary, growth });
impl<S: Walk> Walk for crate::depth::DepthEvidence<S> {
    fn walk(scan: &mut Scan<'_>) -> Option<()> {
        u32::walk(scan)?;
        Box::<[Option<S>]>::walk(scan)?;
        bool::walk(scan)
    }
}
