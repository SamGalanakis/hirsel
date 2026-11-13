//! Direct emission of CoreIR functions, failure edges and storage recipes. No old lowered program,
//! staging, manifest or receipt participates in this entry point.

/// Frozen native memory layout, shared with hirsel-core-rt.
pub mod memory_layout {
    include!("../../hirsel-core-rt/src/abi_layout.rs");
}

mod abi;
mod bytes;
mod cleanup;
mod collections;
mod empty;
mod failure;
mod fast;
mod scalar;
mod storage;
mod tasks;
#[cfg(test)]
mod tests;

use std::collections::BTreeSet;
use std::fmt::{self, Write};

use hirsel_core_ir::*;

use crate::context::{Context, Module as LlvmModule, TargetMachine};

/// Optimisation of the generated object. All levels preserve checked arithmetic.
#[derive(Clone, Copy, Debug, Default)]
pub enum Optimization {
    #[default]
    None,
    Speed,
}

/// An operation whose native contract has not been implemented yet, or a tool error.
#[derive(Debug)]
pub enum Error {
    Unsupported(String),
    Llvm(String),
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported(s) => write!(f, "CoreIR LLVM emission does not yet implement {s}"),
            Self::Llvm(s) => f.write_str(s),
        }
    }
}
impl std::error::Error for Error {}

type Result<T> = std::result::Result<T, Error>;

/// The FORM instance link name, shared by declarations, definitions and callers.
pub fn symbol(module: &Module, function: FuncId) -> String {
    let mut name = String::from("h");
    for byte in module.instances[module.functions[function.index()].instance.index()]
        .digest()
        .0
    {
        write!(name, "{byte:02x}").unwrap();
    }
    name
}

/// Emit x86-64 System V LLVM IR directly from CoreIR.
///
/// Functions take FORM v1a's hidden task context before the destination.
/// Exports and function pointers return scalar results followed by failure status.
/// Proven infallible bodies use a private direct-call ABI without that status.
/// This function trusts its producer's IR and layout recipes.
pub fn emit_ir(module: &Module) -> Result<String> {
    let mut text = String::new();
    for (i, data) in module.statics.iter().enumerate() {
        let bytes = data
            .bytes
            .iter()
            .map(|b| format!("\\{b:02X}"))
            .collect::<String>();
        writeln!(
            text,
            "@s{i} = private constant [{} x i8] c\"{bytes}\", align {}",
            data.bytes.len(),
            data.align
        )
        .unwrap();
    }
    let recipes = hirsel_core_layout::compute(&module.types).expect("producer layouts");
    let mut intrinsics = BTreeSet::new();
    let failures = failure::Failures::new(module);
    for (i, f) in module.functions.iter().enumerate() {
        let sig = &module.signatures[f.signature.index()];
        let has_status = failures.may_fail[i];
        let abi = if has_status || f.body.is_none() {
            abi::Return::internal(&sig.results)
        } else {
            abi::Return::new(&sig.results)
        };
        let name = symbol(module, FuncId(i as u32));
        let params = abi.parameters(sig);
        let Some(body) = &f.body else {
            writeln!(
                text,
                "declare {} @{name}({})",
                abi.return_type(),
                params.join(", ")
            )
            .unwrap();
            continue;
        };
        let mut e = FunctionEmitter {
            module,
            body,
            recipes: &recipes,
            abi,
            has_status,
            failures: &failures,
            constants: &failures.constants[i],
            empty_bounds: empty::EmptyBounds::new(body, &failures.constants[i]),
            code: String::new(),
            allocations: String::new(),
            next: 0,
            intrinsics: &mut intrinsics,
        };
        for (i, t) in body.values.iter().enumerate() {
            writeln!(e.allocations, "  %v{i} = alloca {}", ty(*t)).unwrap();
        }
        for (i, t) in body.slots.iter().enumerate() {
            let (bytes, align) = e.place_layout(*t);
            writeln!(
                e.allocations,
                "  %s{i} = alloca [{bytes} x i8], align {align}"
            )
            .unwrap();
        }
        for i in 0..body.addresses.len() {
            writeln!(e.allocations, "  %a{i} = alloca ptr").unwrap();
        }
        for (i, v) in body.blocks[0]
            .params
            .get(&body.pools.values)
            .unwrap()
            .iter()
            .enumerate()
        {
            let parameter = if sig.dest.is_some() && i == 0 {
                "%dest".into()
            } else {
                format!("%p{}", i - usize::from(sig.dest.is_some()))
            };
            e.put(*v, &parameter);
        }
        e.line("br label %b0");
        for (i, b) in body.blocks.iter().enumerate() {
            writeln!(e.code, "b{i}:").unwrap();
            for (offset, inst) in b.insts.get(&body.insts).unwrap().iter().enumerate() {
                e.inst(inst, b.insts.start as usize + offset)?;
            }
            e.term(&body.terminators[i])?;
        }
        let linkage = if has_status { "" } else { "internal " };
        let body_name = if has_status {
            name.clone()
        } else {
            format!("{name}.infallible")
        };
        writeln!(
            text,
            "define {linkage}{} @{body_name}({}) {{\nentry:\n{}{} }}",
            e.abi.return_type(),
            params.join(", "),
            e.allocations,
            e.code
        )
        .unwrap();
        if !has_status {
            e.infallible_wrapper(&name, sig, &mut text);
        }
    }
    cleanup::drop_glue(module, &recipes, &failures, &mut text, &mut intrinsics)?;
    for decl in intrinsics {
        writeln!(text, "{decl}").unwrap();
    }
    Ok(text)
}

/// Compile CoreIR to an owned ELF object for x86-64 Linux. This uses the
/// retained LLVM target/handle machinery, without the old product pipeline.
/// The caller supplies its linker and entry harness; objects have FORM symbols.
pub fn emit_object(module: &Module, opt: Optimization) -> Result<Vec<u8>> {
    compile(module, opt, |_| (), |_| ()).map(|(bytes, ())| bytes)
}

/// Exclusive compilation stages, for the developer measurement entry.
#[derive(Clone, Copy, Debug)]
pub enum CompilationStage {
    TextConstruction,
    TargetSetup,
    IrParse,
    Optimization,
    ObjectEmission,
    Disposal,
}

/// Observe stage boundaries without changing the generated program.
pub fn emit_object_observed(
    module: &Module,
    opt: Optimization,
    observer: impl FnMut(CompilationStage),
) -> Result<Vec<u8>> {
    compile(module, opt, |_| (), observer).map(|(bytes, ())| bytes)
}

#[cfg(test)]
pub(crate) fn emitted_ir(module: &Module, opt: Optimization) -> Result<String> {
    compile(module, opt, |llvm| llvm.text(), |_| ()).map(|(_, ir)| ir)
}

fn compile<R>(
    module: &Module,
    opt: Optimization,
    inspect: impl FnOnce(&LlvmModule<'_>) -> R,
    mut observer: impl FnMut(CompilationStage),
) -> Result<(Vec<u8>, R)> {
    observer(CompilationStage::TextConstruction);
    let ir = emit_ir(module)?;
    observer(CompilationStage::TargetSetup);
    let target = hirsel_lir::layout_types::TargetSpec::x86_64_unknown_linux_gnu();
    let machine =
        TargetMachine::new(&target, matches!(opt, Optimization::Speed)).map_err(Error::Llvm)?;
    observer(CompilationStage::IrParse);
    let context = Context::new();
    let module = LlvmModule::parse(&context, "hirsel-core", &ir).map_err(Error::Llvm)?;
    module.set_target(&machine);
    observer(CompilationStage::Optimization);
    if matches!(opt, Optimization::Speed) {
        module.run_core_passes(&machine).map_err(Error::Llvm)?;
    }
    observer(CompilationStage::ObjectEmission);
    let bytes = module.emit_object(&machine).map_err(Error::Llvm)?;
    let result = inspect(&module);
    observer(CompilationStage::Disposal);
    Ok((bytes, result))
}

fn ty(t: Ty) -> String {
    match t {
        Ty::Ptr => "ptr".into(),
        Ty::Scalar(Scalar::F32) => "float".into(),
        Ty::Scalar(Scalar::F64) => "double".into(),
        Ty::Scalar(s) => format!("i{}", s.size() * 8),
    }
}
fn bits_ty(t: Ty) -> String {
    format!("i{}", size(t) * 8)
}
fn size(t: Ty) -> u64 {
    match t {
        Ty::Ptr => 8,
        Ty::Scalar(s) => s.size(),
    }
}

struct FunctionEmitter<'a> {
    module: &'a Module,
    body: &'a Body,
    recipes: &'a [hirsel_core_layout::Recipe],
    abi: abi::Return,
    has_status: bool,
    failures: &'a failure::Failures,
    constants: &'a [Option<u64>],
    empty_bounds: empty::EmptyBounds,
    code: String,
    allocations: String,
    next: usize,
    intrinsics: &'a mut BTreeSet<String>,
}
impl FunctionEmitter<'_> {
    fn line(&mut self, s: impl AsRef<str>) {
        writeln!(self.code, "  {}", s.as_ref()).unwrap();
    }
    fn fresh(&mut self) -> String {
        let n = self.next;
        self.next += 1;
        format!("%t{n}")
    }
    fn calc(&mut self, s: impl AsRef<str>) -> String {
        let v = self.fresh();
        self.line(format!("{v} = {}", s.as_ref()));
        v
    }
    fn temp(&mut self, t: &str) -> String {
        let p = self.fresh();
        writeln!(self.allocations, "  {p} = alloca {t}, align 8").unwrap();
        p
    }
    fn vty(&self, v: ValueId) -> Ty {
        self.body.values[v.index()]
    }
    fn get(&mut self, v: ValueId) -> String {
        let t = self.vty(v);
        let x = self.calc(format!("load {}, ptr %v{}", ty(t), v.0));
        self.canonical(t, &x)
    }
    fn put(&mut self, v: ValueId, x: &str) {
        self.line(format!("store {} {x}, ptr %v{}", ty(self.vty(v)), v.0));
    }
    fn address(&mut self, a: AddressId) -> String {
        self.calc(format!("load ptr, ptr %a{}", a.0))
    }
    fn intrinsic(&mut self, name: &str, ret: &str, args: &[(String, String)]) -> String {
        self.intrinsics.insert(format!(
            "declare {ret} @{name}({})",
            args.iter()
                .map(|(t, _)| t.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
        self.calc(format!(
            "call {ret} @{name}({})",
            args.iter()
                .map(|(t, v)| format!("{t} {v}"))
                .collect::<Vec<_>>()
                .join(", ")
        ))
    }
    fn canonical(&mut self, t: Ty, x: &str) -> String {
        if !t.is_float() {
            return x.into();
        }
        let nan = self.calc(format!("fcmp uno {} {x}, {x}", ty(t)));
        let bits = if size(t) == 4 {
            "2143289344"
        } else {
            "9221120237041090560"
        };
        self.calc(format!(
            "select i1 {nan}, {} bitcast ({} {bits} to {}), {} {x}",
            ty(t),
            bits_ty(t),
            ty(t),
            ty(t)
        ))
    }
    fn fail_if(&mut self, flag: &str, fail: Option<BlockId>, cause: u8) -> Result<()> {
        self.branch_failure(flag, fail, Some(cause));
        Ok(())
    }
    fn branch_failure(&mut self, flag: &str, fail: Option<BlockId>, cause: Option<u8>) {
        let n = self.next;
        self.next += 1;
        self.line(format!("br i1 {flag}, label %bad{n}, label %ok{n}"));
        writeln!(self.code, "bad{n}:").unwrap();
        if let Some(cause) = cause {
            self.raise(cause);
        }
        self.propagate_to(fail);
        writeln!(self.code, "ok{n}:").unwrap();
    }
    fn propagate_to(&mut self, fail: Option<BlockId>) {
        if let Some(fail) = fail {
            self.line(format!("br label %b{}", fail.0));
        } else {
            self.return_failure();
        }
    }
    fn raise(&mut self, kind: u8) {
        let shape = format!("[{} x i8]", memory_layout::CAUSE_BYTES);
        self.intrinsics.insert(format!(
            "declare void @hirsel_core_raise(ptr, ptr byval({shape}) align 8)"
        ));
        let cause = self.temp(&shape);
        self.line(format!(
            "store {shape} zeroinitializer, ptr {cause}, align 8"
        ));
        self.line(format!("store i8 {kind}, ptr {cause}, align 8"));
        self.line(format!(
            "call void @hirsel_core_raise(ptr %context, ptr byval({shape}) align 8 {cause})"
        ));
    }
    fn inst(&mut self, inst: &Inst, index: usize) -> Result<()> {
        match *inst {
            Inst::Const { result, bits } => {
                let t = self.vty(result);
                let x = if t.is_float() {
                    format!("bitcast ({} {bits} to {})", bits_ty(t), ty(t))
                } else if t == Ty::Ptr {
                    format!("inttoptr (i64 {bits} to ptr)")
                } else {
                    bits.to_string()
                };
                self.put(result, &x);
            }
            Inst::ReadStatic { result, data } => self.put(result, &format!("@s{}", data.0)),
            Inst::FunctionRef { result, function } => {
                self.put(result, &format!("@{}", symbol(self.module, function)))
            }
            Inst::Unary {
                result,
                op,
                operand,
                fail,
            } => {
                let t = self.vty(operand);
                let x = self.get(operand);
                let r = match op {
                    UnaryOp::Not => self.calc(format!("xor {} {x}, 1", ty(t))),
                    UnaryOp::BitNot => self.calc(format!("xor {} {x}, -1", ty(t))),
                    UnaryOp::Neg if t.is_float() => self.calc(format!("fneg {} {x}", ty(t))),
                    UnaryOp::Neg => {
                        let (v, flag) = self.overflow(t, ArithOp::Sub, "0", &x);
                        self.fail_if(&flag, fail, 0)?;
                        v
                    }
                };
                self.put(result, &r);
            }
            Inst::Binary {
                result,
                op,
                left,
                right,
                fail,
            } => {
                let t = self.vty(left);
                let a = self.get(left);
                let x = self.binary(t, op, &a, right, fail)?;
                self.put(result, &x);
            }
            Inst::Compare {
                result,
                op,
                left,
                right,
            } => {
                let t = self.vty(left);
                let a = self.get(left);
                let b = self.get(right);
                let pred = match op {
                    CompareOp::Eq => "eq",
                    CompareOp::Ne => "ne",
                    CompareOp::Lt => "lt",
                    CompareOp::Le => "le",
                    CompareOp::Gt => "gt",
                    CompareOp::Ge => "ge",
                };
                let cmp = if t.is_float() {
                    format!(
                        "fcmp {}{pred} {} {a}, {b}",
                        if op == CompareOp::Ne { "u" } else { "o" },
                        ty(t)
                    )
                } else {
                    format!(
                        "icmp {}{pred} {} {a}, {b}",
                        if matches!(op, CompareOp::Eq | CompareOp::Ne) {
                            ""
                        } else if t.int().is_some_and(|x| x.0) {
                            "s"
                        } else {
                            "u"
                        },
                        ty(t)
                    )
                };
                let x = self.calc(cmp);
                let x = self.calc(format!("zext i1 {x} to i8"));
                self.put(result, &x);
            }
            Inst::IntArith {
                result,
                flag,
                op,
                mode,
                left,
                right,
            } => {
                let t = self.vty(left);
                let a = self.get(left);
                let b = self.get(right);
                let (r, over) = self.arith(t, op, mode, &a, &b);
                self.put(result, &r);
                if let Some(flag) = flag {
                    let over = self.calc(format!("zext i1 {over} to i8"));
                    self.put(flag, &over);
                }
            }
            Inst::Convert {
                result,
                flag,
                operand,
                mode,
            } => self.convert(result, flag, operand, mode),
            Inst::ScalarUnary {
                result,
                op,
                operand,
            } => {
                let t = self.vty(operand);
                let x = self.get(operand);
                let x = self.scalar_unary(t, op, &x);
                self.put(result, &x);
            }
            Inst::MulAdd {
                result,
                left,
                right,
                addend,
            } => {
                let t = self.vty(left);
                let a = self.get(left);
                let b = self.get(right);
                let c = self.get(addend);
                let x = self.intrinsic(
                    &format!("llvm.fma.{}", if size(t) == 4 { "f32" } else { "f64" }),
                    &ty(t),
                    &[(ty(t), a), (ty(t), b), (ty(t), c)],
                );
                self.put(result, &x);
            }
            Inst::Address { address, fail } => self.project(address, fail)?,
            Inst::Read {
                result, address, ..
            } => {
                let p = self.address(address);
                let x = self.calc(format!("load {}, ptr {p}, align 1", ty(self.vty(result))));
                self.put(result, &x);
            }
            Inst::Store { address, value } => {
                let p = self.address(address);
                let x = self.get(value);
                self.line(format!(
                    "store {} {x}, ptr {p}, align 1",
                    ty(self.vty(value))
                ));
            }
            Inst::StorageLive { .. } | Inst::StorageDead { .. } => {}
            Inst::UpdateMask {
                result,
                mask,
                field,
                initialized,
            } => {
                let t = ty(self.vty(mask));
                let x = self.get(mask);
                let k = if initialized {
                    1u64 << field
                } else {
                    !(1u64 << field)
                };
                let x = self.calc(format!(
                    "{} {t} {x}, {k}",
                    if initialized { "or" } else { "and" }
                ));
                self.put(result, &x);
            }
            Inst::Call { call } => self.call(&self.body.calls[call.index()])?,
            _ => self.storage(inst, index)?,
        }
        Ok(())
    }
    fn edge(&mut self, edge: EdgeId) -> String {
        let e = &self.body.edges[edge.index()];
        let args = e.args.get(&self.body.pools.values).unwrap();
        // Read every source before writing any target: loop parameters can swap.
        let values = args.iter().map(|v| self.get(*v)).collect::<Vec<_>>();
        for (v, x) in self.body.blocks[e.target.index()]
            .params
            .get(&self.body.pools.values)
            .unwrap()
            .iter()
            .zip(values)
        {
            self.put(*v, &x);
        }
        format!("b{}", e.target.0)
    }
    fn edge_label(&mut self, edge: EdgeId) -> String {
        let n = self.next;
        self.next += 1;
        // Each edge gets its own block, including distinct switch arms to the same target.
        format!("e{n}_{}", edge.0)
    }
    fn emit_edge(&mut self, label: &str, edge: EdgeId) {
        writeln!(self.code, "{label}:").unwrap();
        let to = self.edge(edge);
        self.line(format!("br label %{to}"));
    }
    fn term(&mut self, term: &Terminator) -> Result<()> {
        match *term {
            Terminator::Jump(e) => {
                let to = self.edge(e);
                self.line(format!("br label %{to}"));
            }
            Terminator::Branch { condition, yes, no } => {
                let c = self.get(condition);
                let c = self.calc(format!("icmp ne i8 {c}, 0"));
                let y = self.edge_label(yes);
                let n = self.edge_label(no);
                self.line(format!("br i1 {c}, label %{y}, label %{n}"));
                self.emit_edge(&y, yes);
                self.emit_edge(&n, no);
            }
            Terminator::Switch {
                value,
                arms,
                otherwise,
            } => {
                let v = self.get(value);
                let t = ty(self.vty(value));
                let other = self.edge_label(otherwise);
                let arms = arms
                    .get(&self.body.pools.arms)
                    .unwrap()
                    .iter()
                    .map(|a| (*a, self.edge_label(a.edge)))
                    .collect::<Vec<_>>();
                self.line(format!("switch {t} {v}, label %{other} ["));
                for (a, label) in &arms {
                    self.line(format!("{t} {}, label %{label}", a.value));
                }
                self.line("]");
                self.emit_edge(&other, otherwise);
                for (a, label) in arms {
                    self.emit_edge(&label, a.edge);
                }
            }
            Terminator::Return(values) => {
                let xs = values
                    .get(&self.body.pools.values)
                    .unwrap()
                    .iter()
                    .map(|v| self.get(*v))
                    .collect::<Vec<_>>();
                self.return_values(&xs);
            }
            Terminator::Unreachable => self.line("unreachable"),
            Terminator::Propagate => self.return_failure(),
            Terminator::Raise { cause, cleanup } => {
                match cause {
                    RaiseCause::Unwrap => self.raise(3),
                    RaiseCause::Bounds => self.raise(1),
                    RaiseCause::Fail(address) => {
                        let at = self.address(address);
                        let data = self.calc(format!("load ptr, ptr {at}, align 1"));
                        let length = self.calc(format!("getelementptr i8, ptr {at}, i64 8"));
                        let length = self.calc(format!("load i64, ptr {length}, align 1"));
                        let capacity = self.calc(format!("getelementptr i8, ptr {at}, i64 16"));
                        let capacity = self.calc(format!("load i64, ptr {capacity}, align 1"));
                        self.intrinsics.insert(
                            "declare void @hirsel_core_raise_string(ptr, i64, ptr, i64, i64)"
                                .into(),
                        );
                        self.line(format!("call void @hirsel_core_raise_string(ptr %context, i64 0, ptr {data}, i64 {length}, i64 {capacity})"));
                    }
                }
                self.propagate_to(cleanup);
            }
        }
        Ok(())
    }
}
