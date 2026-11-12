//! Independent source instance identity and source call reference.
use hirsel_core_contract::OperationId;
use hirsel_syntax::DefPath;

/// A source call's identity within its independently supplied definition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReferenceInvocation {
    pub body: u32,
    pub block: u32,
    pub statement: u32,
    pub restart: bool,
    pub primitive: Option<OperationId>,
    pub callee: Option<DefPath>,
}

/// Body selected by the independent source instance plan. Candidate anchors
/// cannot select which source invocation inventory they are checked against.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReferenceBody {
    pub definition: DefPath,
    pub instance: hirsel_syntax::ContentHash,
    pub body: u32,
}

hirsel_codec::snapshot_struct!(ReferenceBody {
    definition,
    instance,
    body
});

hirsel_codec::snapshot_struct!(ReferenceInvocation {
    body,
    block,
    statement,
    restart,
    primitive,
    callee
});
