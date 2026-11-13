//! Typed cleanup reads producer recipes. Counted cleanup yields one descendant
//! at a time to the runtime's intrusive worklist, so pointer depth uses no stack.
use super::*;
use cranelift_codegen::ir::{MemFlagsData, Value, condcodes::IntCC};
use cranelift_frontend::Variable;
use hirsel_core_layout::{FieldTy, Layout, Recipe, Repr, TypeRef, charge, field_size};
use std::collections::BTreeSet;
mod resume;
use resume::plan_type;

pub(super) struct Glues {
    pub drops: Vec<Option<cranelift_module::FuncId>>,
    pub resumes: Vec<Option<cranelift_module::FuncId>>,
}
const P: Ty = Ty::Ptr;
const U: Ty = Ty::Scalar(Scalar::U64);
const B: Ty = Ty::Scalar(Scalar::Bool);

impl Glues {
    pub fn declare(
        object: &mut ObjectModule,
        input: &c::Module,
        layouts: &[Recipe],
    ) -> Result<Self> {
        let mut roots = BTreeSet::new();
        fn counted(ty: &FieldTy, roots: &mut BTreeSet<usize>) {
            match ty {
                FieldTy::Rc(t) | FieldTy::Arc(t) => {
                    roots.insert(t.index());
                }
                FieldTy::Array(e, _) | FieldTy::Seq(e) => counted(e, roots),
                FieldTy::Map(k, v) => {
                    counted(k, roots);
                    counted(v, roots);
                }
                _ => {}
            }
        }
        let mut needed = BTreeSet::new();
        for body in input.functions.iter().filter_map(|f| f.body.as_ref()) {
            for inst in &body.insts {
                match inst {
                    Inst::Drop { ty, .. } => {
                        needed.insert(ty.index());
                    }
                    Inst::Release { ty, .. } => {
                        roots.insert(ty.index());
                    }
                    _ => {}
                }
            }
        }
        fn normal(
            t: usize,
            layouts: &[Recipe],
            needed: &mut BTreeSet<usize>,
            roots: &mut BTreeSet<usize>,
            seen: &mut BTreeSet<usize>,
        ) {
            if !seen.insert(t) {
                return;
            }
            fn field(
                f: &FieldTy,
                layouts: &[Recipe],
                needed: &mut BTreeSet<usize>,
                roots: &mut BTreeSet<usize>,
                seen: &mut BTreeSet<usize>,
            ) {
                match f {
                    FieldTy::Inline(t) | FieldTy::Box(t) => {
                        needed.insert(t.index());
                        normal(t.index(), layouts, needed, roots, seen);
                    }
                    FieldTy::Array(e, _) | FieldTy::Seq(e) => {
                        field(e, layouts, needed, roots, seen)
                    }
                    FieldTy::Map(k, v) => {
                        field(k, layouts, needed, roots, seen);
                        field(v, layouts, needed, roots, seen);
                    }
                    _ => counted(f, roots),
                }
            }
            if let Some(d) = &layouts[t].drop {
                for f in d.fields.iter().flatten() {
                    field(&f.ty, layouts, needed, roots, seen);
                }
            }
        }
        let mut normal_seen = BTreeSet::new();
        for t in needed.clone() {
            normal(t, layouts, &mut needed, &mut roots, &mut normal_seen);
        }
        fn descendants(
            t: usize,
            layouts: &[Recipe],
            roots: &mut BTreeSet<usize>,
            visited: &mut BTreeSet<usize>,
        ) {
            if !visited.insert(t) {
                return;
            }
            fn field(
                f: &FieldTy,
                layouts: &[Recipe],
                roots: &mut BTreeSet<usize>,
                visited: &mut BTreeSet<usize>,
            ) {
                match f {
                    FieldTy::Inline(t) => descendants(t.index(), layouts, roots, visited),
                    FieldTy::Box(t) | FieldTy::Rc(t) | FieldTy::Arc(t) => {
                        roots.insert(t.index());
                        descendants(t.index(), layouts, roots, visited);
                    }
                    FieldTy::Array(e, _) | FieldTy::Seq(e) => field(e, layouts, roots, visited),
                    FieldTy::Map(k, v) => {
                        field(k, layouts, roots, visited);
                        field(v, layouts, roots, visited);
                    }
                    _ => {}
                }
            }
            if let Some(d) = &layouts[t].drop {
                for f in d.fields.iter().flatten() {
                    field(&f.ty, layouts, roots, visited);
                }
            }
        }
        let mut visited = BTreeSet::new();
        for root in roots.clone() {
            descendants(root, layouts, &mut roots, &mut visited);
        }
        let mut drops = vec![None; layouts.len()];
        let mut resumes = vec![None; layouts.len()];
        for (i, recipe) in layouts.iter().enumerate() {
            if recipe.drop.is_some() && needed.contains(&i) {
                drops[i] = Some(
                    object
                        .declare_function(
                            &format!("hirsel.core.drop.{i}"),
                            Linkage::Local,
                            &abi::Signature::c(&[P, P], &[B]).clif,
                        )
                        .map_err(error)?,
                );
            }
            if roots.contains(&i) {
                resumes[i] = Some(
                    object
                        .declare_function(
                            &format!("hirsel.core.resume.{i}"),
                            Linkage::Local,
                            &abi::Signature::c(&[P, P, P, P], &[B]).clif,
                        )
                        .map_err(error)?,
                );
            }
        }
        Ok(Self { drops, resumes })
    }
    pub fn emit(&self, object: &mut ObjectModule, tables: &Tables<'_>) -> Result<()> {
        for (i, &function) in self.drops.iter().enumerate() {
            if let Some(function) = function {
                self.emit_one(object, tables, i, function, false)?;
            }
        }
        for (i, &function) in self.resumes.iter().enumerate() {
            if let Some(function) = function {
                self.emit_one(object, tables, i, function, true)?;
            }
        }
        Ok(())
    }
    fn emit_one(
        &self,
        object: &mut ObjectModule,
        tables: &Tables<'_>,
        index: usize,
        function: cranelift_module::FuncId,
        resume: bool,
    ) -> Result<()> {
        let mut context = object.make_context();
        context.func.signature =
            abi::Signature::c(if resume { &[P, P, P, P] } else { &[P, P] }, &[B]).clif;
        let mut bc = FunctionBuilderContext::new();
        let mut b = FunctionBuilder::new(&mut context.func, &mut bc);
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);
        let args = b.block_params(entry).to_vec();
        let failed = b.declare_var(types::I8);
        let zero = b.ins().iconst(types::I8, 0);
        b.def_var(failed, zero);
        let mut g = Glue {
            b,
            object,
            tables,
            glues: self,
            ctx: args[0],
            failed,
        };
        if resume {
            let plan = plan_type(tables, TypeRef(index as u32))?;
            let cursor =
                g.b.ins()
                    .load(types::I64, MemFlagsData::trusted(), args[2], 0);
            let rank = g.icon(0);
            g.resume(&plan, args[1], rank, cursor, args[2], args[3])?;
            let done = g.b.ins().iconst(types::I8, 1);
            g.b.ins().return_(&[done]);
        } else {
            let resource = tables.input.types[index].repr == Repr::Resource;
            let base = if resource {
                g.load(args[1], 0)
            } else {
                args[1]
            };
            if resource && tables.trace_drops {
                let id = if matches!(&tables.layouts[index].layout, Layout::Fixed(f) if f.size >= 8)
                {
                    g.load(base, 0)
                } else {
                    g.icon(0)
                };
                g.call("hirsel_core_test_drop_trace", &[U], &[], &[id])?;
            }
            if let Some(finalizer) = tables.layouts[index]
                .drop
                .as_ref()
                .and_then(|d| d.finalizer)
            {
                g.call("hirsel_core_finalizer_begin", &[P, P], &[], &[g.ctx, base])?;
                let f = tables.functions[finalizer.0 as usize];
                let status = g.direct(f, &[g.ctx, base]);
                g.merge(status);
                g.call("hirsel_core_finalizer_end", &[P, P], &[], &[g.ctx, base])?;
            }
            g.fields(base, index)?;
            if resource {
                let storage = g.icon(tables.layouts[index].logical_size.unwrap());
                g.call("hirsel_core_resource_free", &[P, U], &[], &[base, storage])?;
            }
            let status = g.b.use_var(g.failed);
            g.b.ins().return_(&[status]);
        }
        g.b.seal_all_blocks();
        g.b.finalize(g.object.target_config());
        object
            .define_function(function, &mut context)
            .map_err(error)
    }
}

struct Glue<'a, 'b, 'c> {
    b: FunctionBuilder<'a>,
    object: &'b mut ObjectModule,
    tables: &'c Tables<'c>,
    glues: &'c Glues,
    ctx: Value,
    failed: Variable,
}
impl Glue<'_, '_, '_> {
    fn icon(&mut self, n: u64) -> Value {
        self.b.ins().iconst(types::I64, n as i64)
    }
    fn at(&mut self, base: Value, off: u64) -> Value {
        self.b.ins().iadd_imm_s(base, off as i64)
    }
    fn load(&mut self, base: Value, off: i32) -> Value {
        self.b
            .ins()
            .load(types::I64, MemFlagsData::trusted(), base, off)
    }
    fn merge(&mut self, status: Value) {
        let old = self.b.use_var(self.failed);
        let new = self.b.ins().bor(old, status);
        self.b.def_var(self.failed, new);
    }
    fn direct(&mut self, f: cranelift_module::FuncId, args: &[Value]) -> Value {
        let f = self.object.declare_func_in_func(f, self.b.func);
        let call = self.b.ins().call(f, args);
        self.b.inst_results(call)[0]
    }
    fn call(
        &mut self,
        name: &str,
        params: &[Ty],
        results: &[Ty],
        args: &[Value],
    ) -> Result<Vec<Value>> {
        super::memory::c_call(self.object, &mut self.b, name, params, results, args)
    }
    fn drop_type(&mut self, at: Value, t: TypeRef) {
        if let Some(f) = self.glues.drops[t.index()] {
            let status = self.direct(f, &[self.ctx, at]);
            self.merge(status);
        }
    }
    fn callback(&mut self, t: TypeRef) -> Value {
        let f = self
            .object
            .declare_func_in_func(self.glues.resumes[t.index()].unwrap(), self.b.func);
        self.b.ins().func_addr(types::I64, f)
    }
    fn fields(&mut self, base: Value, index: usize) -> Result<()> {
        let recipe = &self.tables.layouts[index];
        let Some(drop) = &recipe.drop else {
            return Ok(());
        };
        let Layout::Fixed(fixed) = &recipe.layout else {
            return Ok(());
        };
        if let Some(tag) = fixed.tag {
            let native = match tag.width {
                1 => types::I8,
                2 => types::I16,
                _ => types::I32,
            };
            let value = self
                .b
                .ins()
                .load(native, MemFlagsData::trusted(), base, tag.offset as i32);
            let done = self.b.create_block();
            for (i, row) in drop.fields.iter().enumerate() {
                let yes = self.b.create_block();
                let no = self.b.create_block();
                let test = self.b.ins().icmp_imm_s(IntCC::Equal, value, i as i64);
                self.b.ins().brif(test, yes, &[], no, &[]);
                self.b.switch_to_block(yes);
                for f in row {
                    let at = self.at(base, f.offset);
                    self.field(at, &f.ty)?;
                }
                self.b.ins().jump(done, &[]);
                self.b.switch_to_block(no);
            }
            self.b.ins().jump(done, &[]);
            self.b.switch_to_block(done);
        } else {
            for f in &drop.fields[0] {
                let at = self.at(base, f.offset);
                self.field(at, &f.ty)?;
            }
        }
        Ok(())
    }
    fn field(&mut self, at: Value, t: &FieldTy) -> Result<()> {
        match t {
            FieldTy::Inline(t) => self.drop_type(at, *t),
            FieldTy::Box(t) => {
                let p = self.load(at, 0);
                self.drop_type(p, *t);
                let n = self.icon(
                    charge::boxed(
                        field_size(
                            &self.tables.input.types,
                            self.tables.layouts,
                            &FieldTy::Inline(*t),
                        )
                        .2,
                    )
                    .unwrap(),
                );
                self.call("hirsel_core_free_value", &[P, U], &[], &[p, n])?;
            }
            FieldTy::Rc(payload) | FieldTy::Arc(payload) => {
                let p = self.load(at, 0);
                self.release(p, *payload, matches!(t, FieldTy::Arc(_)))?;
            }
            FieldTy::Array(e, n) => {
                let len = self.icon(*n);
                self.array(at, e, len)?;
            }
            FieldTy::Seq(e) => {
                let p = self.load(at, 0);
                let len = self.load(at, 8);
                let cap = self.load(at, 16);
                self.array(p, e, len)?;
                let logical = field_size(&self.tables.input.types, self.tables.layouts, e).2;
                let bytes = self.b.ins().imul_imm_s(cap, logical as i64);
                let charge = self.round8(bytes);
                let done = self.b.create_block();
                let free = self.b.create_block();
                let present = self.b.ins().icmp_imm_s(IntCC::NotEqual, p, 0);
                self.b.ins().brif(present, free, &[], done, &[]);
                self.b.switch_to_block(free);
                self.call("hirsel_core_free_value", &[P, U], &[], &[p, charge])?;
                self.b.ins().jump(done, &[]);
                self.b.switch_to_block(done);
            }
            FieldTy::Handle(hirsel_core_layout::Handle::Causes) => {
                let p = self.load(at, 0);
                self.call("hirsel_core_causes_drop", &[P], &[], &[p])?;
            }
            FieldTy::OwnFn => {
                let env = self.load(at, 8);
                let yes = self.b.create_block();
                let done = self.b.create_block();
                let present = self.b.ins().icmp_imm_s(IntCC::NotEqual, env, 0);
                self.b.ins().brif(present, yes, &[], done, &[]);
                self.b.switch_to_block(yes);
                let code = self.load(env, 0);
                let sig = self
                    .b
                    .import_signature(abi::Signature::c(&[P, P], &[B]).clif);
                self.b.ins().call_indirect(sig, code, &[self.ctx, env]);
                self.b.ins().jump(done, &[]);
                self.b.switch_to_block(done);
            }
            FieldTy::Scalar(_)
            | FieldTy::Stable(_)
            | FieldTy::Fn
            | FieldTy::Borrow
            | FieldTy::Locator => {}
            other => return Err(unsupported(format!("drop field {other:?}"))),
        }
        Ok(())
    }
    fn release(&mut self, p: Value, t: TypeRef, atomic: bool) -> Result<()> {
        let atomic = self.b.ins().iconst(types::I8, i64::from(atomic));
        let n = self
            .icon(charge::counted(self.tables.layouts[t.index()].logical_size.unwrap()).unwrap());
        let callback = self.callback(t);
        self.call(
            "hirsel_core_release",
            &[P, P, B, U, P],
            &[],
            &[self.ctx, p, atomic, n, callback],
        )?;
        Ok(())
    }
    fn round8(&mut self, n: Value) -> Value {
        let n = self.b.ins().iadd_imm_s(n, 7);
        self.b.ins().band_imm_s(n, -8)
    }
    fn array(&mut self, base: Value, e: &FieldTy, len: Value) -> Result<()> {
        let head = self.b.create_block();
        let body = self.b.create_block();
        let done = self.b.create_block();
        self.b.append_block_param(head, types::I64);
        let zero = self.icon(0);
        self.b.ins().jump(head, &[zero.into()]);
        self.b.switch_to_block(head);
        let i = self.b.block_params(head)[0];
        let more = self.b.ins().icmp(IntCC::UnsignedLessThan, i, len);
        self.b.ins().brif(more, body, &[], done, &[]);
        self.b.switch_to_block(body);
        let stride = field_size(&self.tables.input.types, self.tables.layouts, e).0;
        let off = self.b.ins().imul_imm_s(i, stride as i64);
        let at = self.b.ins().iadd(base, off);
        self.field(at, e)?;
        let next = self.b.ins().iadd_imm_s(i, 1);
        self.b.ins().jump(head, &[next.into()]);
        self.b.switch_to_block(done);
        Ok(())
    }
}
