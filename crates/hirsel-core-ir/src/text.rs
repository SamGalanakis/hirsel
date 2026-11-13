//! The text form: `print` and `parse`, line-oriented, one definition, instruction or
//! terminator per line, `;` to end of line a comment. `parse(&print(m)) == m` for
//! every module `parse` produced; a module built in another table order prints
//! and parses to the same program with its tables in reading order.
//!
//! ```text
//! core v0
//! item i0 = app@1/main/add                ; source items (debug identity)
//! type t0 = "app@1/geo/Point"             ; declared types, canonical text
//! static d0 = align 1 hex 6869            ; read-only data
//! sig g0 = (i32, &t0) -> (i32) dest t0    ; params: scalar, ptr or &tN (by address)
//! sig g1 = (u64) -> never                 ; FORM v1: a function whose result is Never
//! inst n0 = app@1/main/add ["i32"] []     ; instance key: definition, types, dictionaries
//! fn f0 = n0 g0 extern                    ; defined in another module
//! fn f1 = n1 g1 {
//!   slot s0 = i32
//!   addr a0 = s0 field 1 index v2 deref : i32   ; base, steps, place type
//! b0(v0: i32, v1: i32):
//!   v2: i32 = add v0, v1 fail b1 at i0 4..9
//!   return v2 at i0 0..20
//! }
//! ```

use crate::ir::*;
use hirsel_core_ids::DefPath;
use hirsel_core_layout::FieldTy;

macro_rules! names {
    ($ty:ident { $($v:ident = $s:literal),* $(,)? }) => {
        impl $ty {
            pub const fn name(self) -> &'static str {
                match self { $(Self::$v => $s),* }
            }
            pub fn from_name(name: &str) -> Option<Self> {
                match name { $($s => Some(Self::$v),)* _ => None }
            }
        }
    };
}
names!(UnaryOp { Neg = "neg", Not = "not", BitNot = "bitnot" });
names!(BinaryOp {
    Add = "add", Sub = "sub", Mul = "mul", Div = "div", Rem = "rem", Shl = "shl", Shr = "shr",
    BitAnd = "bitand", BitOr = "bitor", BitXor = "bitxor", Min = "min", Max = "max", MinNumber = "min_number", MaxNumber = "max_number",
});
names!(CompareOp { Eq = "eq", Ne = "ne", Lt = "lt", Le = "le", Gt = "gt", Ge = "ge" });
names!(ConvertMode {
    Widen = "widen", Truncate = "truncate", TryNarrow = "try_narrow", FloatToInt = "float_to_int",
    IntToFloat = "int_to_float", Reinterpret = "reinterpret", FloatResize = "float_resize",
});
names!(ArithMode { Wrapping = "wrapping", Saturating = "saturating", Checked = "checked" });
names!(ArithOp { Add = "add", Sub = "sub", Mul = "mul", Div = "div" });
names!(ScalarUnaryOp {
    Sqrt = "sqrt", Floor = "floor", Ceil = "ceil", Trunc = "trunc", Round = "round", Abs = "abs",
    LeadingZeros = "leading_zeros", TrailingZeros = "trailing_zeros", CountOnes = "count_ones",
    SwapBytes = "swap_bytes", ReverseBits = "reverse_bits",
});

impl ConvertMode {
    /// Whether the conversion produces a status flag beside its result.
    pub const fn has_flag(self) -> bool {
        matches!(self, Self::TryNarrow | Self::FloatToInt)
    }
}

names!(Counted { Rc = "rc", Arc = "arc" });

/// Whether the module uses a FORM v1 construct; it then prints as `core v1`,
/// and a `core v0` text that uses one is refused.
pub fn uses_v1(m: &Module) -> bool {
    !m.types.is_empty()
        || m.signatures.iter().any(|s| s.never)
        || m.functions.iter().filter_map(|f| f.body.as_ref()).any(|b| {
            b.calls
                .iter()
                .any(|c| c.fail.is_some() || matches!(c.callee, Callee::Runtime(_)))
                || b.terminators.iter().any(|t| {
                    matches!(
                        t,
                        Terminator::Raise { .. } | Terminator::Propagate | Terminator::Unreachable
                    )
                })
                || b.insts.iter().any(|i| {
                    // v0 names a failure block on every fallible instruction.
                    (i.can_fail(b) && i.fail(b).is_none() && !matches!(i, Inst::Call { .. }))
                        || matches!(
                            i,
                            Inst::Alloc { .. }
                                | Inst::Free { .. }
                                | Inst::Copy { .. }
                                | Inst::RegionOpen { .. }
                                | Inst::RegionClose { .. }
                                | Inst::Trim { .. }
                                | Inst::LimitEnter { .. }
                                | Inst::LimitExit
                                | Inst::Drop { .. }
                                | Inst::Share { .. }
                                | Inst::Release { .. }
                                | Inst::TryUnwrap { .. }
                                | Inst::Case { .. }
                                | Inst::SetCase { .. }
                                | Inst::Pointer { .. }
                        )
                })
                || b.pools.steps.iter().any(|s| matches!(s, Step::Byte(_)))
        })
}

// ---------------------------------------------------------------- printing

pub fn print(m: &Module) -> String {
    let mut p = Printer::default();
    p.module(m);
    p.out
}

#[derive(Default)]
struct Printer {
    out: String,
}

fn ty(t: Ty) -> &'static str {
    match t {
        Ty::Scalar(s) => s.name(),
        Ty::Ptr => "ptr",
    }
}
fn place(t: PlaceTy) -> String {
    match t {
        PlaceTy::Value(t) => ty(t).into(),
        PlaceTy::Declared(t) => format!("t{}", t.0),
    }
}
fn quote(s: &str) -> String {
    let mut q = String::from("\"");
    for c in s.chars() {
        if c == '"' || c == '\\' {
            q.push('\\');
        }
        q.push(c);
    }
    q.push('"');
    q
}
fn field_ty(f: &FieldTy) -> String {
    use hirsel_core_layout::Handle;
    match f {
        FieldTy::Scalar(s) => s.name().into(),
        FieldTy::Inline(t) => format!("t{}", t.0),
        FieldTy::Box(t) => format!("box t{}", t.0),
        FieldTy::Seq(e) => format!("seq {}", field_ty(e)),
        FieldTy::Rc(t) => format!("rc t{}", t.0),
        FieldTy::Arc(t) => format!("arc t{}", t.0),
        FieldTy::Stable(t) => format!("stable t{}", t.0),
        FieldTy::Array(e, n) => format!("array {n} {}", field_ty(e)),
        FieldTy::Fn => "fn".into(),
        FieldTy::OwnFn => "ownfn".into(),
        FieldTy::Borrow => "borrow".into(),
        FieldTy::Handle(Handle::Task) => "task".into(),
        FieldTy::Handle(Handle::Sender) => "sender".into(),
        FieldTy::Handle(Handle::Receiver) => "receiver".into(),
        FieldTy::Handle(Handle::Capability) => "cap".into(),
        FieldTy::Handle(Handle::Causes) => "causes".into(),
        FieldTy::Handle(Handle::HostFile) => "hostfile".into(),
        FieldTy::Map(k, v) => format!("map {} {}", field_ty(k), field_ty(v)),
        FieldTy::Text => "text".into(),
        FieldTy::FixedSpan(e) => format!("fixed {}", field_ty(e)),
        FieldTy::SeqSpan(e) => format!("seqspan {}", field_ty(e)),
        FieldTy::IndexedSpan(e) => format!("indexed {}", field_ty(e)),
        FieldTy::Link(t) => format!("link t{}", t.0),
        FieldTy::OptLink(t) => format!("optlink t{}", t.0),
        FieldTy::Locator => "locator".into(),
    }
}
fn fields(fs: &[hirsel_core_layout::Field]) -> String {
    if fs.is_empty() {
        return "{ }".into();
    }
    format!(
        "{{ {} }}",
        list(fs, |f| format!("{}: {}", f.name, field_ty(&f.ty)))
    )
}
/// `own "Pair" { a: u64 }`, `resource "Temp" drop f3 grant 64 { .. }`,
/// `compact u32 "Row" { .. }`, `own "Opt" | None { } | Some { v: u64 }`.
fn shape(s: &Shape) -> String {
    use hirsel_core_layout::{Body as B, Repr, Width};
    let repr = match s.repr {
        Repr::Own => "own".into(),
        Repr::Resource => "resource".into(),
        Repr::Stable => "stable".into(),
        Repr::Scoped => "scoped".into(),
        Repr::Compact(w) => format!(
            "compact {}",
            match w {
                Width::U16 => "u16",
                Width::U32 => "u32",
                Width::U64 => "u64",
            }
        ),
    };
    let mut out = format!("{repr} {}", quote(&s.name));
    if let Some(f) = s.finalizer {
        out += &format!(" drop f{}", f.0);
    }
    if s.grant != 0 {
        out += &format!(" grant {}", s.grant);
    }
    match &s.body {
        B::Record(fs) => out += &format!(" {}", fields(fs)),
        B::Variant(cases) => {
            for c in cases {
                out += &format!(" | {} {}", c.name, fields(&c.fields));
            }
        }
    }
    out
}
fn rt(op: RtOp) -> String {
    match op {
        RtOp::Spawn { x, r } => format!("rt.spawn[t{}, t{}]", x.0, r.0),
        RtOp::Join { r } => format!("rt.join[t{}]", r.0),
        RtOp::Cancel => "rt.cancel".into(),
        RtOp::CheckCancel => "rt.check_cancel".into(),
        RtOp::ChannelNew { item } => format!("rt.channel_new[t{}]", item.0),
        RtOp::ChannelSender => "rt.channel_sender".into(),
        RtOp::Send { item } => format!("rt.send[t{}]", item.0),
        RtOp::TrySend { item } => format!("rt.try_send[t{}]", item.0),
        RtOp::Receive { item } => format!("rt.receive[t{}]", item.0),
        RtOp::DestId => "rt.dest_id".into(),
        RtOp::CompareBytes => "rt.compare_bytes".into(),
        RtOp::ArgsLen => "rt.args_len".into(),
        RtOp::ArgsGet => "rt.args_get".into(),
        RtOp::Host(op) => format!("rt.{}", op.name()),
    }
}
fn list<T>(items: impl IntoIterator<Item = T>, f: impl Fn(T) -> String) -> String {
    items.into_iter().map(f).collect::<Vec<_>>().join(", ")
}

/// A constant's bits as text: decimal for integers, bool and char; hex bits for floats.
fn constant(t: Ty, bits: u64) -> String {
    match t {
        Ty::Scalar(Scalar::Bool) => (if bits == 0 { "false" } else { "true" }).into(),
        Ty::Scalar(Scalar::F32 | Scalar::F64) | Ty::Ptr => format!("{bits:#x}"),
        t => match t.int() {
            Some((true, w)) => (((bits << (64 - w)) as i64) >> (64 - w)).to_string(),
            _ => bits.to_string(),
        },
    }
}

impl Printer {
    fn line(&mut self, s: &str) {
        self.out.push_str(s);
        self.out.push('\n');
    }

    fn module(&mut self, m: &Module) {
        self.line(if uses_v1(m) { "core v1" } else { "core v0" });
        for (i, item) in m.items.iter().enumerate() {
            self.line(&format!("item i{i} = {item}"));
        }
        for (i, t) in m.types.iter().enumerate() {
            self.line(&format!("type t{i} = {}", shape(t)));
        }
        for (i, s) in m.statics.iter().enumerate() {
            let hex: String = s.bytes.iter().map(|b| format!("{b:02x}")).collect();
            self.line(&format!("static d{i} = align {} hex {hex}", s.align));
        }
        for (i, s) in m.signatures.iter().enumerate() {
            let params = list(&s.params, |p| match p {
                Param::Value(t) => ty(*t).into(),
                Param::Address(t) => format!("&t{}", t.0),
            });
            let results = if s.never {
                "never".into()
            } else {
                format!("({})", list(&s.results, |t| ty(*t).into()))
            };
            let dest = s
                .dest
                .map(|t| format!(" dest t{}", t.0))
                .unwrap_or_default();
            self.line(&format!("sig g{i} = ({params}) -> {results}{dest}"));
        }
        for (i, k) in m.instances.iter().enumerate() {
            let types = list(&k.types, |t| quote(t));
            let dicts = list(&k.dictionaries, |d| quote(d));
            self.line(&format!("inst n{i} = {} [{types}] [{dicts}]", k.definition));
        }
        for (i, f) in m.functions.iter().enumerate() {
            let head = format!("fn f{i} = n{} g{}", f.instance.0, f.signature.0);
            match &f.body {
                None => self.line(&format!("{head} extern")),
                Some(body) => {
                    self.line(&format!("{head} {{"));
                    self.body(body);
                    self.line("}");
                }
            }
        }
    }

    fn body(&mut self, b: &Body) {
        for (i, s) in b.slots.iter().enumerate() {
            self.line(&format!("  slot s{i} = {}", place(*s)));
        }
        for (i, a) in b.addresses.iter().enumerate() {
            let mut l = format!("  addr a{i} = ");
            l += &match a.base {
                AddressBase::Slot(s) => format!("s{}", s.0),
                AddressBase::Value(v) => format!("v{}", v.0),
                AddressBase::Address(a) => format!("a{}", a.0),
            };
            for step in a.steps.get(&b.pools.steps).unwrap_or_default() {
                l += &match step {
                    Step::Deref => " deref".into(),
                    Step::Field(f) => format!(" field {f}"),
                    Step::CaseField { case, field } => format!(" case {case} {field}"),
                    Step::Index(v) => format!(" index v{}", v.0),
                    Step::Element(v) => format!(" element v{}", v.0),
                    Step::Byte(v) => format!(" byte v{}", v.0),
                };
            }
            l += &format!(" : {}", place(a.ty));
            self.line(&l);
        }
        let vals = |r: PoolRange<ValueId>| r.get(&b.pools.values).unwrap_or_default().to_vec();
        let def = |v: ValueId| format!("v{}: {}", v.0, ty(b.values[v.index()]));
        let edge = |e: EdgeId| {
            let e = &b.edges[e.index()];
            let args = vals(e.args);
            if args.is_empty() {
                format!("b{}", e.target.0)
            } else {
                format!("b{}({})", e.target.0, list(args, |v| format!("v{}", v.0)))
            }
        };
        let anchor = |a: Option<Anchor>| match a {
            Some(a) => format!(" at i{} {}..{}", a.item.0, a.start, a.end),
            None => String::new(),
        };
        let fail = |f: Option<BlockId>| f.map(|f| format!(" fail b{}", f.0)).unwrap_or_default();
        for (bi, block) in b.blocks.iter().enumerate() {
            let params = vals(block.params);
            if params.is_empty() {
                self.line(&format!("b{bi}:"));
            } else {
                self.line(&format!("b{bi}({}):", list(params, def)));
            }
            let start = block.insts.start as usize;
            for (k, inst) in block
                .insts
                .get(&b.insts)
                .unwrap_or_default()
                .iter()
                .enumerate()
            {
                let v = |v: &ValueId| format!("v{}", v.0);
                let text = match inst {
                    Inst::Const { result, bits } => {
                        format!(
                            "{} = const {}",
                            def(*result),
                            constant(b.values[result.index()], *bits)
                        )
                    }
                    Inst::ReadStatic { result, data } => {
                        format!("{} = static d{}", def(*result), data.0)
                    }
                    Inst::FunctionRef { result, function } => {
                        format!("{} = fnref f{}", def(*result), function.0)
                    }
                    Inst::Unary {
                        result,
                        op,
                        operand,
                        fail: f,
                    } => {
                        format!(
                            "{} = {} {}{}",
                            def(*result),
                            op.name(),
                            v(operand),
                            fail(*f)
                        )
                    }
                    Inst::Binary {
                        result,
                        op,
                        left,
                        right,
                        fail: f,
                    } => {
                        format!(
                            "{} = {} {}, {}{}",
                            def(*result),
                            op.name(),
                            v(left),
                            v(right),
                            fail(*f)
                        )
                    }
                    Inst::Compare {
                        result,
                        op,
                        left,
                        right,
                    } => {
                        format!("{} = {} {}, {}", def(*result), op.name(), v(left), v(right))
                    }
                    Inst::Convert {
                        result,
                        flag,
                        operand,
                        mode,
                    } => {
                        let flag = flag.map(|f| format!(", {}", def(f))).unwrap_or_default();
                        format!("{}{flag} = {} {}", def(*result), mode.name(), v(operand))
                    }
                    Inst::IntArith {
                        result,
                        flag,
                        op,
                        mode,
                        left,
                        right,
                    } => {
                        let flag = flag.map(|f| format!(", {}", def(f))).unwrap_or_default();
                        format!(
                            "{}{flag} = {}_{} {}, {}",
                            def(*result),
                            mode.name(),
                            op.name(),
                            v(left),
                            v(right)
                        )
                    }
                    Inst::MulAdd {
                        result,
                        left,
                        right,
                        addend,
                    } => {
                        format!(
                            "{} = mul_add {}, {}, {}",
                            def(*result),
                            v(left),
                            v(right),
                            v(addend)
                        )
                    }
                    Inst::ScalarUnary {
                        result,
                        op,
                        operand,
                    } => {
                        format!("{} = {} {}", def(*result), op.name(), v(operand))
                    }
                    Inst::Address { address, fail: f } => {
                        format!("address a{}{}", address.0, fail(*f))
                    }
                    Inst::Read {
                        result,
                        address,
                        mode,
                    } => {
                        let op = match mode {
                            ReadMode::Bits => "read",
                            ReadMode::Take => "take",
                        };
                        format!("{} = {op} a{}", def(*result), address.0)
                    }
                    Inst::Store { address, value } => format!("store a{}, {}", address.0, v(value)),
                    Inst::Pointer { result, address } => {
                        format!("{} = pointer a{}", def(*result), address.0)
                    }
                    Inst::StorageLive { slot } => format!("live s{}", slot.0),
                    Inst::StorageDead { slot } => format!("dead s{}", slot.0),
                    Inst::UpdateMask {
                        result,
                        mask,
                        field,
                        initialized,
                    } => {
                        let op = if *initialized {
                            "set_mask"
                        } else {
                            "clear_mask"
                        };
                        format!("{} = {op} {}, {field}", def(*result), v(mask))
                    }
                    Inst::Call { call } => {
                        let c = &b.calls[call.index()];
                        let results = vals(c.results);
                        let lhs = if results.is_empty() {
                            String::new()
                        } else {
                            format!("{} = ", list(results, def))
                        };
                        let callee = match c.callee {
                            Callee::Direct(f) => format!("f{}", f.0),
                            Callee::Indirect(v) => format!("v{}", v.0),
                            Callee::Runtime(op) => rt(op),
                        };
                        let args =
                            list(c.args.get(&b.pools.args).unwrap_or_default(), |a| match a {
                                Arg::Value(v) => format!("v{}", v.0),
                                Arg::Address(a) => format!("&a{}", a.0),
                            });
                        format!(
                            "{lhs}call {callee} g{} ({args}){}",
                            c.signature.0,
                            fail(c.fail)
                        )
                    }
                    Inst::Alloc {
                        result,
                        available,
                        region_entry,
                        kind,
                        place,
                        charge,
                        size,
                        align,
                        fail: f,
                    } => {
                        let kind = match kind {
                            AllocKind::Plain => "plain".into(),
                            AllocKind::Own => "own".into(),
                            AllocKind::Counted(c) => format!("counted {}", c.name()),
                            AllocKind::Resource { grant } => format!("resource grant {grant}"),
                        };
                        let place = match place {
                            AllocPlace::Current => String::new(),
                            AllocPlace::Like(b) => format!(" like v{}", b.0),
                            AllocPlace::Region(r) => format!(" in v{}", r.0),
                        };
                        format!(
                            "{}, {}{} = alloc {kind} {} align {align} charge {}{place}{}",
                            def(*result),
                            def(*available),
                            region_entry.map_or_else(String::new, |v| format!(", {}", def(v))),
                            v(size),
                            v(charge),
                            fail(*f)
                        )
                    }
                    Inst::Free { block, charge } => {
                        format!("free {} charge {}", v(block), v(charge))
                    }
                    Inst::Copy { to, from, bytes } => {
                        format!("copy a{}, a{}, {}", to.0, from.0, v(bytes))
                    }
                    Inst::Case { result, address } => {
                        format!("{} = case a{}", def(*result), address.0)
                    }
                    Inst::SetCase { address, case } => format!("set_case a{}, {case}", address.0),
                    Inst::RegionOpen { result } => format!("{} = region_open", def(*result)),
                    Inst::RegionClose { region } => format!("region_close {}", v(region)),
                    Inst::Trim {
                        block,
                        region,
                        entry,
                        previous,
                        charge,
                    } => {
                        format!(
                            "trim {} in {} entry {} from {} charge {}",
                            v(block),
                            v(region),
                            v(entry),
                            v(previous),
                            v(charge)
                        )
                    }
                    Inst::LimitEnter {
                        admitted,
                        available,
                        bytes,
                    } => format!(
                        "{}, {} = limit_enter {}",
                        def(*admitted),
                        def(*available),
                        v(bytes)
                    ),
                    Inst::LimitExit => "limit_exit".into(),
                    Inst::Drop {
                        address,
                        ty,
                        fail: f,
                    } => format!("drop a{} t{}{}", address.0, ty.0, fail(*f)),
                    Inst::Share { node, kind } => format!("share {} {}", v(node), kind.name()),
                    Inst::Release { node, kind, ty } => {
                        format!("release {} {} t{}", v(node), kind.name(), ty.0)
                    }
                    Inst::TryUnwrap { result, node, kind } => {
                        format!("{} = try_unwrap {} {}", def(*result), v(node), kind.name())
                    }
                };
                self.line(&format!("  {text}{}", anchor(b.inst_anchors[start + k])));
            }
            let term = match &b.terminators[bi] {
                Terminator::Jump(e) => format!("jump {}", edge(*e)),
                Terminator::Branch { condition, yes, no } => {
                    format!("branch v{}, {}, {}", condition.0, edge(*yes), edge(*no))
                }
                Terminator::Switch {
                    value,
                    arms,
                    otherwise,
                } => {
                    let arms = list(arms.get(&b.pools.arms).unwrap_or_default(), |a| {
                        format!("{} -> {}", a.value, edge(a.edge))
                    });
                    format!("switch v{} [{arms}] else {}", value.0, edge(*otherwise))
                }
                Terminator::Return(r) => {
                    let r = vals(*r);
                    if r.is_empty() {
                        "return".into()
                    } else {
                        format!("return {}", list(r, |v| format!("v{}", v.0)))
                    }
                }
                Terminator::Raise { cause, cleanup } => {
                    let cause = match cause {
                        RaiseCause::Unwrap => "unwrap".into(),
                        RaiseCause::Bounds => "bounds".into(),
                        RaiseCause::Fail(a) => format!("fail a{}", a.0),
                    };
                    let cleanup = cleanup
                        .map(|c| format!(" cleanup b{}", c.0))
                        .unwrap_or_default();
                    format!("raise {cause}{cleanup}")
                }
                Terminator::Propagate => "propagate".into(),
                Terminator::Unreachable => "unreachable".into(),
            };
            self.line(&format!("  {term}{}", anchor(block.anchor)));
        }
    }
}

// ---------------------------------------------------------------- parsing

#[derive(Debug, Eq, PartialEq)]
pub struct ParseError {
    pub line: usize,
    pub message: String,
}

pub fn parse(text: &str) -> Result<Module, ParseError> {
    let lines: Vec<(usize, Vec<String>)> = text
        .lines()
        .enumerate()
        .map(|(i, l)| {
            tokens(l).map(|t| (i + 1, t)).map_err(|message| ParseError {
                line: i + 1,
                message,
            })
        })
        .filter(|r| r.as_ref().map_or(true, |(_, t)| !t.is_empty()))
        .collect::<Result<_, _>>()?;
    let mut p = Parser {
        lines,
        at: 0,
        module: Module::default(),
    };
    p.module().map_err(|message| ParseError {
        line: p.current_line(),
        message,
    })?;
    Ok(p.module)
}

/// Split a line into words, punctuation and quoted strings (kept with their quotes,
/// unescaped inside); `;` outside a string starts a comment.
fn tokens(line: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut chars = line.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
        } else if c == ';' {
            break;
        } else if c == '"' {
            chars.next();
            let mut s = String::from("\"");
            loop {
                match chars.next() {
                    Some('\\') => s.push(chars.next().ok_or("unterminated escape")?),
                    Some('"') => break,
                    Some(c) => s.push(c),
                    None => return Err("unterminated string".into()),
                }
            }
            out.push(s);
        } else if "()[],:=&".contains(c) {
            chars.next();
            out.push(c.to_string());
        } else if c == '-' && chars.clone().nth(1) == Some('>') {
            chars.next();
            chars.next();
            out.push("->".into());
        } else {
            let mut w = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() || "()[],:=&;\"".contains(c) {
                    break;
                }
                w.push(c);
                chars.next();
            }
            out.push(w);
        }
    }
    Ok(out)
}

struct Parser {
    lines: Vec<(usize, Vec<String>)>,
    at: usize,
    module: Module,
}

/// One line's tokens.
struct Line<'a> {
    toks: &'a [String],
    at: usize,
}

type R<T> = Result<T, String>;

impl<'a> Line<'a> {
    fn peek(&self) -> Option<&'a str> {
        self.toks.get(self.at).map(String::as_str)
    }
    fn next(&mut self) -> R<&'a str> {
        let t = self.peek().ok_or("unexpected end of line")?;
        self.at += 1;
        Ok(t)
    }
    fn eat(&mut self, t: &str) -> bool {
        let hit = self.peek() == Some(t);
        if hit {
            self.at += 1;
        }
        hit
    }
    fn expect(&mut self, t: &str) -> R<()> {
        if self.eat(t) {
            Ok(())
        } else {
            Err(format!("expected `{t}`, found {:?}", self.peek()))
        }
    }
    fn end(&self) -> R<()> {
        match self.peek() {
            None => Ok(()),
            Some(t) => Err(format!("unexpected `{t}`")),
        }
    }
    fn id(&mut self, prefix: char) -> R<u32> {
        let t = self.next()?;
        t.strip_prefix(prefix)
            .and_then(|n| n.parse().ok())
            .ok_or_else(|| format!("expected {prefix}N, found `{t}`"))
    }
    fn num<T: std::str::FromStr>(&mut self) -> R<T> {
        let t = self.next()?;
        t.parse()
            .map_err(|_| format!("expected a number, found `{t}`"))
    }
    fn string(&mut self) -> R<String> {
        let t = self.next()?;
        t.strip_prefix('"')
            .map(str::to_owned)
            .ok_or_else(|| format!("expected a string, found `{t}`"))
    }
    /// `( item, ... )` or `[ item, ... ]`.
    fn seq<T>(
        &mut self,
        open: &str,
        close: &str,
        mut f: impl FnMut(&mut Self) -> R<T>,
    ) -> R<Vec<T>> {
        self.expect(open)?;
        let mut out = Vec::new();
        if self.eat(close) {
            return Ok(out);
        }
        loop {
            out.push(f(self)?);
            if self.eat(close) {
                return Ok(out);
            }
            self.expect(",")?;
        }
    }
    fn ty(&mut self) -> R<Ty> {
        let t = self.next()?;
        if t == "ptr" {
            return Ok(Ty::Ptr);
        }
        Scalar::from_name(t)
            .map(Ty::Scalar)
            .ok_or_else(|| format!("unknown type `{t}`"))
    }
    fn place(&mut self) -> R<PlaceTy> {
        if self.peek().is_some_and(|t| t.starts_with('t')) {
            Ok(PlaceTy::Declared(TypeId(self.id('t')?)))
        } else {
            Ok(PlaceTy::Value(self.ty()?))
        }
    }
    fn tref(&mut self) -> R<hirsel_core_layout::TypeRef> {
        Ok(hirsel_core_layout::TypeRef(self.id('t')?))
    }
    fn field_ty(&mut self) -> R<FieldTy> {
        use hirsel_core_layout::Handle;
        let boxed = |l: &mut Self| l.field_ty().map(Box::new);
        Ok(match self.peek().ok_or("expected a field type")? {
            "box" | "rc" | "arc" | "stable" | "link" | "optlink" => {
                let w = self.next()?;
                let t = self.tref()?;
                match w {
                    "box" => FieldTy::Box(t),
                    "rc" => FieldTy::Rc(t),
                    "arc" => FieldTy::Arc(t),
                    "stable" => FieldTy::Stable(t),
                    "link" => FieldTy::Link(t),
                    _ => FieldTy::OptLink(t),
                }
            }
            "seq" | "fixed" | "seqspan" | "indexed" => {
                let w = self.next()?;
                let e = boxed(self)?;
                match w {
                    "seq" => FieldTy::Seq(e),
                    "fixed" => FieldTy::FixedSpan(e),
                    "seqspan" => FieldTy::SeqSpan(e),
                    _ => FieldTy::IndexedSpan(e),
                }
            }
            "map" => {
                self.next()?;
                FieldTy::Map(boxed(self)?, boxed(self)?)
            }
            "array" => {
                self.next()?;
                let n = self.num::<u64>()?;
                FieldTy::Array(boxed(self)?, n)
            }
            w => {
                self.next()?;
                match w {
                    "fn" => FieldTy::Fn,
                    "ownfn" => FieldTy::OwnFn,
                    "borrow" => FieldTy::Borrow,
                    "task" => FieldTy::Handle(Handle::Task),
                    "sender" => FieldTy::Handle(Handle::Sender),
                    "receiver" => FieldTy::Handle(Handle::Receiver),
                    "cap" => FieldTy::Handle(Handle::Capability),
                    "causes" => FieldTy::Handle(Handle::Causes),
                    "hostfile" => FieldTy::Handle(Handle::HostFile),
                    "text" => FieldTy::Text,
                    "locator" => FieldTy::Locator,
                    w => match (Scalar::from_name(w), w.strip_prefix('t').map(str::parse)) {
                        (Some(s), _) => FieldTy::Scalar(s),
                        (None, Some(Ok(n))) => FieldTy::Inline(hirsel_core_layout::TypeRef(n)),
                        _ => return Err(format!("unknown field type `{w}`")),
                    },
                }
            }
        })
    }
    fn fields(&mut self) -> R<Vec<hirsel_core_layout::Field>> {
        self.expect("{")?;
        let mut out = Vec::new();
        if self.eat("}") {
            return Ok(out);
        }
        loop {
            let name = self.next()?.to_owned();
            self.expect(":")?;
            out.push(hirsel_core_layout::Field {
                name,
                ty: self.field_ty()?,
            });
            if self.eat("}") {
                return Ok(out);
            }
            self.expect(",")?;
        }
    }
    fn shape(&mut self) -> R<Shape> {
        use hirsel_core_layout::{Body as B, Case, FuncRef, Repr, Width};
        let repr = match self.next()? {
            "own" => Repr::Own,
            "resource" => Repr::Resource,
            "stable" => Repr::Stable,
            "scoped" => Repr::Scoped,
            "compact" => Repr::Compact(match self.next()? {
                "u16" => Width::U16,
                "u32" => Width::U32,
                "u64" => Width::U64,
                w => return Err(format!("bad compact width `{w}`")),
            }),
            r => return Err(format!("unknown representation `{r}`")),
        };
        let name = self.string()?;
        let finalizer = if self.eat("drop") {
            Some(FuncRef(self.id('f')?))
        } else {
            None
        };
        let grant = if self.eat("grant") { self.num()? } else { 0 };
        let body = if self.peek() == Some("|") {
            let mut cases = Vec::new();
            while self.eat("|") {
                let name = self.next()?.to_owned();
                cases.push(Case {
                    name,
                    fields: self.fields()?,
                });
            }
            B::Variant(cases)
        } else {
            B::Record(self.fields()?)
        };
        Ok(Shape {
            name,
            repr,
            body,
            finalizer,
            grant,
        })
    }
    fn value(&mut self) -> R<ValueId> {
        Ok(ValueId(self.id('v')?))
    }
    fn fail(&mut self) -> R<Option<BlockId>> {
        Ok(if self.eat("fail") {
            Some(BlockId(self.id('b')?))
        } else {
            None
        })
    }
    fn anchor(&mut self) -> R<Option<Anchor>> {
        if !self.eat("at") {
            return Ok(None);
        }
        let item = ItemId(self.id('i')?);
        let range = self.next()?;
        let (s, e) = range.split_once("..").ok_or("expected START..END")?;
        let bad = |_| format!("bad range `{range}`");
        Ok(Some(Anchor {
            item,
            start: s.parse().map_err(bad)?,
            end: e.parse().map_err(bad)?,
        }))
    }
}

/// Parse a constant written for type `t` (the inverse of `constant`).
pub(crate) fn parse_constant(t: Ty, s: &str) -> R<u64> {
    let bad = || format!("constant `{s}` does not fit {t:?}");
    if let Some(hex) = s.strip_prefix("0x") {
        if !(t.is_float() || t == Ty::Ptr) {
            return Err(bad());
        }
        let bits = u64::from_str_radix(hex, 16).map_err(|_| bad())?;
        return if t == Ty::Scalar(Scalar::F32) && bits > u64::from(u32::MAX) {
            Err(bad())
        } else {
            Ok(bits)
        };
    }
    match t {
        Ty::Scalar(Scalar::Bool) => match s {
            "true" => Ok(1),
            "false" => Ok(0),
            _ => Err(bad()),
        },
        Ty::Scalar(Scalar::Char) => {
            let c: u32 = s.parse().map_err(|_| bad())?;
            char::from_u32(c).map(|_| u64::from(c)).ok_or_else(bad)
        }
        t => match t.int() {
            Some((true, w)) => {
                let n: i64 = s.parse().map_err(|_| bad())?;
                let (lo, hi) = (-(1i128 << (w - 1)), (1i128 << (w - 1)) - 1);
                if (lo..=hi).contains(&i128::from(n)) {
                    Ok((n as u64) & (u64::MAX >> (64 - w)))
                } else {
                    Err(bad())
                }
            }
            Some((false, w)) => {
                let n: u64 = s.parse().map_err(|_| bad())?;
                if w == 64 || n >> w == 0 {
                    Ok(n)
                } else {
                    Err(bad())
                }
            }
            None => Err(bad()),
        },
    }
}

fn counted(l: &mut Line) -> R<Counted> {
    let t = l.next()?;
    Counted::from_name(t).ok_or_else(|| format!("expected rc or arc, found `{t}`"))
}

/// The per-body state while parsing a function.
#[derive(Default)]
struct BodyParser {
    body: Body,
    types: Vec<Option<Ty>>,
}

impl BodyParser {
    fn define(&mut self, v: ValueId, t: Ty) -> R<()> {
        let i = v.index();
        if self.types.len() <= i {
            self.types.resize(i + 1, None);
        }
        if self.types[i].replace(t).is_some() {
            return Err(format!("v{} defined twice", v.0));
        }
        Ok(())
    }
    fn def(&mut self, l: &mut Line) -> R<ValueId> {
        let v = l.value()?;
        l.expect(":")?;
        let t = l.ty()?;
        self.define(v, t)?;
        Ok(v)
    }
    fn edge(&mut self, l: &mut Line) -> R<EdgeId> {
        let target = BlockId(l.id('b')?);
        let args = if l.peek() == Some("(") {
            l.seq("(", ")", Line::value)?
        } else {
            Vec::new()
        };
        let args = PoolRange::push(&mut self.body.pools.values, args);
        self.body.edges.push(Edge { target, args });
        Ok(EdgeId(self.body.edges.len() as u32 - 1))
    }
    fn ty_of(&self, v: ValueId) -> R<Ty> {
        self.types
            .get(v.index())
            .copied()
            .flatten()
            .ok_or_else(|| format!("v{} used before its type", v.0))
    }

    fn inst(&mut self, l: &mut Line) -> R<Inst> {
        // Instructions without results first.
        match l.peek() {
            Some("address") => {
                l.next()?;
                return Ok(Inst::Address {
                    address: AddressId(l.id('a')?),
                    fail: l.fail()?,
                });
            }
            Some("store") => {
                l.next()?;
                let address = AddressId(l.id('a')?);
                l.expect(",")?;
                return Ok(Inst::Store {
                    address,
                    value: l.value()?,
                });
            }
            Some("live") => {
                l.next()?;
                return Ok(Inst::StorageLive {
                    slot: SlotId(l.id('s')?),
                });
            }
            Some("dead") => {
                l.next()?;
                return Ok(Inst::StorageDead {
                    slot: SlotId(l.id('s')?),
                });
            }
            Some("call") => return self.call(l, Vec::new()),
            Some("free") => {
                l.next()?;
                let block = l.value()?;
                l.expect("charge")?;
                return Ok(Inst::Free {
                    block,
                    charge: l.value()?,
                });
            }
            Some("copy") => {
                l.next()?;
                let to = AddressId(l.id('a')?);
                l.expect(",")?;
                let from = AddressId(l.id('a')?);
                l.expect(",")?;
                return Ok(Inst::Copy {
                    to,
                    from,
                    bytes: l.value()?,
                });
            }
            Some("set_case") => {
                l.next()?;
                let address = AddressId(l.id('a')?);
                l.expect(",")?;
                return Ok(Inst::SetCase {
                    address,
                    case: l.num()?,
                });
            }
            Some("region_close") => {
                l.next()?;
                return Ok(Inst::RegionClose { region: l.value()? });
            }
            Some("trim") => {
                l.next()?;
                let block = l.value()?;
                l.expect("in")?;
                let region = l.value()?;
                l.expect("entry")?;
                let entry = l.value()?;
                l.expect("from")?;
                let previous = l.value()?;
                l.expect("charge")?;
                return Ok(Inst::Trim {
                    block,
                    region,
                    entry,
                    previous,
                    charge: l.value()?,
                });
            }
            Some("limit_exit") => {
                l.next()?;
                return Ok(Inst::LimitExit);
            }
            Some("drop") => {
                l.next()?;
                let address = AddressId(l.id('a')?);
                return Ok(Inst::Drop {
                    address,
                    ty: TypeId(l.id('t')?),
                    fail: l.fail()?,
                });
            }
            Some("share") => {
                l.next()?;
                let node = l.value()?;
                return Ok(Inst::Share {
                    node,
                    kind: counted(l)?,
                });
            }
            Some("release") => {
                l.next()?;
                let node = l.value()?;
                let kind = counted(l)?;
                return Ok(Inst::Release {
                    node,
                    kind,
                    ty: TypeId(l.id('t')?),
                });
            }
            _ => {}
        }
        let mut defs = vec![self.def(l)?];
        while l.eat(",") {
            defs.push(self.def(l)?);
        }
        l.expect("=")?;
        if l.peek() == Some("call") {
            return self.call(l, defs);
        }
        let op = l.next()?;
        let result = defs[0];
        let one = |defs: &[ValueId]| {
            if defs.len() == 1 {
                Ok(())
            } else {
                Err(format!("`{op}` has one result"))
            }
        };
        let flag = |defs: &[ValueId], wanted: bool| match (defs.len(), wanted) {
            (1, false) => Ok(None),
            (2, true) => Ok(Some(defs[1])),
            _ => Err(format!(
                "`{op}` takes {} results",
                if wanted { 2 } else { 1 }
            )),
        };
        let two = |l: &mut Line| -> R<(ValueId, ValueId)> {
            let a = l.value()?;
            l.expect(",")?;
            Ok((a, l.value()?))
        };
        let pair = |defs: &[ValueId]| match defs {
            [a, b] => Ok((*a, *b)),
            _ => Err(format!("`{op}` takes 2 results")),
        };
        let inst = if op == "alloc" {
            let (result, available, region_entry) = match defs.as_slice() {
                [a, b] => (*a, *b, None),
                [a, b, c] => (*a, *b, Some(*c)),
                _ => return Err("alloc takes pointer, room and optionally region entry".into()),
            };
            let kind = match l.next()? {
                "plain" => AllocKind::Plain,
                "own" => AllocKind::Own,
                "counted" => AllocKind::Counted(counted(l)?),
                "resource" => {
                    l.expect("grant")?;
                    AllocKind::Resource { grant: l.num()? }
                }
                k => return Err(format!("unknown allocation kind `{k}`")),
            };
            let size = l.value()?;
            l.expect("align")?;
            let align = l.num()?;
            l.expect("charge")?;
            let charge = l.value()?;
            let place = if l.eat("like") {
                AllocPlace::Like(l.value()?)
            } else if l.eat("in") {
                AllocPlace::Region(l.value()?)
            } else {
                AllocPlace::Current
            };
            Inst::Alloc {
                result,
                available,
                region_entry,
                kind,
                place,
                charge,
                size,
                align,
                fail: l.fail()?,
            }
        } else if op == "limit_enter" {
            let (admitted, available) = pair(&defs)?;
            Inst::LimitEnter {
                admitted,
                available,
                bytes: l.value()?,
            }
        } else if op == "case" {
            one(&defs)?;
            Inst::Case {
                result,
                address: AddressId(l.id('a')?),
            }
        } else if op == "region_open" {
            one(&defs)?;
            Inst::RegionOpen { result }
        } else if op == "try_unwrap" {
            one(&defs)?;
            Inst::TryUnwrap {
                result,
                node: l.value()?,
                kind: counted(l)?,
            }
        } else if op == "const" {
            one(&defs)?;
            Inst::Const {
                result,
                bits: parse_constant(self.ty_of(result)?, l.next()?)?,
            }
        } else if op == "static" {
            one(&defs)?;
            Inst::ReadStatic {
                result,
                data: StaticId(l.id('d')?),
            }
        } else if op == "fnref" {
            one(&defs)?;
            Inst::FunctionRef {
                result,
                function: FuncId(l.id('f')?),
            }
        } else if op == "mul_add" {
            one(&defs)?;
            let (left, right) = two(l)?;
            l.expect(",")?;
            Inst::MulAdd {
                result,
                left,
                right,
                addend: l.value()?,
            }
        } else if op == "pointer" {
            one(&defs)?;
            Inst::Pointer {
                result,
                address: AddressId(l.id('a')?),
            }
        } else if op == "read" || op == "take" {
            one(&defs)?;
            let mode = if op == "read" {
                ReadMode::Bits
            } else {
                ReadMode::Take
            };
            Inst::Read {
                result,
                address: AddressId(l.id('a')?),
                mode,
            }
        } else if op == "set_mask" || op == "clear_mask" {
            one(&defs)?;
            let mask = l.value()?;
            l.expect(",")?;
            Inst::UpdateMask {
                result,
                mask,
                field: l.num()?,
                initialized: op == "set_mask",
            }
        } else if let Some(op) = UnaryOp::from_name(op) {
            one(&defs)?;
            Inst::Unary {
                result,
                op,
                operand: l.value()?,
                fail: l.fail()?,
            }
        } else if let Some(op) = BinaryOp::from_name(op) {
            one(&defs)?;
            let (left, right) = two(l)?;
            Inst::Binary {
                result,
                op,
                left,
                right,
                fail: l.fail()?,
            }
        } else if let Some(op) = CompareOp::from_name(op) {
            one(&defs)?;
            let (left, right) = two(l)?;
            Inst::Compare {
                result,
                op,
                left,
                right,
            }
        } else if let Some(mode) = ConvertMode::from_name(op) {
            Inst::Convert {
                result,
                flag: flag(&defs, mode.has_flag())?,
                operand: l.value()?,
                mode,
            }
        } else if let Some(op) = ScalarUnaryOp::from_name(op) {
            one(&defs)?;
            Inst::ScalarUnary {
                result,
                op,
                operand: l.value()?,
            }
        } else if let Some((mode, aop)) = op
            .split_once('_')
            .and_then(|(m, o)| Some((ArithMode::from_name(m)?, ArithOp::from_name(o)?)))
        {
            let (left, right) = two(l)?;
            Inst::IntArith {
                result,
                flag: flag(&defs, mode == ArithMode::Checked)?,
                op: aop,
                mode,
                left,
                right,
            }
        } else {
            return Err(format!("unknown operation `{op}`"));
        };
        Ok(inst)
    }

    fn call(&mut self, l: &mut Line, results: Vec<ValueId>) -> R<Inst> {
        l.expect("call")?;
        let t = l.next()?;
        let callee = if let Some(f) = t.strip_prefix('f') {
            Callee::Direct(FuncId(f.parse().map_err(|_| format!("bad callee `{t}`"))?))
        } else if let Some(v) = t.strip_prefix('v') {
            Callee::Indirect(ValueId(v.parse().map_err(|_| format!("bad callee `{t}`"))?))
        } else if let Some(name) = t.strip_prefix("rt.") {
            let mut tys = |n: usize| -> R<Vec<TypeId>> {
                let ts = l.seq("[", "]", |l| Ok(TypeId(l.id('t')?)))?;
                if ts.len() == n {
                    Ok(ts)
                } else {
                    Err(format!("`{t}` takes {n} types"))
                }
            };
            Callee::Runtime(match name {
                "spawn" => {
                    let ts = tys(2)?;
                    RtOp::Spawn { x: ts[0], r: ts[1] }
                }
                "join" => RtOp::Join { r: tys(1)?[0] },
                "cancel" => RtOp::Cancel,
                "check_cancel" => RtOp::CheckCancel,
                "channel_new" => RtOp::ChannelNew { item: tys(1)?[0] },
                "channel_sender" => RtOp::ChannelSender,
                "send" => RtOp::Send { item: tys(1)?[0] },
                "try_send" => RtOp::TrySend { item: tys(1)?[0] },
                "receive" => RtOp::Receive { item: tys(1)?[0] },
                "dest_id" => RtOp::DestId,
                "compare_bytes" => RtOp::CompareBytes,
                "args_len" => RtOp::ArgsLen,
                "args_get" => RtOp::ArgsGet,
                _ => match HostOp::ALL.into_iter().find(|op| op.name() == name) {
                    Some(op) => RtOp::Host(op),
                    None => return Err(format!("unknown engine operation `{t}`")),
                },
            })
        } else {
            return Err(format!("bad callee `{t}`"));
        };
        let signature = SigId(l.id('g')?);
        let args = l.seq("(", ")", |l| {
            if l.eat("&") {
                Ok(Arg::Address(AddressId(l.id('a')?)))
            } else {
                Ok(Arg::Value(l.value()?))
            }
        })?;
        let args = PoolRange::push(&mut self.body.pools.args, args);
        let results = PoolRange::push(&mut self.body.pools.values, results);
        self.body.calls.push(CallSite {
            callee,
            signature,
            args,
            results,
            fail: l.fail()?,
        });
        Ok(Inst::Call {
            call: CallId(self.body.calls.len() as u32 - 1),
        })
    }

    fn terminator(&mut self, l: &mut Line) -> R<Option<Terminator>> {
        let t = match l.peek() {
            Some("jump") => {
                l.next()?;
                Terminator::Jump(self.edge(l)?)
            }
            Some("branch") => {
                l.next()?;
                let condition = l.value()?;
                l.expect(",")?;
                let yes = self.edge(l)?;
                l.expect(",")?;
                Terminator::Branch {
                    condition,
                    yes,
                    no: self.edge(l)?,
                }
            }
            Some("switch") => {
                l.next()?;
                let value = l.value()?;
                let arms = l.seq("[", "]", |l| {
                    let value = l.num()?;
                    l.expect("->")?;
                    Ok(SwitchArm {
                        value,
                        edge: self.edge(l)?,
                    })
                })?;
                let arms = PoolRange::push(&mut self.body.pools.arms, arms);
                l.expect("else")?;
                Terminator::Switch {
                    value,
                    arms,
                    otherwise: self.edge(l)?,
                }
            }
            Some("return") => {
                l.next()?;
                let mut vals = Vec::new();
                if l.peek().is_some_and(|t| t.starts_with('v')) {
                    vals.push(l.value()?);
                    while l.eat(",") {
                        vals.push(l.value()?);
                    }
                }
                Terminator::Return(PoolRange::push(&mut self.body.pools.values, vals))
            }
            Some("raise") => {
                l.next()?;
                let cause = match l.next()? {
                    "unwrap" => RaiseCause::Unwrap,
                    "bounds" => RaiseCause::Bounds,
                    "fail" => RaiseCause::Fail(AddressId(l.id('a')?)),
                    c => return Err(format!("unknown cause `{c}`")),
                };
                let cleanup = if l.eat("cleanup") {
                    Some(BlockId(l.id('b')?))
                } else {
                    None
                };
                Terminator::Raise { cause, cleanup }
            }
            Some("propagate") => {
                l.next()?;
                Terminator::Propagate
            }
            Some("unreachable") => {
                l.next()?;
                Terminator::Unreachable
            }
            _ => return Ok(None),
        };
        Ok(Some(t))
    }
}

impl Parser {
    fn current_line(&self) -> usize {
        self.lines.get(self.at.saturating_sub(1)).map_or(0, |l| l.0)
    }
    fn line(&mut self) -> Option<Line<'_>> {
        let l = self.lines.get(self.at)?;
        self.at += 1;
        Some(Line { toks: &l.1, at: 0 })
    }
    fn peek_word(&self) -> Option<&str> {
        self.lines
            .get(self.at)
            .and_then(|l| l.1.first())
            .map(String::as_str)
    }

    fn module(&mut self) -> R<()> {
        let v1;
        {
            let mut l = self.line().ok_or("empty input")?;
            l.expect("core")?;
            v1 = match l.next()? {
                "v0" => false,
                "v1" => true,
                v => return Err(format!("unknown form `{v}`")),
            };
            l.end()?;
        }
        while let Some(word) = self.peek_word().map(str::to_owned) {
            let index = |len: usize| len as u32;
            match word.as_str() {
                "item" => {
                    let n = index(self.module.items.len());
                    let mut l = self.line().unwrap();
                    l.next()?;
                    expect_index(&mut l, 'i', n)?;
                    let t = l.next()?;
                    let path = DefPath::from_full(t)
                        .ok_or_else(|| format!("bad definition path `{t}`"))?;
                    l.end()?;
                    self.module.items.push(path);
                }
                "type" => {
                    let n = index(self.module.types.len());
                    let mut l = self.line().unwrap();
                    l.next()?;
                    expect_index(&mut l, 't', n)?;
                    let s = l.shape()?;
                    l.end()?;
                    self.module.types.push(s);
                }
                "static" => {
                    let n = index(self.module.statics.len());
                    let mut l = self.line().unwrap();
                    l.next()?;
                    expect_index(&mut l, 'd', n)?;
                    l.expect("align")?;
                    let align = l.num()?;
                    l.expect("hex")?;
                    let hex = if l.peek().is_some() { l.next()? } else { "" };
                    l.end()?;
                    let bytes = (0..hex.len() / 2)
                        .map(|i| u8::from_str_radix(hex.get(2 * i..2 * i + 2).unwrap_or("x"), 16))
                        .collect::<Result<Vec<_>, _>>()
                        .ok()
                        .filter(|_| hex.len() % 2 == 0)
                        .ok_or("bad hex")?;
                    self.module.statics.push(Static { align, bytes });
                }
                "sig" => {
                    let n = index(self.module.signatures.len());
                    let mut l = self.line().unwrap();
                    l.next()?;
                    expect_index(&mut l, 'g', n)?;
                    let params = l.seq("(", ")", |l| {
                        if l.eat("&") {
                            Ok(Param::Address(TypeId(l.id('t')?)))
                        } else {
                            Ok(Param::Value(l.ty()?))
                        }
                    })?;
                    l.expect("->")?;
                    let never = l.eat("never");
                    let results = if never {
                        Vec::new()
                    } else {
                        l.seq("(", ")", Line::ty)?
                    };
                    let dest = if l.eat("dest") {
                        Some(TypeId(l.id('t')?))
                    } else {
                        None
                    };
                    l.end()?;
                    self.module.signatures.push(Signature {
                        params,
                        results,
                        dest,
                        never,
                    });
                }
                "inst" => {
                    let n = index(self.module.instances.len());
                    let mut l = self.line().unwrap();
                    l.next()?;
                    expect_index(&mut l, 'n', n)?;
                    let t = l.next()?;
                    let definition = DefPath::from_full(t)
                        .ok_or_else(|| format!("bad definition path `{t}`"))?;
                    let types = l.seq("[", "]", Line::string)?;
                    let dictionaries = l.seq("[", "]", Line::string)?;
                    l.end()?;
                    self.module.instances.push(InstanceKey {
                        definition,
                        types,
                        dictionaries,
                    });
                }
                "fn" => self.function()?,
                w => return Err(format!("unexpected `{w}`")),
            }
        }
        if !v1 && uses_v1(&self.module) {
            return Err("a FORM v1 construct under `core v0`".into());
        }
        Ok(())
    }

    fn function(&mut self) -> R<()> {
        let n = self.module.functions.len() as u32;
        let (instance, signature, external) = {
            let mut l = self.line().unwrap();
            l.next()?;
            expect_index(&mut l, 'f', n)?;
            let instance = InstanceId(l.id('n')?);
            let signature = SigId(l.id('g')?);
            let external = l.eat("extern");
            if !external {
                l.expect("{")?;
            }
            l.end()?;
            (instance, signature, external)
        };
        let body = if external { None } else { Some(self.body()?) };
        self.module.functions.push(Function {
            instance,
            signature,
            body,
        });
        Ok(())
    }

    fn body(&mut self) -> R<Body> {
        let mut b = BodyParser::default();
        let mut open: Option<usize> = None; // the block whose terminator is pending
        loop {
            let Some(mut l) = self.line() else {
                return Err("unterminated function".into());
            };
            match l.peek() {
                Some("}") => {
                    l.next()?;
                    l.end()?;
                    if open.is_some() {
                        return Err("block without terminator".into());
                    }
                    break;
                }
                Some("slot") if b.body.blocks.is_empty() => {
                    l.next()?;
                    expect_index(&mut l, 's', b.body.slots.len() as u32)?;
                    let t = l.place()?;
                    l.end()?;
                    b.body.slots.push(t);
                }
                Some("addr") if b.body.blocks.is_empty() => {
                    l.next()?;
                    expect_index(&mut l, 'a', b.body.addresses.len() as u32)?;
                    let t = l.next()?;
                    let num = |p: char| t.strip_prefix(p).and_then(|n| n.parse().ok());
                    let base = if let Some(n) = num('s') {
                        AddressBase::Slot(SlotId(n))
                    } else if let Some(n) = num('v') {
                        AddressBase::Value(ValueId(n))
                    } else if let Some(n) = num('a') {
                        AddressBase::Address(AddressId(n))
                    } else {
                        return Err(format!("bad address base `{t}`"));
                    };
                    let mut steps = Vec::new();
                    while !l.eat(":") {
                        steps.push(match l.next()? {
                            "deref" => Step::Deref,
                            "field" => Step::Field(l.num()?),
                            "case" => Step::CaseField {
                                case: l.num()?,
                                field: l.num()?,
                            },
                            "index" => Step::Index(l.value()?),
                            "element" => Step::Element(l.value()?),
                            "byte" => Step::Byte(l.value()?),
                            s => return Err(format!("unknown step `{s}`")),
                        });
                    }
                    let ty = l.place()?;
                    l.end()?;
                    let steps = PoolRange::push(&mut b.body.pools.steps, steps);
                    b.body.addresses.push(AddressInfo { base, steps, ty });
                }
                Some(t) if t.starts_with('b') && open.is_none() => {
                    expect_index(&mut l, 'b', b.body.blocks.len() as u32)?;
                    let params = if l.peek() == Some("(") {
                        l.seq("(", ")", |l| b.def(l))?
                    } else {
                        Vec::new()
                    };
                    l.expect(":")?;
                    l.end()?;
                    let params = PoolRange::push(&mut b.body.pools.values, params);
                    let start = b.body.insts.len() as u32;
                    b.body.blocks.push(Block {
                        params,
                        insts: PoolRange::new(start, 0),
                        anchor: None,
                    });
                    open = Some(b.body.blocks.len() - 1);
                }
                _ => {
                    let Some(block) = open else {
                        return Err("instruction outside a block".into());
                    };
                    if let Some(t) = b.terminator(&mut l)? {
                        let anchor = l.anchor()?;
                        l.end()?;
                        b.body.blocks[block].anchor = anchor;
                        b.body.terminators.push(t);
                        open = None;
                    } else {
                        let inst = b.inst(&mut l)?;
                        let anchor = l.anchor()?;
                        l.end()?;
                        b.body.insts.push(inst);
                        b.body.inst_anchors.push(anchor);
                        b.body.blocks[block].insts.len += 1;
                    }
                }
            }
        }
        b.body.values = b
            .types
            .iter()
            .enumerate()
            .map(|(i, t)| t.ok_or_else(|| format!("v{i} is never defined")))
            .collect::<R<_>>()?;
        Ok(b.body)
    }
}

fn expect_index(l: &mut Line, prefix: char, n: u32) -> R<()> {
    let got = l.id(prefix)?;
    if got != n {
        return Err(format!("expected {prefix}{n}, found {prefix}{got}"));
    }
    if l.peek() == Some("=") {
        l.next()?;
    }
    Ok(())
}
