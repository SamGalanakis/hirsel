//! Ordinary descriptor leaves use the installed decoder's fixed-int walk.
use super::*;
use crate::{decode_structure as structure, decode_variants as variants};
use hirsel_diag as diag;
use hirsel_tir as tir;
structure!(diag::Label { span, message });
structure!(diag::Fix {
    span,
    replacement,
    message
});
variants!(diag::Severity {
    Error,
    Warning,
    Note
});
variants!(diag::Obligation { Multiplicity, CopyProof, GenericBound { arguments }, ParameterMode { argument } });
structure!(diag::Diagnostic {
    rule,
    severity,
    message,
    primary,
    secondary,
    notes,
    fix,
    obligations,
    root_cause
});
structure!(hirsel_artifact_store::Reference {
    output,
    domain,
    len
});
structure!(hirsel_artifact_store::Domain { kind, schema });
impl Walk for hirsel_artifact_store::Output {
    fn walk(scan: &mut Scan<'_>) -> Option<()> {
        ast::ContentHash::walk(scan)
    }
}
structure!(tir::Materialized { root });
structure!(tir::ValueNode { ty, kind });
variants!(tir::ValueKind { Constant(f0), FunctionReference { definition, arguments }, Construct { constructor, fields } });
variants!(tir::Constant { Unit, Bool(f0), Integer(f0), F32(f0), F64(f0), String(f0), Bytes(f0) });
variants!(tir::Constructor { Tuple, Record(f0), Variant { definition, variant }, Sequence, Map, Set, SortedMap { by }, Box });
structure!(tir::Operand { place, usage });
impl Walk for tir::PlaceId {
    fn walk(scan: &mut Scan<'_>) -> Option<()> {
        u32::walk(scan)
    }
}
variants!(ty::UseKind {
    Borrow,
    Take,
    Copy,
    Mut
});
variants!(ty::Size { Constant(f0), Parameter(f0) });
variants!(ty::GenericArgument { Type(f0), Size(f0) });

structure!(ty::SignatureDependency {
    definition,
    fingerprint
});
variants!(tir::Storage {
    Parameter,
    Local,
    Temporary,
    Capture,
    ReturnSlot
});
structure!(tir::Local {
    ty,
    storage,
    scope,
    span
});
variants!(tir::Projection { Field(f0), Index(f0), VariantField { variant, field } });
structure!(tir::Place {
    loan,
    binding,
    root,
    projection,
    ty
});
structure!(tir::Scope { parent, span });
structure!(tir::ConstructField { field, value });
variants!(tir::Rvalue { Constant(f0), ReadConstant { definition, arguments }, FunctionReference { definition, arguments }, Size(f0), Use(f0), Unary { op, value }, Binary { op, left, right }, Construct { constructor, fields, destination }, Closure { context, body, captures, destination } });
structure!(tir::Cleanup { head });
variants!(tir::CleanupAction { Drop { place, if_live }, EndLoan { loan, if_active }, ReleaseRegion(f0), CloseArena(f0), JoinTasks { scope, cancel } });
structure!(tir::CleanupNode { action, parent });
structure!(tir::BorrowCursor {
    carrier,
    initializer,
    cover
});
variants!(tir::TailKind { SelfTail, ConstructorHole(f0) });
structure!(tir::TailSite {
    owner_body,
    call_block,
    kind,
    owned_loop_arguments
});
structure!(tir::ScopedParameter { parameter, fields });
variants!(tir::OriginBindingKind { Alias, Fresh });
variants!(tir::SourceEventKind { Call, Iteration, CountedIteration, CountedEntry { from, to, inclusive }, Entry });
structure!(tir::SourceEvent { kind, source });
variants!(tir::InstructionKind { Defuse { place }, BindOrigin { source, place, kind }, BindFields { source, fields }, EndOrigin(f0), StorageLive(f0), StorageDead { place, if_live }, EndScope(f0), Assign { destination, value }, Replace { destination, value, displaced, if_live }, BeginLoan { id, place, mutable, if_inactive }, EndLoan { loan, if_active }, Drop { place, if_live }, RunCleanup(f0), Check { condition, rule }, Charge(f0) });
structure!(tir::Instruction {
    kind,
    span,
    on_failure
});
structure!(tir::CallArgument { parameter, value });
structure!(tir::CallReturn { destination, next });
structure!(tir::ArenaScopeGrant {
    intrinsic,
    callback_parameter,
    arena_parameter
});
structure!(tir::Block {
    instructions,
    terminator,
    span
});
structure!(tir::MatchArm {
    pattern,
    guard,
    target
});
variants!(tir::MatchPatternKind { Wildcard, Binding { usage, guard_usage }, Literal(f0), Range { start, end }, Tuple(f0), Record { definition, fields }, Variant { definition, variant, fields } });
structure!(tir::MatchPattern { place, kind });
structure!(tir::MatchTable {
    scrutinee,
    patterns,
    arms
});
structure!(tir::Body {
    arena_scope_grant,
    parameter_scope,
    locals,
    places,
    scopes,
    blocks,
    rvalues,
    cleanups,
    cleanup_nodes,
    entry,
    parameters,
    result,
    borrow_cursors,
    tail_sites,
    matches,
    scoped_parameters
});
structure!(tir::ProtocolAssociationReceipt {
    association,
    body,
    open,
    next
});
structure!(tir::Definition {
    caller_bodies,
    protocol_associations,
    types,
    path,
    signature_hash,
    bodies,
    entry,
});
variants!(tir::Callee { Definition { path, arguments }, Indirect { forward_caller, value, arguments } });
variants!(tir::Terminator { Goto(f0), Match { table, from }, Branch { condition, yes, no }, Switch { value, arms, otherwise }, Call { callee, arguments, returns, on_failure }, Return { value, cleanup }, Fail { rule, cleanup }, Unreachable });

scalar!(tir::LocalId=>4, tir::BlockId=>4, tir::ScopeId=>4, tir::CleanupId=>4, tir::CleanupNodeId=>4, tir::BodyId=>4, tir::LoanId=>4, tir::RvalueId=>4, tir::MatchTableId=>4, tir::MatchPatternId=>4, ty::LocalId=>4, ty::RegionId=>4);
structure!(ty::Origin { root, fields });
variants!(ty::OriginRoot { ArgumentBrand(f0), Expression(f0,f1), Parameter(f0), Local(f0,f1), Definition(f0), Sibling(f0) });
variants!(ty::CaptureContext {
    Scoped,
    OwningScoped,
    Escaping,
    Task
});
structure!(crate::TirHeader {
    version,
    path,
    signature_hash,
    entry
});
structure!(crate::TirBodyHeader {
    arena_scope_grant,
    parameter_scope,
    entry,
    result
});
variants!(ty::Destination { Heap, Arena(f0), Caller(f0), Region(f0), Inferred });
// hirsel-types/src/generics.rs:9-21,418-426 own these private serde fields.
// BoundDeclarations has the same allocation layout as (i64, Arc<[GenericParameter]>, Option<Arc<()>>).
impl Walk for ty::GenericDeclarations {
    fn walk(scan: &mut Scan<'_>) -> Option<()> {
        type Declarations =
            Arc<std::collections::BTreeMap<ast::SiteRef, Arc<[ty::GenericParameter]>>>;
        Declarations::walk(scan)?;
        Declarations::walk(scan)?;
        loop {
            match scan.read_u8()? {
                0 => break,
                1 => {
                    scan.add(
                        size_of::<(i64, Arc<[ty::GenericParameter]>, Option<Arc<()>>)>()
                            + 2 * size_of::<usize>(),
                    )?;
                    i64::walk(scan)?;
                    Arc::<[ty::GenericParameter]>::walk(scan)?;
                }
                _ => return None,
            }
        }
        i64::walk(scan)?;
        Option::<ty::LexicalContext>::walk(scan)
    }
}
impl Walk for ty::LexicalContext {
    fn walk(scan: &mut Scan<'_>) -> Option<()> {
        scan.add(size_of::<ty::LexicalDeclarations>() + 2 * size_of::<usize>())?;
        std::collections::BTreeMap::<ast::SiteRef, (Vec<usize>, Arc<[ty::GenericParameter]>)>::walk(scan)?;
        Vec::<(usize, usize)>::walk(scan)?;
        usize::walk(scan)
    }
}

structure!(ty::ProtocolAssociation {
    source,
    fingerprint
});
