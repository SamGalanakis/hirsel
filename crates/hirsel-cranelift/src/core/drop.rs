//! Typed cleanup reads producer recipes. Counted cleanup yields one descendant
//! at a time to the runtime's intrusive worklist, so pointer depth uses no stack.
use super::*;
use cranelift_codegen::ir::{MemFlagsData, Value, condcodes::IntCC};
use hirsel_core_layout::{FieldTy, Layout, Recipe, Repr, TypeRef, charge, field_size};
use std::collections::BTreeSet;
mod resume;
use resume::plan_type;

pub(super) struct Glues {
    pub drops: Vec<Option<cranelift_module::FuncId>>,
    pub callbacks: Vec<Option<cranelift_module::FuncId>>,
    pub resumes: Vec<Option<cranelift_module::FuncId>>,
    pub fixed: Vec<Option<[cranelift_module::DataId; 2]>>,
    pub buffers: Vec<(FieldTy, cranelift_module::FuncId)>,
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
        let mut descriptors = BTreeSet::new();
        for body in input.functions.iter().filter_map(|f| f.body.as_ref()) {
            for inst in &body.insts {
                match inst {
                    Inst::Drop { ty, .. } => {
                        needed.insert(ty.index());
                    }
                    Inst::Release { ty, .. } => {
                        roots.insert(ty.index());
                    }
                    Inst::Call { call } => match body.calls[call.index()].callee {
                        Callee::Runtime(RtOp::Spawn { x, r }) => {
                            descriptors.extend([x.index(), r.index()]);
                            needed.extend([x.index(), r.index()]);
                        }
                        Callee::Runtime(RtOp::ChannelNew { item }) => {
                            descriptors.insert(item.index());
                            needed.insert(item.index());
                        }
                        _ => {}
                    },
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
        // A row with a finalizer is one counted step, cleaned whole by its
        // ordinary glue (FORM v1c), so its fields are not descendants.
        fn descendants(
            t: usize,
            layouts: &[Recipe],
            roots: &mut BTreeSet<usize>,
            visited: &mut BTreeSet<usize>,
            whole: &mut BTreeSet<usize>,
        ) {
            if layouts[t]
                .drop
                .as_ref()
                .is_some_and(|d| d.finalizer.is_some())
            {
                whole.insert(t);
                return;
            }
            if !visited.insert(t) {
                return;
            }
            fn field(
                f: &FieldTy,
                layouts: &[Recipe],
                roots: &mut BTreeSet<usize>,
                visited: &mut BTreeSet<usize>,
                whole: &mut BTreeSet<usize>,
            ) {
                match f {
                    FieldTy::Inline(t) => descendants(t.index(), layouts, roots, visited, whole),
                    FieldTy::Box(t) | FieldTy::Rc(t) | FieldTy::Arc(t) => {
                        roots.insert(t.index());
                        descendants(t.index(), layouts, roots, visited, whole);
                    }
                    FieldTy::Array(e, _) | FieldTy::Seq(e) => {
                        field(e, layouts, roots, visited, whole)
                    }
                    FieldTy::Map(k, v) => {
                        field(k, layouts, roots, visited, whole);
                        field(v, layouts, roots, visited, whole);
                    }
                    _ => {}
                }
            }
            if let Some(d) = &layouts[t].drop {
                for f in d.fields.iter().flatten() {
                    field(&f.ty, layouts, roots, visited, whole);
                }
            }
        }
        let mut visited = BTreeSet::new();
        let mut whole = BTreeSet::new();
        loop {
            for root in roots.clone() {
                descendants(root, layouts, &mut roots, &mut visited, &mut whole);
            }
            let fresh: Vec<usize> = whole.difference(&needed).copied().collect();
            if fresh.is_empty() {
                break;
            }
            for t in fresh {
                needed.insert(t);
                normal(t, layouts, &mut needed, &mut roots, &mut normal_seen);
            }
        }
        let mut drops = vec![None; layouts.len()];
        let mut callbacks = vec![None; layouts.len()];
        let mut resumes = vec![None; layouts.len()];
        for (i, recipe) in layouts.iter().enumerate() {
            if (recipe.drop.is_some() || descriptors.contains(&i)) && needed.contains(&i) {
                drops[i] = Some(
                    object
                        .declare_function(
                            &format!("hirsel.core.drop.{i}"),
                            Linkage::Local,
                            &abi::Signature::c(object, &[P, P, U], &[B]).clif,
                        )
                        .map_err(error)?,
                );
            }
            if descriptors.contains(&i) {
                callbacks[i] = Some(
                    object
                        .declare_function(
                            &format!("hirsel.core.drop.callback.{i}"),
                            Linkage::Local,
                            &abi::Signature::c(object, &[P, P], &[B]).clif,
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
                            &abi::Signature::c(object, &[P, P, P, P], &[B]).clif,
                        )
                        .map_err(error)?,
                );
            }
        }
        let mut fixed = vec![None; layouts.len()];
        for (i, function) in resumes.iter().enumerate() {
            if let Some(function) = *function {
                let logical = layouts[i].logical_size.unwrap();
                let mut ids = Vec::new();
                for (owner, charge, offset) in [
                    ("own", charge::boxed(logical).unwrap(), 0),
                    (
                        "counted",
                        charge::counted(logical).unwrap(),
                        memory_layout::NODE_PAYLOAD as u64,
                    ),
                ] {
                    let id = object
                        .declare_data(
                            &format!("hirsel.core.static.{owner}.{i}"),
                            Linkage::Local,
                            false,
                            false,
                        )
                        .map_err(error)?;
                    let mut image = vec![0; memory_layout::STATIC_DESCRIPTOR_BYTES];
                    let endian = object.isa().endianness();
                    let encode = |value: u64| match endian {
                        cranelift_codegen::ir::Endianness::Little => value.to_le_bytes(),
                        cranelift_codegen::ir::Endianness::Big => value.to_be_bytes(),
                    };
                    let at = memory_layout::STATIC_DESCRIPTOR_CHARGE;
                    image[at..at + 8].copy_from_slice(&encode(charge));
                    let at = memory_layout::STATIC_DESCRIPTOR_PAYLOAD_OFFSET;
                    image[at..at + 8].copy_from_slice(&encode(offset));
                    let mut data = DataDescription::new();
                    data.define(image.into_boxed_slice());
                    data.set_align(8);
                    let reference = object.declare_func_in_data(function, &mut data);
                    data.write_function_addr(
                        memory_layout::STATIC_DESCRIPTOR_RECIPE as u32,
                        reference,
                    );
                    object.define_data(id, &data).map_err(error)?;
                    ids.push(id);
                }
                fixed[i] = Some([ids[0], ids[1]]);
            }
        }
        let mut buffers = Vec::new();
        fn collect(t: &FieldTy, buffers: &mut Vec<FieldTy>) {
            match t {
                FieldTy::Seq(e) => {
                    if !buffers.contains(t) {
                        buffers.push(t.clone());
                    }
                    collect(e, buffers);
                }
                FieldTy::Map(k, v) => {
                    if !buffers.contains(t) {
                        buffers.push(t.clone());
                    }
                    collect(k, buffers);
                    collect(v, buffers);
                }
                FieldTy::Array(e, _) => collect(e, buffers),
                _ => {}
            }
        }
        let mut types = Vec::new();
        for (i, recipe) in layouts.iter().enumerate() {
            if visited.contains(&i) {
                if let Some(d) = &recipe.drop {
                    for f in d.fields.iter().flatten() {
                        collect(&f.ty, &mut types);
                    }
                }
            }
        }
        // Inline types are already in the descendant closure.
        for (i, t) in types.into_iter().enumerate() {
            let function = object
                .declare_function(
                    &format!("hirsel.core.buffer.{i}"),
                    Linkage::Local,
                    &abi::Signature::c(object, &[P, P, P, P], &[B]).clif,
                )
                .map_err(error)?;
            buffers.push((t, function));
        }
        Ok(Self {
            drops,
            callbacks,
            resumes,
            fixed,
            buffers,
        })
    }
    pub fn emit(
        &self,
        object: &mut ObjectModule,
        tables: &Tables<'_>,
        observer: &impl profile::Observer,
        scratch: &mut Compilation,
    ) -> Result<()> {
        for (i, &function) in self.drops.iter().enumerate() {
            if let Some(function) = function {
                self.emit_one(object, tables, (i, function, false), observer, scratch)?;
                if let Some(callback) = self.callbacks[i] {
                    self.emit_callback(object, callback, function, observer, scratch)?;
                }
            }
        }
        for (i, &function) in self.resumes.iter().enumerate() {
            if let Some(function) = function {
                self.emit_one(object, tables, (i, function, true), observer, scratch)?;
            }
        }
        for (ty, function) in &self.buffers {
            self.emit_buffer(object, tables, ty, *function, observer, scratch)?;
        }
        Ok(())
    }
    fn emit_callback(
        &self,
        object: &mut ObjectModule,
        callback: cranelift_module::FuncId,
        target: cranelift_module::FuncId,
        observer: &impl profile::Observer,
        scratch: &mut Compilation,
    ) -> Result<()> {
        let context = &mut scratch.codegen;
        object.clear_context(context);
        context.func.signature = abi::Signature::c(object, &[P, P], &[B]).clif;
        let mut b = FunctionBuilder::new(&mut context.func, &mut scratch.frontend);
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);
        let args = b.block_params(entry).to_vec();
        let zero = b.ins().iconst(types::I64, 0);
        let f = object.declare_func_in_func(target, b.func);
        let call = b.ins().call(f, &[args[0], args[1], zero]);
        let result = b.inst_results(call)[0];
        b.ins().return_(&[result]);
        b.seal_all_blocks();
        b.finalize(object.target_config());
        observer.define(object, callback, context).map_err(error)
    }
    fn emit_buffer(
        &self,
        object: &mut ObjectModule,
        tables: &Tables<'_>,
        ty: &FieldTy,
        function: cranelift_module::FuncId,
        observer: &impl profile::Observer,
        scratch: &mut Compilation,
    ) -> Result<()> {
        let build = observer.enter("build_glue");
        let context = &mut scratch.codegen;
        object.clear_context(context);
        context.func.signature = abi::Signature::c(object, &[P, P, P, P], &[B]).clif;
        let mut b = FunctionBuilder::new(&mut context.func, &mut scratch.frontend);
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);
        let args = b.block_params(entry).to_vec();
        let failed = b.ins().iconst(types::I8, 0);
        let caller_site = b.ins().iconst(types::I64, 0);
        let mut g = Glue {
            b,
            object,
            tables,
            glues: self,
            ctx: args[0],
            caller_site,
            failed,
            calls: calls::Calls::new(&tables.runtimes, tables.signatures, 0),
        };
        g.resume_buffer(ty, args[1], args[2], args[3])?;
        let done = g.b.ins().iconst(types::I8, 1);
        g.b.ins().return_(&[done]);
        g.b.seal_all_blocks();
        g.b.finalize(g.object.target_config());
        drop(build);
        observer.define(object, function, context).map_err(error)
    }
    fn emit_one(
        &self,
        object: &mut ObjectModule,
        tables: &Tables<'_>,
        definition: (usize, cranelift_module::FuncId, bool),
        observer: &impl profile::Observer,
        scratch: &mut Compilation,
    ) -> Result<()> {
        let (index, function, resume) = definition;
        let build = observer.enter("build_glue");
        let context = &mut scratch.codegen;
        object.clear_context(context);
        context.func.signature = abi::Signature::c(
            object,
            if resume { &[P, P, P, P] } else { &[P, P, U] },
            &[B],
        )
        .clif;
        let mut b = FunctionBuilder::new(&mut context.func, &mut scratch.frontend);
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);
        let args = b.block_params(entry).to_vec();
        let failed = b.ins().iconst(types::I8, 0);
        let caller_site = if resume {
            b.ins().iconst(types::I64, 0)
        } else {
            args[2]
        };
        let mut g = Glue {
            b,
            object,
            tables,
            glues: self,
            ctx: args[0],
            caller_site,
            failed,
            calls: calls::Calls::new(&tables.runtimes, tables.signatures, 0),
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
            if resource
                && tables.trace_drops
                && let hirsel_core_layout::Body::Record(fields) = &tables.input.types[index].body
                && let Some(i) = fields
                    .iter()
                    .position(|f| f.ty == FieldTy::Scalar(Scalar::U64))
            {
                let Layout::Fixed(layout) = &tables.layouts[index].layout else {
                    unreachable!()
                };
                let id = g.load(base, layout.offsets[0][i] as i32);
                g.call("hirsel_core_test_drop_trace", &[U], &[], &[id])?;
            }
            if let Some(finalizer) = tables.layouts[index]
                .drop
                .as_ref()
                .and_then(|d| d.finalizer)
            {
                // A resource's finalizer runs under its grant; a core slot kernel's on
                // an own row runs under the current limit (FORM v1c).
                if resource {
                    g.call("hirsel_core_finalizer_begin", &[P, P], &[], &[g.ctx, base])?;
                }
                let f = tables.functions[finalizer.0 as usize];
                let status = g.direct(f, &[g.ctx, base, g.caller_site]);
                g.merge(status);
                if resource {
                    g.call("hirsel_core_finalizer_end", &[P, P], &[], &[g.ctx, base])?;
                }
            }
            g.fields(base, index)?;
            if resource {
                let storage = g.icon(tables.layouts[index].logical_size.unwrap());
                g.call("hirsel_core_resource_free", &[P, U], &[], &[base, storage])?;
            }
            let status = g.failed;
            g.b.ins().return_(&[status]);
        }
        g.b.seal_all_blocks();
        g.b.finalize(g.object.target_config());
        drop(build);
        observer.define(object, function, context).map_err(error)
    }
}

struct Glue<'a, 'b, 'c> {
    b: FunctionBuilder<'a>,
    object: &'b mut ObjectModule,
    tables: &'c Tables<'c>,
    glues: &'c Glues,
    ctx: Value,
    caller_site: Value,
    failed: Value,
    calls: calls::Calls<'c>,
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
        self.failed = self.b.ins().bor(self.failed, status);
    }
    fn direct(&mut self, f: cranelift_module::FuncId, args: &[Value]) -> Value {
        let f = self.calls.reference(self.object, &mut self.b, f);
        let call = self.b.ins().call(f, args);
        self.b.inst_results(call)[0]
    }
    fn call(
        &mut self,
        name: &'static str,
        params: &[Ty],
        results: &[Ty],
        args: &[Value],
    ) -> Result<calls::NativeValues> {
        self.calls
            .c(self.object, &mut self.b, name, params, results, args)
    }
    fn drop_type(&mut self, at: Value, t: TypeRef) {
        if let Some(f) = self.glues.drops[t.index()] {
            let status = self.direct(f, &[self.ctx, at, self.caller_site]);
            self.merge(status);
        }
    }
    fn descriptor(&mut self, t: TypeRef, counted: bool) -> Value {
        let id = self.glues.fixed[t.index()].unwrap()[usize::from(counted)];
        let reference = self.object.declare_data_in_func(id, self.b.func);
        self.b.ins().symbol_value(types::I64, reference)
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
            let value = super::case::case_value(&mut self.b, &tag, base, MemFlagsData::trusted());
            let done = self.b.create_block();
            self.b.append_block_param(done, types::I8);
            let incoming = self.failed;
            for (i, row) in drop.fields.iter().enumerate() {
                self.failed = incoming;
                let yes = self.b.create_block();
                let no = self.b.create_block();
                let test = self.b.ins().icmp_imm_s(IntCC::Equal, value, i as i64);
                self.b.ins().brif(test, yes, &[], no, &[]);
                self.b.switch_to_block(yes);
                for f in row {
                    let at = self.at(base, f.offset);
                    self.field(at, &f.ty)?;
                }
                self.b.ins().jump(done, &[self.failed.into()]);
                self.b.switch_to_block(no);
            }
            self.b.ins().jump(done, &[incoming.into()]);
            self.b.switch_to_block(done);
            self.failed = self.b.block_params(done)[0];
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
                fast::free(&mut self.b, &mut self.calls, self.object, self.ctx, p, n)?;
            }
            FieldTy::Rc(payload) | FieldTy::Arc(payload) => {
                let p = self.load(at, 0);
                self.release(p, *payload, matches!(t, FieldTy::Arc(_)))?;
            }
            FieldTy::Array(e, n) => {
                let len = self.icon(*n);
                self.array(at, e, len)?;
            }
            // SEM-4: a List of a zero-sized element has no storage to clean.
            FieldTy::Seq(_) if hirsel_core_layout::storage_free(&self.tables.input.types, t) => {}
            FieldTy::Seq(e) => {
                let p = self.load(at, 0);
                let len = self.load(at, 8);
                let cap = self.load(at, 16);
                let owned = self.b.ins().icmp_imm_s(IntCC::NotEqual, cap, 0);
                let zero = self.icon(0);
                let owned_len = self.b.ins().select(owned, len, zero);
                self.array(p, e, owned_len)?;
                let logical = field_size(&self.tables.input.types, self.tables.layouts, e).2;
                self.free_buffer(p, cap, logical)?;
            }
            FieldTy::Map(k, v) => {
                let p = self.load(at, 0);
                let len = self.load(at, 8);
                let cap = self.load(at, 16);
                let owned = self.b.ins().icmp_imm_s(IntCC::NotEqual, cap, 0);
                let zero = self.icon(0);
                let owned_len = self.b.ins().select(owned, len, zero);
                self.map(p, k, v, owned_len)?;
                let logical = field_size(&self.tables.input.types, self.tables.layouts, k).2
                    + field_size(&self.tables.input.types, self.tables.layouts, v).2
                    + 16;
                self.free_buffer(p, cap, logical)?;
            }
            FieldTy::Handle(hirsel_core_layout::Handle::HostFile) => {
                let handle = self.load(at, 0);
                self.call(
                    "hirsel_core_hostfile_drop",
                    &[P, U],
                    &[],
                    &[self.ctx, handle],
                )?;
            }
            FieldTy::Handle(hirsel_core_layout::Handle::Causes) => {
                let p = self.load(at, 0);
                self.call("hirsel_core_causes_drop", &[P], &[], &[p])?;
            }
            FieldTy::Handle(hirsel_core_layout::Handle::Task) => {
                let handle = self.load(at, 0);
                let status =
                    self.call("hirsel_core_task_drop", &[P, P], &[B], &[self.ctx, handle])?[0];
                self.merge(status);
            }
            FieldTy::Handle(hirsel_core_layout::Handle::Sender) => {
                let handle = self.load(at, 0);
                self.call("hirsel_core_sender_drop", &[P], &[], &[handle])?;
            }
            FieldTy::Handle(hirsel_core_layout::Handle::Receiver) => {
                let handle = self.load(at, 0);
                let status = self.call(
                    "hirsel_core_receiver_drop",
                    &[P, P],
                    &[B],
                    &[self.ctx, handle],
                )?[0];
                self.merge(status);
            }
            FieldTy::OwnFn => {
                let env = self.load(at, 8);
                let yes = self.b.create_block();
                let done = self.b.create_block();
                let present = self.b.ins().icmp_imm_s(IntCC::NotEqual, env, 0);
                self.b.ins().brif(present, yes, &[], done, &[]);
                self.b.switch_to_block(yes);
                let code = self.load(env, 0);
                let sig = self.calls.own_fn_cleanup(self.object, &mut self.b);
                self.b.ins().call_indirect(sig, code, &[self.ctx, env]);
                self.b.ins().jump(done, &[]);
                self.b.switch_to_block(done);
            }
            FieldTy::Scalar(_)
            | FieldTy::Stable(_)
            | FieldTy::Fn
            | FieldTy::Borrow
            | FieldTy::View(_)
            | FieldTy::Locator(_) => {}
            other => return Err(unsupported(format!("drop field {other:?}"))),
        }
        Ok(())
    }
    fn release(&mut self, p: Value, t: TypeRef, atomic: bool) -> Result<()> {
        let charge = charge::counted(self.tables.layouts[t.index()].logical_size.unwrap()).unwrap();
        fast::release(
            &mut self.b,
            &mut self.calls,
            self.object,
            [self.ctx, p],
            (
                charge,
                (!hirsel_core_layout::terminal(
                    &self.tables.input.types,
                    self.tables.layouts,
                    &if atomic {
                        FieldTy::Arc(t)
                    } else {
                        FieldTy::Rc(t)
                    },
                ))
                .then(|| self.glues.fixed[t.index()].unwrap()[1]),
            ),
            atomic,
        )
    }
    fn free_buffer(&mut self, p: Value, cap: Value, logical: u64) -> Result<()> {
        let bytes = self.b.ins().imul_imm_s(cap, logical as i64);
        let charge = self.round8(bytes);
        let done = self.b.create_block();
        let free = self.b.create_block();
        let present = self.b.ins().icmp_imm_s(IntCC::NotEqual, cap, 0);
        self.b.ins().brif(present, free, &[], done, &[]);
        self.b.switch_to_block(free);
        fast::free(
            &mut self.b,
            &mut self.calls,
            self.object,
            self.ctx,
            p,
            charge,
        )?;
        self.b.ins().jump(done, &[]);
        self.b.switch_to_block(done);
        Ok(())
    }
    fn map(&mut self, base: Value, k: &FieldTy, v: &FieldTy, len: Value) -> Result<()> {
        let entry =
            hirsel_core_layout::map_entry(&self.tables.input.types, self.tables.layouts, k, v);
        let head = self.b.create_block();
        let body = self.b.create_block();
        let live = self.b.create_block();
        let next = self.b.create_block();
        let done = self.b.create_block();
        self.b.append_block_param(head, types::I64);
        self.b.append_block_param(head, types::I8);
        self.b.append_block_param(next, types::I8);
        self.b.append_block_param(done, types::I8);
        let zero = self.icon(0);
        self.b.ins().jump(head, &[zero.into(), self.failed.into()]);
        self.b.switch_to_block(head);
        let i = self.b.block_params(head)[0];
        self.failed = self.b.block_params(head)[1];
        let more = self.b.ins().icmp(IntCC::UnsignedLessThan, i, len);
        self.b
            .ins()
            .brif(more, body, &[], done, &[self.failed.into()]);
        self.b.switch_to_block(body);
        let off = self.b.ins().imul_imm_s(i, entry.stride as i64);
        let at = self.b.ins().iadd(base, off);
        let state = self.load(at, 8);
        self.b
            .ins()
            .brif(state, live, &[], next, &[self.failed.into()]);
        self.b.switch_to_block(live);
        let key = self.at(at, entry.key);
        self.field(key, k)?;
        let value = self.at(at, entry.value);
        self.field(value, v)?;
        self.b.ins().jump(next, &[self.failed.into()]);
        self.b.switch_to_block(next);
        self.failed = self.b.block_params(next)[0];
        let i = self.b.ins().iadd_imm_s(i, 1);
        self.b.ins().jump(head, &[i.into(), self.failed.into()]);
        self.b.switch_to_block(done);
        self.failed = self.b.block_params(done)[0];
        Ok(())
    }
    fn round8(&mut self, n: Value) -> Value {
        let n = self.b.ins().iadd_imm_s(n, 7);
        self.b.ins().band_imm_s(n, -8)
    }
    fn element_needs_drop(&self, e: &FieldTy) -> bool {
        match e {
            FieldTy::Inline(t) => self.tables.layouts[t.index()].drop.is_some(),
            FieldTy::Array(e, n) => *n != 0 && self.element_needs_drop(e),
            FieldTy::Scalar(_)
            | FieldTy::Stable(_)
            | FieldTy::Fn
            | FieldTy::Borrow
            | FieldTy::View(_)
            | FieldTy::Locator(_) => false,
            _ => true,
        }
    }
    fn array(&mut self, base: Value, e: &FieldTy, len: Value) -> Result<()> {
        // Buffer ownership still requires its free/refund, but an element
        // without a drop operation needs no visit in an unoptimized loop.
        if !self.element_needs_drop(e) {
            return Ok(());
        }
        let head = self.b.create_block();
        let body = self.b.create_block();
        let done = self.b.create_block();
        self.b.append_block_param(head, types::I64);
        self.b.append_block_param(head, types::I8);
        self.b.append_block_param(done, types::I8);
        let zero = self.icon(0);
        self.b.ins().jump(head, &[zero.into(), self.failed.into()]);
        self.b.switch_to_block(head);
        let i = self.b.block_params(head)[0];
        self.failed = self.b.block_params(head)[1];
        let more = self.b.ins().icmp(IntCC::UnsignedLessThan, i, len);
        self.b
            .ins()
            .brif(more, body, &[], done, &[self.failed.into()]);
        self.b.switch_to_block(body);
        let stride = field_size(&self.tables.input.types, self.tables.layouts, e).0;
        let off = self.b.ins().imul_imm_s(i, stride as i64);
        let at = self.b.ins().iadd(base, off);
        self.field(at, e)?;
        let next = self.b.ins().iadd_imm_s(i, 1);
        self.b.ins().jump(head, &[next.into(), self.failed.into()]);
        self.b.switch_to_block(done);
        self.failed = self.b.block_params(done)[0];
        Ok(())
    }
}
