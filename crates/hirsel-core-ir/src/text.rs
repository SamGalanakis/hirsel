//! The text form: `print` and `parse`, line-oriented, one definition, instruction or
//! terminator per line, `;` to end of line a comment. `parse(&print(m)) == m` for
//! every module `parse` produced; a module built in another table order prints
//! and parses to the same program with its tables in reading order.
//!
//! ```text
//! core v1
//! item i0 = app@1/main/add                ; source items (debug identity)
//! type t0 = "app@1/geo/Point"             ; declared types, canonical text
//! static d0 = align 1 hex 6869            ; read-only data
//! sig g0 = (i32, &view t0) -> (i32) dest t0 ; params: scalar, ptr or &mut/view/take tN
//! sig g1 = (u64) -> never                 ; FORM v1: a function whose result is Never
//! sig g2 = (&view t0) -> () dest view t0 in (0) ; a dest view anchored on params 0… (OWN-20)
//! inst n0 = app@1/main/add [t0] []        ; instance row: definition, argument rows, dictionaries
//! fn f0 = n0 g0 extern                    ; defined in another module
//! fn f1 = n1 g1 {
//!   slot s0 = i32
//!   addr a0 = s0 field 1 index v2 deref : i32   ; base, steps, place type
//! b0(v0: i32, v1: i32):
//!   v2: i32 = add v0, v1 fail b1 at i0 4..9
//!   return v2 at i0 0..20
//! }
//! ```

mod allocation;
mod descriptor_text;
mod runtime;
mod value_text;
use runtime::rt;
use value_text::constant;
use value_text::static_value;

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
    BitAnd = "bitand", BitOr = "bitor", BitXor = "bitxor", Min = "min", Max = "max", MinNumber = "min_number", MaxNumber = "max_number", Rotl = "rotl", MulHigh = "mul_high",
});
names!(CompareOp { Eq = "eq", Ne = "ne", Lt = "lt", Le = "le", Gt = "gt", Ge = "ge" });
names!(ConvertMode {
    Widen = "widen", Truncate = "truncate", TryNarrow = "try_narrow", FloatToInt = "float_to_int",
    IntToFloat = "int_to_float", Reinterpret = "reinterpret", FloatResize = "float_resize",
});
names!(ArithMode { Wrapping = "wrapping", Saturating = "saturating", Checked = "checked" });
names!(SeqWord { Data = "data", Len = "len", Cap = "cap" });
names!(ArithOp { Add = "add", Sub = "sub", Mul = "mul", Div = "div" });
names!(ScalarUnaryOp {
    Sqrt = "sqrt", Floor = "floor", Ceil = "ceil", Trunc = "trunc", Round = "round", RoundEven = "round_even", Abs = "abs",
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

fn ty(t: Ty) -> std::borrow::Cow<'static, str> {
    match t {
        Ty::Scalar(s) => s.name().into(),
        Ty::Ptr => "ptr".into(),
        Ty::Vector(v) => format!("v{}x{}", v.lanes(), v.lane().name()).into(),
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
mod handle_names;
use handle_names::HANDLE_NAMES;

fn field_ty(f: &FieldTy) -> String {
    match f {
        FieldTy::Scalar(s) => s.name().into(),
        FieldTy::Inline(t) => format!("t{}", t.0),
        FieldTy::Box(t) => format!("box t{}", t.0),
        FieldTy::Seq(e) => format!("seq {}", field_ty(e)),
        FieldTy::Columns(e) => format!("columns {}", field_ty(e)),
        FieldTy::FrozenBytes => "frozen_bytes".into(),
        FieldTy::Rc(t) => format!("rc t{}", t.0),
        FieldTy::Arc(t) => format!("arc t{}", t.0),
        FieldTy::Shared(t) => format!("shared t{}", t.0),
        FieldTy::RegionRef(t) => format!("region_ref t{}", t.0),
        FieldTy::Array(e, n) => format!("array {n} {}", field_ty(e)),
        FieldTy::Fn => "fn".into(),
        FieldTy::OwnFn => "ownfn".into(),
        FieldTy::Borrow => "borrow".into(),
        FieldTy::View(e) => format!("view {}", field_ty(e)),
        FieldTy::Handle(handle) => HANDLE_NAMES
            .iter()
            .find_map(|&(name, kind)| (*handle == kind).then_some(name))
            .expect("every Handle has a text spelling")
            .into(),
        FieldTy::Map(k, v) => format!("map {} {}", field_ty(k), field_ty(v)),
        FieldTy::Text => "text".into(),
        FieldTy::FixedSpan(e) => format!("fixed {}", field_ty(e)),
        FieldTy::SeqSpan(e) => format!("seqspan {}", field_ty(e)),
        FieldTy::IndexedSpan(e) => format!("indexed {}", field_ty(e)),
        FieldTy::Link(t) => format!("link t{}", t.0),
        FieldTy::Locator(t) => format!("locator {}", field_ty(t)),
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
/// `own "Pair" { a: u64 }`, `own "u64" value u64 { 0: u64 }`, `own "()" nothing { }`,
/// `resource "Temp" drop f3 grant 64 { .. }`, `own "u64" value u64 held u64 { 0: u64 }`,
/// `compact u32 "Row" { .. }`, `own "Opt" | None { } | Some { v: u64 }`, `own "Row" codec f4 { .. }`.
fn shape(s: &Shape) -> String {
    use hirsel_core_layout::{Body as B, Repr, Width};
    let repr = match s.repr {
        Repr::Own => "own".into(),
        Repr::Resource => "resource".into(),
        Repr::Region => "region".into(),
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
    match s.carried {
        Carried::Address => {}
        Carried::Value(t) => out += &format!(" value {}", ty(t)),
        Carried::Nothing => out += " nothing",
    }
    if let Some(f) = s.finalizer {
        out += &format!(" drop f{}", f.0);
    }
    if let Some(f) = s.clone {
        out += &format!(" clone f{}", f.0);
    }
    if let Some(f) = s.codec {
        out += &format!(" codec f{}", f.0);
    }
    if let Some(f) = s.key_codec {
        out += &format!(" key_codec f{}", f.0);
    }
    if s.grant != 0 {
        out += &format!(" grant {}", s.grant);
    }
    if s.send {
        out += " send";
    }
    if let Some(held) = &s.held {
        out += &format!(" held {}", field_ty(held));
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
fn list<T>(items: impl IntoIterator<Item = T>, f: impl Fn(T) -> String) -> String {
    items.into_iter().map(f).collect::<Vec<_>>().join(", ")
}

impl Printer {
    fn line(&mut self, s: &str) {
        self.out.push_str(s);
        self.out.push('\n');
    }

    fn module(&mut self, m: &Module) {
        self.line("core v1");
        for (i, item) in m.items.iter().enumerate() {
            self.line(&format!("item i{i} = {item}"));
        }
        for (i, t) in m.types.iter().enumerate() {
            self.line(&format!("type t{i} = {}", shape(t)));
        }
        for (i, s) in m.statics.iter().enumerate() {
            match s {
                Static::Bytes { align, bytes } => {
                    let hex = hirsel_core_ids::hex::encode(bytes);
                    self.line(&format!("static d{i} = align {align} hex {hex}"));
                }
                Static::Aggregate { ty, value } => {
                    self.line(&format!(
                        "static d{i} = aggregate t{} {}",
                        ty.0,
                        static_value(value)
                    ));
                }
                Static::Computed { ty, constant } => {
                    self.line(&format!(
                        "static d{i} = computed {} {constant}",
                        field_ty(ty)
                    ));
                }
            }
        }
        for (i, s) in m.signatures.iter().enumerate() {
            let params = list(&s.params, |p| match p {
                Param::Value(t) => ty(*t).into(),
                Param::Address { place: p, loan } => format!("&{} {}", loan_word(*loan), place(*p)),
            });
            let results = if s.never {
                "never".into()
            } else {
                format!("({})", list(&s.results, |t| ty(*t).into()))
            };
            let dest = match (s.dest, s.anchors.as_slice()) {
                (None, _) => String::new(),
                (Some(t), []) => format!(" dest t{}", t.0),
                (Some(t), k) => format!(" dest view t{} in ({})", t.0, list(k, |k| k.to_string())),
            };
            self.line(&format!("sig g{i} = ({params}) -> {results}{dest}"));
        }
        for (i, k) in m.instances.iter().enumerate() {
            let types = list(&k.arguments, |t| format!("t{}", t.0));
            let dicts = list(&k.dictionaries, |d| quote(d));
            self.line(&format!("inst n{i} = {} [{types}] [{dicts}]", k.definition));
        }
        for (i, f) in m.functions.iter().enumerate() {
            let mut head = format!("fn f{i} = n{} g{}", f.instance.0, f.signature.0);
            if let Some(hash) = m.code_variants.get(&FuncId(i as u32)) {
                head += &format!(" code {hash}");
            }
            if f.versioned {
                head += " versioned";
            }
            if f.cold {
                head += " cold";
            }
            if f.pack_export {
                head += " pack_export";
            }
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
        for v in &b.callable_parameters {
            self.line(&format!("  callable v{}", v.0));
        }
        for root in &b.compact_roots {
            self.line(&format!("  root t{} {}", root.ty.0, root.width));
        }
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
                    Step::Seq(w) => format!(" seq {}", w.name()),
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
            let cold = if e.cold { " cold" } else { "" };
            if args.is_empty() {
                format!("b{}{cold}", e.target.0)
            } else {
                format!(
                    "b{}({}){cold}",
                    e.target.0,
                    list(args, |v| format!("v{}", v.0))
                )
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
                    Inst::Simd {
                        result,
                        op,
                        first,
                        second,
                        fail: target,
                    } => {
                        let args = second.map(|s| format!(", v{}", s.0)).unwrap_or_default();
                        format!(
                            "{} = simd {} {}{args}{}",
                            def(*result),
                            op.name(),
                            v(first),
                            fail(*target)
                        )
                    }
                    Inst::PutLanes { to, vector, count } => {
                        format!("put_lanes a{}, {}, {}", to.0, v(vector), v(count))
                    }
                    Inst::Const { result, bits } => {
                        format!(
                            "{} = const {}",
                            def(*result),
                            constant(b.values[result.index()], *bits)
                        )
                    }
                    Inst::LayoutConst { result, query } => match query {
                        LayoutQuery::FieldSize(field) => {
                            format!("{} = field_size {}", def(*result), field_ty(field))
                        }
                        LayoutQuery::HeldSize(ty) => {
                            format!("{} = held_size t{}", def(*result), ty.0)
                        }
                        LayoutQuery::FrameCapacity(field) => {
                            format!("{} = frame_capacity {}", def(*result), field_ty(field))
                        }
                        LayoutQuery::BlockSize { ty, prefix } => {
                            format!("{} = block_size t{} {prefix}", def(*result), ty.0)
                        }
                        LayoutQuery::Target => format!("{} = target", def(*result)),
                    },
                    Inst::ReadStatic { result, data } => {
                        format!("{} = static d{}", def(*result), data.0)
                    }
                    Inst::FunctionRef { result, function } => {
                        format!("{} = fnref f{}", def(*result), function.0)
                    }
                    Inst::DescriptorRef { result, .. } => {
                        descriptor_text::print(def(*result), inst)
                    }
                    Inst::CompactWidth { result, word, ty } => {
                        format!("{} = width {} t{}", def(*result), v(word), ty.0)
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
                            Callee::PackExport(v) => format!("export.v{}", v.0),
                            Callee::Runtime(op) => rt(op),
                        };
                        let args =
                            list(c.args.get(&b.pools.args).unwrap_or_default(), |a| match a {
                                Arg::Value(v) => format!("v{}", v.0),
                                Arg::Address(a) => format!("&a{}", a.0),
                            });
                        format!(
                            "{lhs}call {callee} g{} ({args}){}{}",
                            c.signature.0,
                            fail(c.fail),
                            if c.cold { " cold" } else { "" }
                        )
                    }
                    Inst::Alloc {
                        result,
                        available,
                        region_entry,
                        kind,
                        place,
                        charge,
                        overflow,
                        size,
                        align,
                    } => {
                        let kind = match kind {
                            AllocKind::Own { edge } => format!("for {}", field_ty(edge)),
                            AllocKind::Environment { ty } => format!("env t{}", ty.0),
                            AllocKind::Region => "region".into(),
                            AllocKind::Resource { grant } => format!("resource grant {grant}"),
                        };
                        let place = allocation::format_place(*place);
                        format!(
                            "{}, {}{} = alloc {kind} {} align {align} charge {}{place}{}",
                            def(*result),
                            def(*available),
                            region_entry.map_or_else(String::new, |v| format!(", {}", def(v))),
                            v(size),
                            v(charge),
                            overflow.map_or_else(String::new, |v| format!(" overflow v{}", v.0)),
                        )
                    }
                    Inst::Grow {
                        result,
                        available,
                        carrier,
                        charge,
                        overflow,
                        size,
                        align,
                        capacity,
                        moved,
                        refund,
                    } => format!(
                        "{}, {} = grow a{} {} align {align} charge {} capacity {} moved {} refund {}{}",
                        def(*result),
                        def(*available),
                        carrier.0,
                        v(size),
                        v(charge),
                        v(capacity),
                        v(moved),
                        v(refund),
                        overflow.map_or_else(String::new, |v| format!(" overflow v{}", v.0)),
                    ),
                    Inst::Free { block, charge } => {
                        format!("free {} charge {}", v(block), v(charge))
                    }
                    Inst::FrameAdmit {
                        storage,
                        admitted,
                        available,
                        charge,
                        overflow,
                        replace,
                    } => format!(
                        "{}, {} = frame_admit a{} charge {}{}{}",
                        def(*admitted),
                        def(*available),
                        storage.0,
                        v(charge),
                        overflow.map_or_else(String::new, |v| format!(" overflow v{}", v.0)),
                        if *replace { " replace" } else { "" },
                    ),
                    Inst::FrameActive { storage, result } => {
                        format!("{} = frame_active a{}", def(*result), storage.0)
                    }
                    Inst::FrameRefund { storage } => format!("frame_refund a{}", storage.0),
                    Inst::PromoteFrame { carrier } => format!("promote_frame a{}", carrier.0),
                    Inst::Copy { to, from, bytes } => {
                        format!("copy a{}, a{}, {}", to.0, from.0, v(bytes))
                    }
                    Inst::Move { to, from, bytes } => {
                        format!("move a{}, a{}, {}", to.0, from.0, v(bytes))
                    }
                    Inst::Prefetch { data, offset } => {
                        format!("prefetch {}, {}", v(data), v(offset))
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
                    Inst::BuiltinUse { definition } => format!("builtin_use {definition}"),
                    Inst::LoopIteration => "loop_iteration".into(),
                    Inst::Step { fail: f } => format!("step{}", fail(*f)),
                    Inst::StackCheck { fail: f } => format!("stack_check{}", fail(*f)),
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
                    Inst::Mark {
                        address,
                        count,
                        live,
                        ty,
                    }
                    | Inst::Unmark {
                        address,
                        count,
                        live,
                        ty,
                    } => {
                        format!(
                            "{} a{}, {}, {} : {}",
                            if matches!(inst, Inst::Mark { .. }) {
                                "mark"
                            } else {
                                "unmark"
                            },
                            address.0,
                            v(count),
                            if *live { "live" } else { "empty" },
                            place(*ty)
                        )
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
                        RaiseCause::Capacity {
                            requested,
                            available,
                        } => format!("capacity v{}, v{}", requested.0, available.0),
                    };
                    let cleanup = cleanup
                        .map(|c| format!(" cleanup b{}", c.0))
                        .unwrap_or_default();
                    format!("raise {cause}{cleanup}")
                }
                Terminator::Propagate => "propagate".into(),
                Terminator::Unreachable => "unreachable".into(),
            };
            let decision = if block.decision { " decision" } else { "" };
            self.line(&format!("  {term}{}{decision}", anchor(block.anchor)));
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
pub(crate) fn loan_word(loan: Loan) -> &'static str {
    match loan {
        Loan::Mut => "mut",
        Loan::View => "view",
        Loan::Take => "take",
    }
}

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
        if let Some((lanes, lane)) = t.strip_prefix('v').and_then(|t| t.split_once('x'))
            && let Some(shape) = lanes
                .parse()
                .ok()
                .zip(Scalar::from_name(lane))
                .and_then(|(lanes, lane)| Vector::new(lane, lanes))
        {
            return Ok(Ty::Vector(shape));
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
        let boxed = |l: &mut Self| l.field_ty().map(Box::new);
        Ok(match self.peek().ok_or("expected a field type")? {
            "box" | "rc" | "arc" | "shared" | "region_ref" | "link" => {
                let w = self.next()?;
                let t = self.tref()?;
                match w {
                    "box" => FieldTy::Box(t),
                    "rc" => FieldTy::Rc(t),
                    "arc" => FieldTy::Arc(t),
                    "shared" => FieldTy::Shared(t),
                    "region_ref" => FieldTy::RegionRef(t),
                    _ => FieldTy::Link(t),
                }
            }
            "view" => {
                self.next()?;
                FieldTy::View(boxed(self)?)
            }
            "locator" => {
                self.next()?;
                FieldTy::Locator(boxed(self)?)
            }
            "frozen_bytes" => {
                self.next()?;
                FieldTy::FrozenBytes
            }
            "seq" | "columns" | "fixed" | "seqspan" | "indexed" => {
                let w = self.next()?;
                let e = boxed(self)?;
                match w {
                    "seq" => FieldTy::Seq(e),
                    "columns" => FieldTy::Columns(e),
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
                    "text" => FieldTy::Text,
                    w => match (Scalar::from_name(w), w.strip_prefix('t').map(str::parse)) {
                        (Some(s), _) => FieldTy::Scalar(s),
                        (None, Some(Ok(n))) => FieldTy::Inline(hirsel_core_layout::TypeRef(n)),
                        _ => FieldTy::Handle(
                            HANDLE_NAMES
                                .iter()
                                .find_map(|&(name, kind)| (w == name).then_some(kind))
                                .ok_or_else(|| format!("unknown field type `{w}`"))?,
                        ),
                    },
                }
            }
        })
    }
    fn static_value(&mut self) -> R<StaticValue> {
        let case = if self.eat("case") {
            Some(self.num()?)
        } else {
            None
        };
        if case.is_some() || self.peek() == Some("[") {
            let fields = self.seq("[", "]", Self::static_value)?;
            Ok(StaticValue::Fields { case, fields })
        } else {
            Ok(StaticValue::Bits(self.num()?))
        }
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
            "region" => Repr::Region,
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
        let carried = if self.eat("value") {
            Carried::Value(self.ty()?)
        } else if self.eat("nothing") {
            Carried::Nothing
        } else {
            Carried::Address
        };
        let finalizer = if self.eat("drop") {
            Some(FuncRef(self.id('f')?))
        } else {
            None
        };
        let clone = if self.eat("clone") {
            Some(FuncRef(self.id('f')?))
        } else {
            None
        };
        let codec = if self.eat("codec") {
            Some(FuncRef(self.id('f')?))
        } else {
            None
        };
        let key_codec = if self.eat("key_codec") {
            Some(FuncRef(self.id('f')?))
        } else {
            None
        };
        let grant = if self.eat("grant") { self.num()? } else { 0 };
        let send = self.eat("send");
        let held = if self.eat("held") {
            Some(self.field_ty()?)
        } else {
            None
        };
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
            carried,
            body,
            finalizer,
            clone,
            codec,
            key_codec,
            grant,
            send,
            held,
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
pub fn parse_constant(t: Ty, s: &str) -> Result<u64, String> {
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
        let cold = l.eat("cold");
        self.body.edges.push(Edge { target, args, cold });
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
            Some("put_lanes") => {
                l.next()?;
                let to = AddressId(l.id('a')?);
                l.expect(",")?;
                let vector = l.value()?;
                l.expect(",")?;
                return Ok(Inst::PutLanes {
                    to,
                    vector,
                    count: l.value()?,
                });
            }
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
            Some("promote_frame") => {
                l.next()?;
                return Ok(Inst::PromoteFrame {
                    carrier: AddressId(l.id('a')?),
                });
            }
            Some("frame_refund") => {
                l.next()?;
                return Ok(Inst::FrameRefund {
                    storage: AddressId(l.id('a')?),
                });
            }
            Some(op @ ("copy" | "move")) => {
                let overlap = op == "move";
                l.next()?;
                let to = AddressId(l.id('a')?);
                l.expect(",")?;
                let from = AddressId(l.id('a')?);
                l.expect(",")?;
                let bytes = l.value()?;
                return Ok(if overlap {
                    Inst::Move { to, from, bytes }
                } else {
                    Inst::Copy { to, from, bytes }
                });
            }
            Some("prefetch") => {
                l.next()?;
                let data = l.value()?;
                l.expect(",")?;
                return Ok(Inst::Prefetch {
                    data,
                    offset: l.value()?,
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
            Some("step") => {
                l.next()?;
                return Ok(Inst::Step { fail: l.fail()? });
            }
            Some("stack_check") => {
                l.next()?;
                return Ok(Inst::StackCheck { fail: l.fail()? });
            }
            Some("loop_iteration") => {
                l.next()?;
                return Ok(Inst::LoopIteration);
            }
            Some("builtin_use") => {
                l.next()?;
                let definition = DefPath::from_full(l.next()?).ok_or("bad builtin definition")?;
                return Ok(Inst::BuiltinUse {
                    definition: Box::new(definition),
                });
            }
            Some("limit_exit") => {
                l.next()?;
                return Ok(Inst::LimitExit);
            }
            Some(word @ ("mark" | "unmark")) => {
                l.next()?;
                let address = AddressId(l.id('a')?);
                l.expect(",")?;
                let count = l.value()?;
                l.expect(",")?;
                let live = match l.next()? {
                    "live" => true,
                    "empty" => false,
                    other => return Err(format!("`{word}` takes live or empty, not `{other}`")),
                };
                l.expect(":")?;
                let ty = l.place()?;
                return Ok(if word == "mark" {
                    Inst::Mark {
                        address,
                        count,
                        live,
                        ty,
                    }
                } else {
                    Inst::Unmark {
                        address,
                        count,
                        live,
                        ty,
                    }
                });
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
        let inst = if op == "simd" {
            one(&defs)?;
            let name = l.next()?;
            let op = SimdOp::ALL
                .into_iter()
                .find(|op| op.name() == name)
                .ok_or_else(|| format!("unknown vector operation `{name}`"))?;
            let first = l.value()?;
            let second = if op.binary() {
                l.expect(",")?;
                Some(l.value()?)
            } else {
                None
            };
            Inst::Simd {
                result: defs[0],
                op,
                first,
                second,
                fail: l.fail()?,
            }
        } else if op == "alloc" {
            allocation::parse(l, &defs)?
        } else if op == "frame_admit" {
            let (admitted, available) = pair(&defs)?;
            let storage = AddressId(l.id('a')?);
            l.expect("charge")?;
            let charge = l.value()?;
            let overflow = if l.eat("overflow") {
                Some(l.value()?)
            } else {
                None
            };
            Inst::FrameAdmit {
                storage,
                admitted,
                available,
                charge,
                overflow,
                replace: l.eat("replace"),
            }
        } else if op == "frame_active" {
            let [result] = defs.as_slice() else {
                return Err("frame_active takes one result".into());
            };
            Inst::FrameActive {
                storage: AddressId(l.id('a')?),
                result: *result,
            }
        } else if op == "grow" {
            let (result, available) = pair(&defs)?;
            let carrier = AddressId(l.id('a')?);
            let size = l.value()?;
            l.expect("align")?;
            let align = l.num()?;
            l.expect("charge")?;
            let charge = l.value()?;
            l.expect("capacity")?;
            let capacity = l.value()?;
            l.expect("moved")?;
            let moved = l.value()?;
            l.expect("refund")?;
            let refund = l.value()?;
            let overflow = if l.eat("overflow") {
                Some(l.value()?)
            } else {
                None
            };
            Inst::Grow {
                result,
                available,
                carrier,
                charge,
                overflow,
                size,
                align,
                capacity,
                moved,
                refund,
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
        } else if matches!(
            op,
            "field_size" | "held_size" | "block_size" | "frame_capacity" | "target"
        ) {
            one(&defs)?;
            let query = match op {
                "field_size" => LayoutQuery::FieldSize(l.field_ty()?),
                "frame_capacity" => LayoutQuery::FrameCapacity(l.field_ty()?),
                "held_size" => LayoutQuery::HeldSize(TypeId(l.id('t')?)),
                "target" => LayoutQuery::Target,
                _ => LayoutQuery::BlockSize {
                    ty: TypeId(l.id('t')?),
                    prefix: l.num()?,
                },
            };
            Inst::LayoutConst { result, query }
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
        } else if op == "descriptor" {
            one(&defs)?;
            descriptor_text::parse(l, result)?
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
        } else if op == "width" {
            one(&defs)?;
            let word = l.value()?;
            Inst::CompactWidth {
                result,
                word,
                ty: TypeId(l.id('t')?),
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
        } else if let Some(v) = t.strip_prefix("export.v") {
            Callee::PackExport(ValueId(v.parse().map_err(|_| format!("bad callee `{t}`"))?))
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
                "receive_rec" => RtOp::ReceiveRec { item: tys(1)?[0] },
                "receive_until" => RtOp::ReceiveUntil { item: tys(1)?[0] },
                "try_receive" => RtOp::TryReceive { item: tys(1)?[0] },
                "latest_new" => RtOp::LatestNew { item: tys(1)?[0] },
                "latest_handle" => RtOp::LatestHandle,
                "conn_new" => RtOp::ConnNew { item: tys(1)?[0] },
                "conn_client" => RtOp::ConnClient,
                "conn_send" => RtOp::ConnSend { item: tys(1)?[0] },
                "conn_try_send" => RtOp::ConnTrySend { item: tys(1)?[0] },
                "conn_receive" => RtOp::ConnReceive { item: tys(1)?[0] },
                "conn_try_receive" => RtOp::ConnTryReceive { item: tys(1)?[0] },
                "spawn_unit" => {
                    let ts = tys(2)?;
                    RtOp::SpawnUnit { x: ts[0], r: ts[1] }
                }
                "transfer" => {
                    let ts = tys(2)?;
                    RtOp::Transfer { x: ts[0], r: ts[1] }
                }
                "latest_publish" => RtOp::LatestPublish,
                "latest_read" => RtOp::LatestRead,
                "shared_new" => RtOp::SharedNew { item: tys(1)?[0] },
                "shared_share" => RtOp::SharedShare,
                "shared_release" => RtOp::SharedRelease { item: tys(1)?[0] },
                "dest_id" => RtOp::DestId,
                "compare_bytes" => RtOp::CompareBytes,
                "map_hash_bytes" => RtOp::MapHashBytes,
                "map_hash_word" => RtOp::MapHashWord,
                "map_hash_start" => RtOp::MapHashStart,
                "map_hash_add_word" => RtOp::MapHashAddWord,
                "map_hash_add_bytes" => RtOp::MapHashAddBytes,
                "map_hash_finish" => RtOp::MapHashFinish,
                "map_place_word" => RtOp::MapPlaceWord,
                "map_place_bytes" => RtOp::MapPlaceBytes,
                "map_strong_word" => RtOp::MapStrongWord,
                "map_strong_bytes" => RtOp::MapStrongBytes,
                "map_strong_start" => RtOp::MapStrongStart,
                "xxh3_short" => RtOp::Xxh3Short,
                "xxh3_stripes" => RtOp::Xxh3Stripes,
                name if let Some(op) = runtime::sha(name) => op,
                "zero_bytes" => RtOp::ZeroBytes,
                "glue_mark" => RtOp::GlueMark,
                "glue_push" => RtOp::GluePush,
                "glue_pop" => RtOp::GluePop,
                "glue_cut" => RtOp::GlueCut,
                "crc32c" => RtOp::Crc32c,
                "crc64nvme" => RtOp::Crc64Nvme,
                "crc32c_combine" => RtOp::Crc32cCombine,
                "crc64nvme_combine" => RtOp::Crc64NvmeCombine,
                "ed25519_verify" => RtOp::Ed25519Verify,
                "p256_verify" => RtOp::P256Verify,
                "p256_verify_der" => RtOp::P256VerifyDer,
                "p384_verify" => RtOp::P384Verify,
                "p384_verify_der" => RtOp::P384VerifyDer,
                "rsa_verify_pkcs1_sha256" => RtOp::RsaVerifyPkcs1Sha256,
                "rsa_verify_pss_sha256" => RtOp::RsaVerifyPssSha256,
                "argon2id" => RtOp::Argon2id,
                "cause_next" => RtOp::CauseNext,
                "cause_value" => RtOp::CauseValue,
                "args_len" => RtOp::ArgsLen,
                "args_get" => RtOp::ArgsGet,
                "root_limit" => RtOp::RootLimit,
                "launch_word" => RtOp::LaunchWord,
                "launch_handle" => RtOp::LaunchHandle,
                "secret_share" => RtOp::SecretShare,
                "describe_emit" => RtOp::DescribeEmit,
                "launch_target" => RtOp::LaunchTarget { view: tys(1)?[0] },
                "isolate_enter" => RtOp::IsolateEnter,
                "isolate_leave" => RtOp::IsolateLeave,
                "isolate_catch" => RtOp::IsolateCatch,
                "step_enter" => RtOp::StepEnter,
                "step_leave" => RtOp::StepLeave,
                "step_catch" => RtOp::StepCatch,
                "work_steps" => RtOp::WorkSteps,
                name if let Some(op) = runtime::parse_pack(name, &mut tys)? => op,
                name if let Some(op) = hirsel_core_layout::flow::Op::ALL
                    .iter()
                    .copied()
                    .find(|op| op.name() == name) =>
                {
                    RtOp::Flow(op)
                }
                "frame_record" => RtOp::FrameRecord,
                name if let Some(i) = name
                    .strip_prefix("closed_")
                    .and_then(|i| i.parse::<u8>().ok())
                    .filter(|&i| usize::from(i) < crate::CLOSED.len()) =>
                {
                    RtOp::Closed(i)
                }
                "join_rec" => RtOp::Returnable(Wait::Join { r: tys(1)?[0] }),
                "conn_send_rec" => RtOp::Returnable(Wait::ConnSend { item: tys(1)?[0] }),
                "conn_receive_rec" => RtOp::Returnable(Wait::ConnReceive { item: tys(1)?[0] }),
                name if let Some(op) = name
                    .strip_suffix("_rec")
                    .and_then(|n| HostOp::ALL.iter().copied().find(|op| op.name() == n)) =>
                {
                    RtOp::Returnable(Wait::Host(op))
                }
                _ => match HostOp::ALL.iter().copied().find(|op| op.name() == name) {
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
            cold: l.eat("cold"),
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
                    "capacity" => {
                        let requested = l.value()?;
                        l.expect(",")?;
                        RaiseCause::Capacity {
                            requested,
                            available: l.value()?,
                        }
                    }
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
        {
            let mut l = self.line().ok_or("empty input")?;
            l.expect("core")?;
            l.expect("v1")?;
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
                    self.module.types.push(Row::new(s));
                }
                "static" => {
                    let n = index(self.module.statics.len());
                    let mut l = self.line().unwrap();
                    l.next()?;
                    expect_index(&mut l, 'd', n)?;
                    if l.eat("aggregate") {
                        let ty = TypeId(l.id('t')?);
                        let value = l.static_value()?;
                        l.end()?;
                        self.module.statics.push(Static::Aggregate { ty, value });
                        continue;
                    }
                    if l.eat("computed") {
                        let ty = l.field_ty()?;
                        let t = l.next()?;
                        let constant = DefPath::from_full(t)
                            .ok_or_else(|| format!("bad definition path `{t}`"))?;
                        l.end()?;
                        self.module.statics.push(Static::Computed { ty, constant });
                        continue;
                    }
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
                    self.module.statics.push(Static::Bytes { align, bytes });
                }
                "sig" => {
                    let n = index(self.module.signatures.len());
                    let mut l = self.line().unwrap();
                    l.next()?;
                    expect_index(&mut l, 'g', n)?;
                    let params = l.seq("(", ")", |l| {
                        if l.eat("&") {
                            let loan = if l.eat("mut") {
                                Loan::Mut
                            } else if l.eat("view") {
                                Loan::View
                            } else if l.eat("take") {
                                Loan::Take
                            } else {
                                return Err("expected `mut`, `view` or `take` after `&`".into());
                            };
                            Ok(Param::Address {
                                place: l.place()?,
                                loan,
                            })
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
                    let (dest, anchors) = if l.eat("dest") {
                        let view = l.eat("view");
                        let dest = Some(TypeId(l.id('t')?));
                        let anchors = if view {
                            l.expect("in")?;
                            l.seq("(", ")", |l| {
                                let k = l.next()?;
                                k.parse().map_err(|_| format!("bad anchor `{k}`"))
                            })?
                        } else {
                            vec![]
                        };
                        (dest, anchors)
                    } else {
                        (None, vec![])
                    };
                    l.end()?;
                    self.module.signatures.push(Signature {
                        params,
                        results,
                        dest,
                        anchors,
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
                    let arguments = l.seq("[", "]", |l| l.id('t').map(TypeId))?;
                    let dictionaries = l.seq("[", "]", Line::string)?;
                    l.end()?;
                    self.module.instances.push(InstanceKey {
                        definition,
                        arguments,
                        dictionaries,
                    });
                }
                "fn" => self.function()?,
                w => return Err(format!("unexpected `{w}`")),
            }
        }
        Ok(())
    }

    fn function(&mut self) -> R<()> {
        let n = self.module.functions.len() as u32;
        let (instance, signature, code, versioned, cold, pack_export, external) = {
            let mut l = self.line().unwrap();
            l.next()?;
            expect_index(&mut l, 'f', n)?;
            let instance = InstanceId(l.id('n')?);
            let signature = SigId(l.id('g')?);
            let code = if l.eat("code") {
                let text = l.next()?;
                if text.len() != 64 || !text.is_ascii() {
                    return Err("bad code identity".into());
                }
                let mut bytes = [0u8; 32];
                for (i, byte) in bytes.iter_mut().enumerate() {
                    *byte = u8::from_str_radix(&text[2 * i..2 * i + 2], 16)
                        .map_err(|_| "bad code identity")?;
                }
                Some(hirsel_core_ids::ContentHash(bytes))
            } else {
                None
            };
            let versioned = l.eat("versioned");
            let cold = l.eat("cold");
            let pack_export = l.eat("pack_export");
            let external = l.eat("extern");
            if !external {
                l.expect("{")?;
            }
            l.end()?;
            (
                instance,
                signature,
                code,
                versioned,
                cold,
                pack_export,
                external,
            )
        };
        let body = if external { None } else { Some(self.body()?) };
        if let Some(hash) = code {
            self.module.code_variants.insert(FuncId(n), hash);
        }
        self.module.functions.push(Function {
            instance,
            signature,
            body,
            versioned,
            cold,
            pack_export,
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
                Some("callable") if b.body.blocks.is_empty() => {
                    l.next()?;
                    b.body.callable_parameters.push(l.value()?);
                    l.end()?;
                }
                Some("root") if b.body.blocks.is_empty() => {
                    l.next()?;
                    let ty = TypeId(l.id('t')?);
                    let width: u8 = l.num()?;
                    l.end()?;
                    b.body.compact_roots.push(CompactRoot { ty, width });
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
                            "seq" => {
                                let w = l.next()?;
                                Step::Seq(
                                    SeqWord::from_name(w)
                                        .ok_or_else(|| format!("unknown seq word `{w}`"))?,
                                )
                            }
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
                        decision: false,
                    });
                    open = Some(b.body.blocks.len() - 1);
                }
                _ => {
                    let Some(block) = open else {
                        return Err("instruction outside a block".into());
                    };
                    if let Some(t) = b.terminator(&mut l)? {
                        let anchor = l.anchor()?;
                        let decision = anchor.is_some() && l.eat("decision");
                        l.end()?;
                        b.body.blocks[block].anchor = anchor;
                        b.body.blocks[block].decision = decision;
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
