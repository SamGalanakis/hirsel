//! Fixed-int consumer schemas. Accessors witness field types without values.
use super::*;
use crate::{decode_structure as structure, decode_variants as variants};
structure!(ast::TokenRange { start, end });
structure!(ast::Origin { span, tokens });
structure!(ast::PackageId { name, version });
structure!(ast::ItemPath {
    package,
    modules,
    name
});
impl Walk for ast::DefPath {
    fn walk(scan: &mut Scan<'_>) -> Option<()> {
        field(|v: &Self| &v.0, scan)?;
        Some(())
    }
}
variants!(ast::Visibility {
    Private,
    Public,
    PublicOpaque
});
structure!(ast::Path { package, segments });
structure!(ast::LoopSourceSyntax { open, next, origin });
structure!(ast::FormatAssociation { function, origin });
structure!(ast::CheckAssociation { function, origin });
variants!(ast::StatePromiseKind {
    AdvanceOnly,
    PreserveCaptures,
    PreserveUnique,
    Captures
});
structure!(ast::StatePromiseSyntax {
    kind,
    selectors,
    origin
});
variants!(ast::Keyword {
    Fn,
    Let,
    Mut,
    Match,
    If,
    Else,
    For,
    In,
    While,
    Break,
    Continue,
    Return,
    Take,
    Copy,
    Unique,
    Type,
    Distinct,
    Flat,
    Resource,
    Drop,
    Box,
    Pub,
    Opaque,
    Const,
    Comptime,
    Shape,
    Heap,
    Before,
    Requires,
    Ensures,
    Result,
    Where,
    Uses,
    Of,
    Local,
    Global,
    Once,
    Todo,
    Extern,
    Unsafe,
    True,
    False
});
variants!(ast::AttributeArgument { Name(f0), LoopSource(f0), Format(f0), SerializableCheck(f0), StatePromise(f0), Keyword(f0), Integer(f0), String(f0) });
structure!(ast::Attribute {
    name,
    arguments,
    origin
});
variants!(ast::Bound {
    Plain,
    Flat,
    Data,
    Key,
    Sendable,
    Inert,
    Copyable,
    NoallocDrop,
    Serializable
});
structure!(ast::Sharing {
    member,
    binder,
    other,
    origin
});
variants!(ast::GenericKind { Type(f0), Size, Implementation{signature,sharing} });
structure!(ast::GenericParam { name, kind });
variants!(ast::PassingMode { Borrow, Take, Mut });
structure!(ast::Parameter {
    label,
    name,
    passing,
    ty
});
variants!(ast::ContractKind { Requires, Ensures });
structure!(ast::ContractSyntax { kind, expression });
structure!(ast::FunctionSyntax {
    attributes,
    generics,
    parameters,
    result,
    uses,
    contracts
});
structure!(ast::ForeignAbi { literal, contract });
variants!(ast::TypeFlavor {
    Ordinary,
    Distinct,
    Flat,
    Resource
});
structure!(ast::FieldSyntax {
    growable,
    name,
    ty,
    origin
});
structure!(ast::VariantSyntax { name, fields });
variants!(ast::TypeBody { Record(f0), Variants(f0), Alias(f0), Distinct{field,fact}, Opaque });
structure!(ast::DropSyntax {
    noalloc,
    binding,
    body
});
variants!(ast::MemberSpec { Type{name,bounds,authority,origin}, Size{name,origin}, Operation{name,signature,origin} });
variants!(ast::MemberDefinition { Type{name,ty,origin}, Size{name,value,origin}, Operation{name,target,origin} });
variants!(ast::ItemKind { Function{signature,body,abi}, Const{ty,value}, Type{flavor,generics,bounds,authority,body,drop}, Signature{members}, Implementation{signature,members} });
variants!(ast::Generated { Body(f0), Generator(f0), Specialization(f0) });
structure!(ast::Item {
    name_span,
    origin,
    name,
    definition,
    visibility,
    attributes,
    kind,
    generated
});
variants!(ast::Punct {
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Comma,
    Colon,
    Semicolon,
    Dot,
    DotDot,
    DotDotEq,
    At,
    Question,
    Arrow,
    FatArrow,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Bang,
    Tilde,
    Amp,
    Pipe,
    Caret,
    AndAnd,
    OrOr,
    Shl,
    Shr,
    Eq,
    EqEq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    PlusEq,
    MinusEq,
    StarEq,
    SlashEq,
    PercentEq,
    AmpEq,
    PipeEq,
    CaretEq,
    ShlEq,
    ShrEq
});
variants!(ast::FormatPiece {
    Spec,
    Whole,
    Head,
    Middle,
    Tail
});
variants!(ast::TriviaKind {
    Space,
    Newline,
    LineComment,
    BlockComment,
    DocLine,
    DocBlock
});
variants!(ast::TokenKind { Ident(f0), Label(f0), Keyword(f0), Punct(f0), Integer, Float, Character, Byte, String, Bytes, Format(f0), Trivia(f0), Invalid, Eof });
structure!(ast::Token { kind, span });
variants!(ast::Literal { Integer(f0), Float(f0), Character(f0), Byte(f0), String(f0), Bytes(f0), Bool(f0) });
structure!(ast::Argument { label, value });
variants!(ast::UnaryOp {
    Neg,
    Not,
    BitNot,
    Take,
    Copy,
    Mut,
    Unique,
    Box
});
variants!(ast::BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Shl,
    Shr,
    BitAnd,
    BitOr,
    BitXor,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
    Range,
    RangeInclusive
});
structure!(ast::MatchArm {
    pattern,
    guard,
    body
});
structure!(ast::FormatSpec {
    fill,
    align,
    width,
    precision,
    flags,
    kind
});
variants!(ast::FormatStep { Append, AppendSpec(f0), Mark, Pad(f0), Finish });
structure!(ast::StaticBinding {
    name,
    implementation,
    origin
});
variants!(ast::ExprKind { Literal(f0), Path(f0), Tuple(f0), Sequence(f0), Call{callee,arguments}, Field{base,field}, Index{base,index}, Unary{op,value}, Binary{op,left,right}, Try{value,map_error}, Block{label,statements,tail}, If{condition,then_branch,else_branch}, Match{value,arms}, Closure{signature,body}, Return(f0), Break{label,value}, Continue(f0), Shape(f0), Before(f0), Comptime(f0), Heap, Result, Todo(f0), Formatter(f0), With{callee,bindings}, Error });
structure!(ast::Expr { origin, kind });
impl Walk for ast::AssignOp {
    fn walk(scan: &mut Scan<'_>) -> Option<()> {
        field(|v: &Self| &v.0, scan)?;
        Some(())
    }
}
variants!(ast::StmtKind { Let{mutable,pattern,ty,value,otherwise}, Assign{place,op,value}, Expr(f0), While{label,condition,body}, For{label,pattern,source,body}, Function{name,signature,body} });
structure!(ast::Stmt { origin, kind });
structure!(ast::PatternField { label, pattern });
variants!(ast::PatternKind { Wildcard, Binding(f0), Literal(f0), Range{start,end}, Tuple(f0), Constructor{path,fields,rest}, Or(f0), Error });
structure!(ast::Pattern { origin, kind });
variants!(ast::TypeArgument { Type(f0), SizeLiteral(f0) });
variants!(ast::TypeModifier {
    Unique,
    Once,
    Sendable
});
structure!(ast::BrandSyntax { sibling, path });
variants!(ast::RelationPlace { Path(f0), Heap, Environment });
structure!(ast::RelationSyntax { places, origin });
variants!(ast::TypeExprKind { Named{path,arguments}, Tuple(f0), Function(f0), Box(f0), Qualified{base,modifiers,brand,relation}, Error });
structure!(ast::TypeExpr { origin, kind });
structure!(ast::FormatString {
    expression,
    pieces,
    holes,
    specs
});
structure!(ast::RepeatLiteral {
    expression,
    sequence,
    value,
    count,
    evaluated_count
});
variants!(ast::LetCondition { If{matching,otherwise}, While{body,matching} });
structure!(ast::Group { expression, origin });
variants!(ast::AnalysisSite { Item, Parameter(f0), Field(f0), Payload{variant,field} });
structure!(ast::AnalysisAnnotation {
    site,
    name,
    payload,
    spaced,
    origin
});
structure!(ast::SyntaxTree {
    tokens,
    expressions,
    statements,
    patterns,
    types,
    formats,
    repeats,
    conditions,
    groups,
    analyses
});
impl Walk for ast::ContentHash {
    fn walk(scan: &mut Scan<'_>) -> Option<()> {
        field(|v: &Self| &v.0, scan)?;
        Some(())
    }
}
impl Walk for ast::ItemSyntax {
    fn walk(scan: &mut Scan<'_>) -> Option<()> {
        let _: fn(&Self) = |v| {
            let Self {
                item: _,
                tree: _,
                anchor: _,
                text_hash: _,
                retention: _,
                occurrence: _,
            } = v;
        };
        ast::Item::walk(scan)?;
        ast::SyntaxTree::walk(scan)?;
        hirsel_span::Anchor::walk(scan)?;
        if scan.take(8)? != [0; 8] {
            return None;
        }
        ast::ContentHash::walk(scan)
    }
}
variants!(ty::UniqueResultRow { Root, Tuple(f0), Payload{variant,field} });
structure!(ty::UniqueSummary {
    preserves_unique,
    results,
    nonretaining
});
variants!(ty::WrapperKind {
    Pure,
    Handle,
    Effect
});
structure!(ty::ForeignWrapper {
    kind,
    deterministic
});
structure!(ty::HeapRole { arena, operation });
structure!(ty::NominalRoles {
    unsafe_capability,
    result_type,
    tasks_capability,
    scope_capability,
    heap
});
variants!(ty::ScalarDomain {
    Integer,
    Float,
    Scalar
});
variants!(ty::SchemaConstraint { Bound{parameter,bound,rule}, Domain{parameter,domain,rule}, CompleteRangeFits{source,destination}, DestinationNoWider{source,destination}, PositiveSize(f0), NoallocDrop(f0) });
structure!(ty::ImplementationBinder {
    name,
    family,
    members
});
structure!(ty::SignatureMetadata { inferred_unique, visibility, alias, constant_alias, track_caller, unsafe_ffi, wrapper, roles, schema, recursive_group, implementations } skip retention);
structure!(ty::RelationPlace { parameter, fields });
structure!(ty::StatePromise {
    selector,
    advance_only,
    preserve_captures,
    preserve_unique,
    unretained
});
variants!(ty::GenericParameter { Type{name,bounds}, Size{name} });
structure!(ty::Parameter {
    name,
    label,
    passing,
    ty
});
variants!(ty::Allocation {
    MayAllocate,
    NoAlloc
});
variants!(ty::AuthorityTerm { Capability(f0), Parameter(f0), TypeParameter(f0) });
variants!(ty::AuthoritySummary { Known(f0), Unknown });
variants!(ty::Uses { Unrestricted, Only(f0) });
structure!(ty::Contract {
    kind,
    ordinal,
    code_hash
});
structure!(ty::ForeignContract {
    abi,
    blocking,
    aliasing_contract
});
structure!(ty::FunctionType {
    state_promises,
    generics,
    parameters,
    result,
    error,
    allocation,
    authority,
    uses,
    contracts,
    incomplete,
    foreign
});
structure!(ty::LoopSourceDeclaration { open, next, option });
variants!(ty::CapabilityKind { Global, Scoped });
structure!(ty::SerializableCheck {
    function,
    representation
});
structure!(ty::Layout {
    c,
    packed,
    align,
    soa,
    flat
});
structure!(ty::TypeDefinition {
    loop_source,
    format,
    destructor_allocation,
    generics,
    representation,
    bounds,
    authority,
    capability,
    implementation_authority,
    serializable,
    serializable_check,
    layout,
    distinct,
    resource,
    drop,
    fact,
    captures
});
variants!(ty::FamilyMemberKind { Type{bounds}, Size, Operation });
structure!(ty::FamilyMember { name, kind });
structure!(ty::FamilySignature { members });
variants!(ty::MemberActual { Type(f0), Size(f0), Operation(f0) });
structure!(ty::ImplementationSignature { family, members });
variants!(ty::SignatureKind { Function(f0), Type(f0), Const{ty,generics}, Family(f0), Implementation(f0) });
structure!(ty::Signature {
    metadata,
    definition,
    kind,
    fingerprint
});
