//! The typed construction interface of CoreIR lowering. Lowering describes
//! each module item and instruction once, in final module order; the sink
//! consumes it immediately. Product objects use the native LLVM builder. The
//! text producer exists only as the printed-module oracle's second producer
//! and for diagnostic IR dumps; no product path parses its output.
use std::borrow::Cow;
use std::fmt::{self, Write};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum Type {
    Void,
    Int(u32),
    Float,
    Double,
    Ptr,
    Metadata,
    Array(u64, Box<Type>),
    Vector(u32, Box<Type>),
    Struct(Vec<Type>, bool),
}
impl Type {
    pub const I1: Self = Self::Int(1);
    pub const I8: Self = Self::Int(8);
    pub const I32: Self = Self::Int(32);
    pub const I64: Self = Self::Int(64);
    pub fn bytes(count: u64) -> Self {
        Self::Array(count, Box::new(Self::I8))
    }
    pub fn words(count: u64) -> Self {
        Self::Array(count, Box::new(Self::I64))
    }
    pub fn structure(fields: impl Into<Vec<Self>>) -> Self {
        Self::Struct(fields.into(), false)
    }
    pub fn is_void(&self) -> bool {
        matches!(self, Self::Void)
    }
}
impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Void => f.write_str("void"),
            Self::Int(bits) => write!(f, "i{bits}"),
            Self::Float => f.write_str("float"),
            Self::Double => f.write_str("double"),
            Self::Ptr => f.write_str("ptr"),
            Self::Metadata => f.write_str("metadata"),
            Self::Array(count, element) => write!(f, "[{count} x {element}]"),
            Self::Vector(count, element) => write!(f, "<{count} x {element}>"),
            Self::Struct(fields, packed) => {
                if *packed {
                    f.write_char('<')?;
                }
                f.write_str("{ ")?;
                for (i, field) in fields.iter().enumerate() {
                    if i != 0 {
                        f.write_str(", ")?;
                    }
                    field.fmt(f)?;
                }
                f.write_str(" }")?;
                if *packed {
                    f.write_char('>')?;
                }
                Ok(())
            }
        }
    }
}

/// A function-local name. Numbered families share one prefix each.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Local {
    Temp(usize),
    Value(usize),
    Address(usize),
    Slot(usize),
    Param(usize),
    Named(&'static str),
}
impl Local {
    pub const CONTEXT: Self = Self::Named("context");
    pub const DEST: Self = Self::Named("dest");
    pub const CALLER_SITE: Self = Self::Named("caller_site");
    pub const SRET: Self = Self::Named("sret");
    pub const PAYLOAD: Self = Self::Named("payload");
    pub const CURSOR: Self = Self::Named("cursor");
    pub const WORKLIST: Self = Self::Named("worklist");
    pub const INPUT: Self = Self::Named("input");
    pub const RESULT: Self = Self::Named("result");
    pub fn write_name(self, out: &mut impl Write) -> fmt::Result {
        match self {
            Self::Temp(i) => write!(out, "t{i}"),
            Self::Value(i) => write!(out, "v{i}"),
            Self::Address(i) => write!(out, "a{i}"),
            Self::Slot(i) => write!(out, "s{i}"),
            Self::Param(i) => write!(out, "p{i}"),
            Self::Named(name) => out.write_str(name),
        }
    }
}
impl fmt::Display for Local {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_char('%')?;
        self.write_name(f)
    }
}

/// A basic block label: a prefix, an optional number and an optional edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct Label {
    pub name: &'static str,
    pub n: u32,
    pub edge: u32,
}
impl Label {
    const NONE: u32 = u32::MAX;
    pub const fn plain(name: &'static str) -> Self {
        Self {
            name,
            n: Self::NONE,
            edge: Self::NONE,
        }
    }
    pub fn new(name: &'static str, n: usize) -> Self {
        Self {
            name,
            n: n as u32,
            edge: Self::NONE,
        }
    }
    pub fn edge(n: usize, edge: u32) -> Self {
        Self {
            name: "e",
            n: n as u32,
            edge,
        }
    }
    pub fn block(i: impl TryInto<u32>) -> Self {
        Self {
            name: "b",
            n: i.try_into().ok().unwrap(),
            edge: Self::NONE,
        }
    }
    pub fn write_name(self, out: &mut impl Write) -> fmt::Result {
        out.write_str(self.name)?;
        if self.n != Self::NONE {
            write!(out, "{}", self.n)?;
        }
        if self.edge != Self::NONE {
            write!(out, "_{}", self.edge)?;
        }
        Ok(())
    }
}
impl fmt::Display for Label {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.write_name(f)
    }
}

pub(super) type Name = Cow<'static, str>;

fn plain_symbol(name: &str) -> bool {
    !name.is_empty()
        && !name.as_bytes()[0].is_ascii_digit()
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'$' | b'.' | b'_'))
}
/// A global symbol as LLVM prints it, quoted where its characters need it.
pub(super) fn write_global(out: &mut impl Write, name: &str) -> fmt::Result {
    out.write_char('@')?;
    if plain_symbol(name) {
        return out.write_str(name);
    }
    out.write_char('"')?;
    for byte in name.bytes() {
        if byte == b'"' || byte == b'\\' || !(32..127).contains(&byte) {
            write!(out, "\\{byte:02X}")?;
        } else {
            out.write_char(byte as char)?;
        }
    }
    out.write_char('"')
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Value {
    Local(Local),
    /// An integer of the operand's type, printed in decimal.
    Int(i128),
    Bool(bool),
    Null,
    Poison,
    Zero,
    Global(Name),
    /// A floating constant given by its bit pattern.
    Bitcast {
        from: Type,
        bits: u64,
        to: Type,
    },
    IntToPtr(u64),
    PtrToInt(Name),
    /// `add (i64 left, i64 right)` over a constant expression.
    Add(Box<Value>, i64),
    Struct {
        packed: bool,
        fields: Vec<(Type, Value)>,
    },
    Bytes(Vec<u8>),
    /// `metadata !{!a, !b}` as a call argument.
    Metadata(Vec<usize>),
}
pub(super) const CONTEXT: Value = Value::Local(Local::CONTEXT);
impl Value {
    pub fn global(name: impl Into<Name>) -> Self {
        Self::Global(name.into())
    }
}
impl From<Local> for Value {
    fn from(local: Local) -> Self {
        Self::Local(local)
    }
}
impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Self::Bool(b)
    }
}
impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local(name) => name.fmt(f),
            Self::Int(n) => n.fmt(f),
            Self::Bool(value) => value.fmt(f),
            Self::Null => f.write_str("null"),
            Self::Poison => f.write_str("poison"),
            Self::Zero => f.write_str("zeroinitializer"),
            Self::Global(name) => write_global(f, name),
            Self::Bitcast { from, bits, to } => write!(f, "bitcast ({from} {bits} to {to})"),
            Self::IntToPtr(bits) => write!(f, "inttoptr (i64 {bits} to ptr)"),
            Self::PtrToInt(name) => {
                f.write_str("ptrtoint (ptr ")?;
                write_global(f, name)?;
                f.write_str(" to i64)")
            }
            Self::Add(left, right) => write!(f, "add (i64 {left}, i64 {right})"),
            Self::Struct { packed, fields } => {
                if *packed {
                    f.write_char('<')?;
                }
                f.write_str("{ ")?;
                for (i, (ty, value)) in fields.iter().enumerate() {
                    if i != 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{ty} {value}")?;
                }
                f.write_str(" }")?;
                if *packed {
                    f.write_char('>')?;
                }
                Ok(())
            }
            Self::Bytes(bytes) => {
                f.write_str("c\"")?;
                for b in bytes {
                    write!(f, "\\{b:02X}")?;
                }
                f.write_char('"')
            }
            Self::Metadata(ids) => {
                f.write_str("!{")?;
                list(f, ids)?;
                f.write_char('}')
            }
        }
    }
}
fn list(out: &mut impl Write, ids: &[usize]) -> fmt::Result {
    for (i, id) in ids.iter().enumerate() {
        if i != 0 {
            out.write_str(", ")?;
        }
        write!(out, "!{id}")?;
    }
    Ok(())
}

#[derive(Clone, Copy, Debug)]
#[repr(u32)]
pub(super) enum Binary {
    Add,
    Sub,
    Mul,
    UDiv,
    SDiv,
    URem,
    SRem,
    And,
    Or,
    Xor,
    Shl,
    LShr,
    AShr,
    FAdd,
    FSub,
    FMul,
    FDiv,
}
impl Binary {
    pub fn name(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Sub => "sub",
            Self::Mul => "mul",
            Self::UDiv => "udiv",
            Self::SDiv => "sdiv",
            Self::URem => "urem",
            Self::SRem => "srem",
            Self::And => "and",
            Self::Or => "or",
            Self::Xor => "xor",
            Self::Shl => "shl",
            Self::LShr => "lshr",
            Self::AShr => "ashr",
            Self::FAdd => "fadd",
            Self::FSub => "fsub",
            Self::FMul => "fmul",
            Self::FDiv => "fdiv",
        }
    }
}
#[derive(Clone, Copy, Debug)]
#[repr(u32)]
pub(super) enum Cast {
    Trunc,
    ZExt,
    SExt,
    FPToUI,
    FPToSI,
    UIToFP,
    SIToFP,
    FPTrunc,
    FPExt,
    PtrToInt,
    IntToPtr,
    Bitcast,
}
impl Cast {
    pub fn name(self) -> &'static str {
        match self {
            Self::Trunc => "trunc",
            Self::ZExt => "zext",
            Self::SExt => "sext",
            Self::FPToUI => "fptoui",
            Self::FPToSI => "fptosi",
            Self::UIToFP => "uitofp",
            Self::SIToFP => "sitofp",
            Self::FPTrunc => "fptrunc",
            Self::FPExt => "fpext",
            Self::PtrToInt => "ptrtoint",
            Self::IntToPtr => "inttoptr",
            Self::Bitcast => "bitcast",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub(super) enum Predicate {
    OEq = 1,
    OGt = 2,
    OGe = 3,
    OLt = 4,
    OLe = 5,
    Uno = 8,
    UNe = 14,
    Eq = 32,
    Ne = 33,
    Ugt = 34,
    Uge = 35,
    Ult = 36,
    Ule = 37,
    Sgt = 38,
    Sge = 39,
    Slt = 40,
    Sle = 41,
}
impl Predicate {
    pub fn name(self) -> &'static str {
        match self {
            Self::OEq => "oeq",
            Self::OGt => "ogt",
            Self::OGe => "oge",
            Self::OLt => "olt",
            Self::OLe => "ole",
            Self::Uno => "uno",
            Self::UNe => "une",
            Self::Eq => "eq",
            Self::Ne => "ne",
            Self::Ugt => "ugt",
            Self::Uge => "uge",
            Self::Ult => "ult",
            Self::Ule => "ule",
            Self::Sgt => "sgt",
            Self::Sge => "sge",
            Self::Slt => "slt",
            Self::Sle => "sle",
        }
    }
    pub fn floating(self) -> bool {
        (self as u32) < 32
    }
}
/// LLVM's `AtomicOrdering` numbering.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub(super) enum Ordering {
    Monotonic = 2,
    Acquire = 4,
}
impl Ordering {
    fn name(self) -> &'static str {
        match self {
            Self::Monotonic => "monotonic",
            Self::Acquire => "acquire",
        }
    }
}
/// Alias scope lists of one access. `!alias.scope` is always present.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Scoped {
    pub alias: Vec<usize>,
    pub noalias: Vec<usize>,
}
#[derive(Clone, Debug, Default)]
pub(super) struct Memory {
    /// Zero omits the alignment: ABI alignment for an access.
    pub align: u32,
    pub invariant: bool,
    pub ordering: Option<Ordering>,
    pub scopes: Option<Scoped>,
}
impl Memory {
    pub fn aligned(align: u32) -> Self {
        Self {
            align,
            ..Self::default()
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Extension {
    None,
    Zero,
    Sign,
}
impl Extension {
    fn text(self) -> &'static str {
        match self {
            Self::None => "",
            Self::Zero => " zeroext",
            Self::Sign => " signext",
        }
    }
}
#[derive(Clone, Debug)]
pub(super) struct Parameter {
    pub ty: Type,
    pub extension: Extension,
    pub noalias: bool,
    pub readonly: bool,
    pub sret: Option<Type>,
    pub byval: Option<Type>,
    pub align: u32,
}
impl Parameter {
    pub fn plain(ty: Type) -> Self {
        Self {
            ty,
            extension: Extension::None,
            noalias: false,
            readonly: false,
            sret: None,
            byval: None,
            align: 0,
        }
    }
    pub fn extended(ty: Type, extension: Extension) -> Self {
        Self {
            extension,
            ..Self::plain(ty)
        }
    }
    pub fn sret(storage: Type) -> Self {
        Self {
            sret: Some(storage),
            ..Self::plain(Type::Ptr)
        }
    }
}
impl From<Type> for Parameter {
    fn from(ty: Type) -> Self {
        Self::plain(ty)
    }
}
#[derive(Clone, Debug)]
pub(super) struct FnSig {
    pub result: Type,
    pub extension: Extension,
    pub nonnull: bool,
    pub params: Vec<Parameter>,
}
impl FnSig {
    pub fn new(result: Type, params: Vec<Parameter>) -> Self {
        Self {
            result,
            extension: Extension::None,
            nonnull: false,
            params,
        }
    }
    pub fn plain(result: Type, params: impl IntoIterator<Item = Type>) -> Self {
        Self::new(result, params.into_iter().map(Parameter::plain).collect())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Inline {
    Default,
    Always,
    Never,
}
#[derive(Clone, Debug, Default)]
pub(super) struct CallAttrs {
    pub cold: bool,
    pub tail: bool,
    pub always_inline: bool,
    pub scopes: Option<Scoped>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub(super) enum Linkage {
    External,
    Internal,
    Private,
}
impl Linkage {
    fn text(self) -> &'static str {
        match self {
            Self::External => "",
            Self::Internal => "internal ",
            Self::Private => "private ",
        }
    }
}
#[derive(Clone, Debug)]
pub(super) struct FnAttrs {
    pub linkage: Linkage,
    pub inline: Inline,
    pub nounwind: bool,
    pub argmem_read: bool,
    pub destination_size: Option<u64>,
    pub view_input: bool,
    pub subprogram: Option<usize>,
}
impl Default for FnAttrs {
    fn default() -> Self {
        Self {
            linkage: Linkage::External,
            inline: Inline::Default,
            nounwind: false,
            argmem_read: false,
            destination_size: None,
            view_input: false,
            subprogram: None,
        }
    }
}
impl FnAttrs {
    pub fn private() -> Self {
        Self {
            linkage: Linkage::Private,
            ..Self::default()
        }
    }
}
#[derive(Clone, Debug)]
pub(super) struct Global {
    pub name: Name,
    pub linkage: Linkage,
    pub constant: bool,
    pub externally_initialized: bool,
    pub ty: Type,
    /// `None` declares an external global.
    pub init: Option<Value>,
    pub align: u32,
    pub absolute_symbol: bool,
}
#[derive(Clone, Debug)]
pub(super) enum MdItem {
    Node(usize),
    String(String),
    I32(u32),
}
#[derive(Clone, Debug)]
pub(super) enum Metadata {
    /// `!{...}`; a distinct tuple may name itself as an operand.
    Tuple {
        distinct: bool,
        items: Vec<MdItem>,
    },
    File {
        filename: String,
    },
    CompileUnit {
        file: usize,
    },
    SubroutineType {
        types: usize,
    },
    Subprogram {
        name: String,
        linkage: String,
        file: usize,
        ty: usize,
        unit: usize,
    },
    Location {
        line: u64,
        column: u32,
        scope: usize,
    },
    LexicalBlockFile {
        scope: usize,
        file: usize,
    },
}

/// One synchronous instruction. The sink consumes it immediately.
pub(super) enum Operation<'a> {
    Binary(Binary, &'a Type, &'a Value, &'a Value),
    Cast(Cast, &'a Type, &'a Value, &'a Type),
    Compare(Predicate, &'a Type, &'a Value, &'a Value),
    Select(&'a Type, &'a Value, &'a Value, &'a Value),
    FNeg(&'a Type, &'a Value),
    Load(&'a Type, &'a Value, &'a Memory),
    Store(&'a Type, &'a Value, &'a Value, &'a Memory),
    Gep {
        ty: &'a Type,
        base: &'a Value,
        indices: &'a [(Type, Value)],
        inbounds: bool,
    },
    Extract(&'a Type, &'a Value, u32),
    Insert(&'a Type, &'a Value, &'a Type, &'a Value, u32),
    Phi(&'a Type, &'a [(Value, Label)]),
    Call(&'a FnSig, &'a Value, &'a [Value], &'a CallAttrs),
    /// `cmpxchg weak` with monotonic success and failure orderings.
    CompareExchange(&'a Value, &'a Type, &'a Value, &'a Value),
    /// `atomicrmw sub ... release, align 8`.
    AtomicSub(&'a Value, &'a Type, &'a Value),
    Fence(Ordering),
    Branch(Label),
    Conditional(&'a Value, Label, Label, bool),
    Switch(&'a Type, &'a Value, Label, &'a [(i128, Label)]),
    Return(Option<(&'a Type, &'a Value)>),
    Unreachable,
}

/// Module items arrive in their final order; function bodies arrive between
/// `begin_function` and `end_function`. Metadata may arrive in any order
/// before its first use; its identifiers are the producer's own.
pub(super) trait Sink {
    fn global(&mut self, global: &Global);
    fn declare(&mut self, name: &str, sig: &FnSig, attrs: &FnAttrs);
    fn module_asm(&mut self, text: &str);
    fn metadata(&mut self, id: usize, node: &Metadata);
    fn named_metadata(&mut self, name: &str, ids: &[usize]);
    fn begin_function(&mut self, name: &str, sig: &FnSig, params: &[Local], attrs: &FnAttrs);
    fn end_function(&mut self);
    fn block(&mut self, label: Label);
    fn alloca(&mut self, name: Local, ty: &Type, align: u32);
    fn instruction(&mut self, result: Option<Local>, op: Operation<'_>, location: Option<usize>);
}

/// LLVM text of the same module. The printed-module oracle parses it in a
/// fresh context and compares the print with the native module's.
#[derive(Default)]
pub(super) struct TextSink {
    text: String,
    metadata: String,
    header: String,
    allocations: String,
    code: String,
}
impl TextSink {
    pub fn finish(mut self) -> String {
        self.text.push_str(&self.metadata);
        self.text
    }
}
fn scopes(out: &mut String, scoped: &Option<Scoped>) {
    if let Some(s) = scoped {
        out.push_str(", !alias.scope !{");
        list(out, &s.alias).unwrap();
        out.push('}');
        if !s.noalias.is_empty() {
            out.push_str(", !noalias !{");
            list(out, &s.noalias).unwrap();
            out.push('}');
        }
    }
}
fn parameter(out: &mut String, p: &Parameter) {
    write!(out, "{}{}", p.ty, p.extension.text()).unwrap();
    if p.noalias {
        out.push_str(" noalias");
    }
    if p.readonly {
        out.push_str(" readonly");
    }
    if let Some(ty) = &p.sret {
        write!(out, " sret({ty})").unwrap();
    }
    if let Some(ty) = &p.byval {
        write!(out, " byval({ty})").unwrap();
    }
    if p.align != 0 {
        write!(out, " align {}", p.align).unwrap();
    }
}
pub(super) fn result_type(out: &mut String, sig: &FnSig) {
    out.push_str(sig.extension.text().trim_start());
    if sig.extension != Extension::None {
        out.push(' ');
    }
    if sig.nonnull {
        out.push_str("nonnull ");
    }
    write!(out, "{}", sig.result).unwrap();
}
fn function_attrs(out: &mut String, attrs: &FnAttrs) {
    match attrs.inline {
        Inline::Default => {}
        Inline::Always => out.push_str(" alwaysinline"),
        Inline::Never => out.push_str(" noinline"),
    }
    if attrs.nounwind {
        out.push_str(" nounwind");
    }
    if attrs.argmem_read {
        out.push_str(" memory(argmem: read)");
    }
    if let Some(bytes) = attrs.destination_size {
        write!(out, " \"hirsel-destination-size\"=\"{bytes}\"").unwrap();
    }
    if attrs.view_input {
        out.push_str(" \"hirsel-view-input\"");
    }
    if let Some(scope) = attrs.subprogram {
        write!(out, " !dbg !{scope}").unwrap();
    }
}
fn md_string(out: &mut String, text: &str) {
    out.push('!');
    quoted(out, text);
}
fn quoted(out: &mut String, text: &str) {
    out.push('"');
    for byte in text.bytes() {
        if byte == b'"' || byte == b'\\' || !(32..127).contains(&byte) {
            write!(out, "\\{byte:02X}").unwrap();
        } else {
            out.push(byte as char);
        }
    }
    out.push('"');
}
fn memory(out: &mut String, m: &Memory) {
    if m.align != 0 {
        write!(out, ", align {}", m.align).unwrap();
    }
    if m.invariant {
        out.push_str(", !invariant.load !1");
    }
    scopes(out, &m.scopes);
}
impl Sink for TextSink {
    fn global(&mut self, g: &Global) {
        let out = &mut self.text;
        write_global(out, &g.name).unwrap();
        out.push_str(" = ");
        if g.init.is_none() {
            out.push_str("external ");
        }
        out.push_str(g.linkage.text());
        if g.externally_initialized {
            out.push_str("externally_initialized ");
        }
        out.push_str(if g.constant { "constant " } else { "global " });
        write!(out, "{}", g.ty).unwrap();
        if let Some(init) = &g.init {
            write!(out, " {init}").unwrap();
        }
        if g.align != 0 {
            write!(out, ", align {}", g.align).unwrap();
        }
        if g.absolute_symbol {
            out.push_str(", !absolute_symbol !{i64 -1, i64 -1}");
        }
        out.push('\n');
    }
    fn declare(&mut self, name: &str, sig: &FnSig, attrs: &FnAttrs) {
        let out = &mut self.text;
        out.push_str("declare ");
        result_type(out, sig);
        out.push(' ');
        write_global(out, name).unwrap();
        out.push('(');
        for (i, p) in sig.params.iter().enumerate() {
            if i != 0 {
                out.push_str(", ");
            }
            parameter(out, p);
        }
        out.push(')');
        function_attrs(out, attrs);
        out.push('\n');
    }
    fn module_asm(&mut self, text: &str) {
        writeln!(self.text, "module asm \"{text}\"").unwrap();
    }
    fn metadata(&mut self, id: usize, node: &Metadata) {
        let out = &mut self.metadata;
        write!(out, "!{id} = ").unwrap();
        match node {
            Metadata::Tuple { distinct, items } => {
                if *distinct {
                    out.push_str("distinct ");
                }
                out.push_str("!{");
                for (i, item) in items.iter().enumerate() {
                    if i != 0 {
                        out.push_str(", ");
                    }
                    match item {
                        MdItem::Node(id) => write!(out, "!{id}").unwrap(),
                        MdItem::String(s) => md_string(out, s),
                        MdItem::I32(n) => write!(out, "i32 {n}").unwrap(),
                    }
                }
                out.push('}');
            }
            Metadata::File { filename } => {
                out.push_str("!DIFile(filename: ");
                quoted(out, filename);
                out.push_str(", directory: \"\")");
            }
            Metadata::CompileUnit { file } => write!(
                out,
                "distinct !DICompileUnit(language: DW_LANG_Rust, file: !{file}, producer: \"Hirsel\", isOptimized: false, runtimeVersion: 0, emissionKind: FullDebug)"
            )
            .unwrap(),
            Metadata::SubroutineType { types } => {
                write!(out, "!DISubroutineType(types: !{types})").unwrap()
            }
            Metadata::Subprogram {
                name,
                linkage,
                file,
                ty,
                unit,
            } => {
                out.push_str("distinct !DISubprogram(name: ");
                quoted(out, name);
                out.push_str(", linkageName: ");
                quoted(out, linkage);
                write!(out, ", scope: !{file}, file: !{file}, line: 1, type: !{ty}, scopeLine: 1, spFlags: DISPFlagDefinition, unit: !{unit})").unwrap();
            }
            Metadata::Location {
                line,
                column,
                scope,
            } => {
                if *column == 0 {
                    write!(out, "!DILocation(line: {line}, scope: !{scope})").unwrap()
                } else {
                    write!(
                        out,
                        "!DILocation(line: {line}, column: {column}, scope: !{scope})"
                    )
                    .unwrap()
                }
            }
            Metadata::LexicalBlockFile { scope, file } => write!(
                out,
                "!DILexicalBlockFile(scope: !{scope}, file: !{file}, discriminator: 0)"
            )
            .unwrap(),
        }
        out.push('\n');
    }
    fn named_metadata(&mut self, name: &str, ids: &[usize]) {
        write!(self.metadata, "!{name} = !{{").unwrap();
        list(&mut self.metadata, ids).unwrap();
        self.metadata.push_str("}\n");
    }
    fn begin_function(&mut self, name: &str, sig: &FnSig, params: &[Local], attrs: &FnAttrs) {
        let out = &mut self.header;
        out.clear();
        write!(out, "define {}", attrs.linkage.text()).unwrap();
        result_type(out, sig);
        out.push(' ');
        write_global(out, name).unwrap();
        out.push('(');
        for (i, (p, local)) in sig.params.iter().zip(params).enumerate() {
            if i != 0 {
                out.push_str(", ");
            }
            parameter(out, p);
            write!(out, " {local}").unwrap();
        }
        out.push(')');
        function_attrs(out, attrs);
        self.allocations.clear();
        self.code.clear();
    }
    fn end_function(&mut self) {
        writeln!(
            self.text,
            "{} {{\nentry:\n{}{}}}",
            self.header, self.allocations, self.code
        )
        .unwrap();
    }
    fn alloca(&mut self, name: Local, ty: &Type, align: u32) {
        write!(self.allocations, "  {name} = alloca {ty}").unwrap();
        if align != 0 {
            write!(self.allocations, ", align {align}").unwrap();
        }
        self.allocations.push('\n');
    }
    fn block(&mut self, label: Label) {
        writeln!(self.code, "{label}:").unwrap();
    }
    fn instruction(&mut self, result: Option<Local>, op: Operation<'_>, location: Option<usize>) {
        let out = &mut self.code;
        out.push_str("  ");
        if let Some(name) = result {
            write!(out, "{name} = ").unwrap();
        }
        match op {
            Operation::Binary(op, ty, a, b) => write!(out, "{} {ty} {a}, {b}", op.name()).unwrap(),
            Operation::Cast(op, from, value, to) => {
                write!(out, "{} {from} {value} to {to}", op.name()).unwrap()
            }
            Operation::Compare(op, ty, a, b) => write!(
                out,
                "{} {} {ty} {a}, {b}",
                if op.floating() { "fcmp" } else { "icmp" },
                op.name()
            )
            .unwrap(),
            Operation::Select(ty, condition, yes, no) => {
                write!(out, "select i1 {condition}, {ty} {yes}, {ty} {no}").unwrap()
            }
            Operation::FNeg(ty, value) => write!(out, "fneg {ty} {value}").unwrap(),
            Operation::Load(ty, p, m) => {
                match m.ordering {
                    Some(o) => write!(out, "load atomic {ty}, ptr {p} {}", o.name()).unwrap(),
                    None => write!(out, "load {ty}, ptr {p}").unwrap(),
                }
                memory(out, m);
            }
            Operation::Store(ty, value, p, m) => {
                match m.ordering {
                    Some(o) => {
                        write!(out, "store atomic {ty} {value}, ptr {p} {}", o.name()).unwrap()
                    }
                    None => write!(out, "store {ty} {value}, ptr {p}").unwrap(),
                }
                memory(out, m);
            }
            Operation::Gep {
                ty,
                base,
                indices,
                inbounds,
            } => {
                let flag = if inbounds { " inbounds" } else { "" };
                write!(out, "getelementptr{flag} {ty}, ptr {base}").unwrap();
                for (ty, v) in indices {
                    write!(out, ", {ty} {v}").unwrap();
                }
            }
            Operation::Extract(ty, value, index) => {
                write!(out, "extractvalue {ty} {value}, {index}").unwrap()
            }
            Operation::Insert(ty, aggregate, field, value, index) => write!(
                out,
                "insertvalue {ty} {aggregate}, {field} {value}, {index}"
            )
            .unwrap(),
            Operation::Phi(ty, incoming) => {
                write!(out, "phi {ty} ").unwrap();
                for (i, (value, block)) in incoming.iter().enumerate() {
                    if i != 0 {
                        out.push_str(", ");
                    }
                    write!(out, "[{value}, %{block}]").unwrap();
                }
            }
            Operation::Call(sig, callee, args, attrs) => {
                if attrs.tail {
                    out.push_str("tail ");
                }
                out.push_str("call ");
                result_type(out, sig);
                write!(out, " {callee}(").unwrap();
                for (i, (param, arg)) in sig.params.iter().zip(args).enumerate() {
                    if i != 0 {
                        out.push_str(", ");
                    }
                    parameter(out, param);
                    write!(out, " {arg}").unwrap();
                }
                out.push(')');
                if attrs.always_inline {
                    out.push_str(" alwaysinline");
                }
                if attrs.cold {
                    out.push_str(" cold");
                }
                scopes(out, &attrs.scopes);
            }
            Operation::CompareExchange(p, ty, expected, new) => write!(
                out,
                "cmpxchg weak ptr {p}, {ty} {expected}, {ty} {new} monotonic monotonic"
            )
            .unwrap(),
            Operation::AtomicSub(p, ty, value) => {
                write!(out, "atomicrmw sub ptr {p}, {ty} {value} release, align 8").unwrap()
            }
            Operation::Fence(o) => write!(out, "fence {}", o.name()).unwrap(),
            Operation::Branch(target) => write!(out, "br label %{target}").unwrap(),
            Operation::Conditional(test, yes, no, likely) => {
                write!(out, "br i1 {test}, label %{yes}, label %{no}").unwrap();
                if likely {
                    out.push_str(", !prof !0");
                }
            }
            Operation::Switch(ty, value, default, arms) => {
                write!(out, "switch {ty} {value}, label %{default} [").unwrap();
                for (value, label) in arms {
                    write!(out, "\n    {ty} {value}, label %{label}").unwrap();
                }
                out.push_str("\n  ]");
            }
            Operation::Return(Some((ty, value))) => write!(out, "ret {ty} {value}").unwrap(),
            Operation::Return(None) => out.push_str("ret void"),
            Operation::Unreachable => out.push_str("unreachable"),
        }
        if let Some(location) = location {
            write!(out, ", !dbg !{location}").unwrap();
        }
        out.push('\n');
    }
}

#[cfg(hirsel_lazy_core)]
pub(super) mod native;
