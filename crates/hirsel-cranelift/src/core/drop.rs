//! Typed cleanup reads producer recipes. Counted cleanup yields one descendant
//! at a time to the runtime's intrusive worklist, so pointer depth uses no stack.
use super::*;
use cranelift_codegen::ir::{MemFlagsData, Value, condcodes::IntCC};
use hirsel_core_layout::{FieldTy, Layout, Recipe, RecipeRow, Repr, TypeRef, charge, field_size};
use std::collections::BTreeSet;
mod resume;
use resume::plan_type;

pub(super) struct Glues {
    pub drops: Vec<Option<cranelift_module::FuncId>>,
    /// The resumable in-place cleanup of a value a runtime holder keeps: a
    /// channel's item, a task's result, a Latest's root (counted::Recipe).
    pub values: Vec<Option<cranelift_module::FuncId>>,
    pub resumes: Vec<Option<cranelift_module::FuncId>>,
    pub fixed: Vec<Option<[cranelift_module::DataId; 2]>>,
    /// An own-fn environment's fixed descriptor, by its capture row: the
    /// environment's word 0 names it (core-ir crate docs, "Callable environments").
    pub environments: Vec<Option<cranelift_module::DataId>>,
    pub buffers: Vec<(FieldTy, cranelift_module::FuncId)>,
    /// A resource type's drain recipe over its storage: the trace hook and
    /// finalizer as step 0, then its fields (LANG-19). Every release of a
    /// resource, ordinary or drained, runs it from the resource's own frame.
    pub resources: Vec<Option<cranelift_module::FuncId>>,
}
const P: Ty = Ty::Ptr;
const U: Ty = Ty::Scalar(Scalar::U64);
const B: Ty = Ty::Scalar(Scalar::Bool);

impl Glues {
    pub fn declare(
        object: &mut ObjectModule,
        input: &c::Module,
        layouts: &[RecipeRow],
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
        let mut values = BTreeSet::new();
        let mut environments = BTreeSet::new();
        for body in input.functions.iter().filter_map(|f| f.body.as_ref()) {
            for inst in &body.insts {
                match inst {
                    // The drain releases an environment from its descriptor.
                    Inst::Alloc {
                        kind: c::AllocKind::Environment { ty },
                        ..
                    } => {
                        environments.insert(ty.index());
                        roots.insert(ty.index());
                        needed.insert(ty.index());
                    }
                    Inst::Drop { ty, .. } => {
                        needed.insert(ty.index());
                    }
                    Inst::Release { ty, .. } => {
                        roots.insert(ty.index());
                    }
                    Inst::Call { call } => match body.calls[call.index()].callee {
                        Callee::Runtime(RtOp::Spawn { x, r } | RtOp::SpawnUnit { x, r }) => {
                            descriptors.extend([x.index(), r.index()]);
                            values.insert(r.index());
                            needed.extend([x.index(), r.index()]);
                        }
                        Callee::Runtime(RtOp::ChannelNew { item } | RtOp::LatestNew { item }) => {
                            descriptors.insert(item.index());
                            values.insert(item.index());
                            needed.insert(item.index());
                        }
                        _ => {}
                    },
                    _ => {}
                }
            }
        }
        let resources: BTreeSet<usize> = (0..input.types.len())
            .filter(|&t| input.types[t].repr == Repr::Resource)
            .collect();
        // Ordinary glue walks only the value it was handed; every owned block
        // below it is released by the drain (LANG-19): a box from its type's
        // descriptor, Seq and Map storage by its buffer recipe, whose element
        // types are therefore drained descendants (`buffered`), and a resource
        // by its recipe, whose fields are drained descendants too.
        fn normal(
            t: usize,
            layouts: &[RecipeRow],
            resources: &BTreeSet<usize>,
            needed: &mut BTreeSet<usize>,
            roots: &mut BTreeSet<usize>,
            buffered: &mut BTreeSet<usize>,
            seen: &mut BTreeSet<usize>,
        ) {
            if !seen.insert(t) {
                return;
            }
            if resources.contains(&t) {
                buffered.insert(t);
                return;
            }
            // A buffer recipe stages each element: an inline element's
            // type is a drained descendant, a boxed or counted one a root.
            fn element(f: &FieldTy, roots: &mut BTreeSet<usize>, buffered: &mut BTreeSet<usize>) {
                match f {
                    FieldTy::Inline(t) => {
                        buffered.insert(t.index());
                    }
                    FieldTy::Box(t) | FieldTy::Rc(t) | FieldTy::Arc(t) => {
                        roots.insert(t.index());
                    }
                    FieldTy::Array(e, _) | FieldTy::Seq(e) => element(e, roots, buffered),
                    FieldTy::Map(k, v) => {
                        element(k, roots, buffered);
                        element(v, roots, buffered);
                    }
                    _ => {}
                }
            }
            fn field(
                f: &FieldTy,
                layouts: &[RecipeRow],
                resources: &BTreeSet<usize>,
                needed: &mut BTreeSet<usize>,
                roots: &mut BTreeSet<usize>,
                buffered: &mut BTreeSet<usize>,
                seen: &mut BTreeSet<usize>,
            ) {
                match f {
                    FieldTy::Inline(t) => {
                        needed.insert(t.index());
                        normal(t.index(), layouts, resources, needed, roots, buffered, seen);
                    }
                    // A non-terminal box is released by the drain from its
                    // descriptor (LANG-19), so its type is a root too.
                    FieldTy::Box(t) => {
                        needed.insert(t.index());
                        roots.insert(t.index());
                        normal(t.index(), layouts, resources, needed, roots, buffered, seen);
                    }
                    FieldTy::Array(e, _) => {
                        field(e, layouts, resources, needed, roots, buffered, seen)
                    }
                    FieldTy::Seq(_) | FieldTy::Map(..) => element(f, roots, buffered),
                    _ => counted(f, roots),
                }
            }
            if let Some(d) = &layouts[t].drop {
                for f in d.fields.iter().flatten() {
                    field(&f.ty, layouts, resources, needed, roots, buffered, seen);
                }
            }
        }
        let mut normal_seen = BTreeSet::new();
        let mut buffered = BTreeSet::new();
        for t in needed.clone() {
            normal(
                t,
                layouts,
                &resources,
                &mut needed,
                &mut roots,
                &mut buffered,
                &mut normal_seen,
            );
        }
        // A resource is handed to the drain whole and cleaned by its recipe,
        // whose fields are descendants (`resourced`).
        fn descendants(
            t: usize,
            layouts: &[RecipeRow],
            resources: &BTreeSet<usize>,
            roots: &mut BTreeSet<usize>,
            visited: &mut BTreeSet<usize>,
            resourced: &mut BTreeSet<usize>,
        ) {
            if !visited.insert(t) {
                return;
            }
            if resources.contains(&t) {
                resourced.insert(t);
            }
            fn field(
                f: &FieldTy,
                layouts: &[RecipeRow],
                resources: &BTreeSet<usize>,
                roots: &mut BTreeSet<usize>,
                visited: &mut BTreeSet<usize>,
                resourced: &mut BTreeSet<usize>,
            ) {
                match f {
                    FieldTy::Inline(t) => {
                        descendants(t.index(), layouts, resources, roots, visited, resourced)
                    }
                    FieldTy::Box(t) | FieldTy::Rc(t) | FieldTy::Arc(t) => {
                        roots.insert(t.index());
                        descendants(t.index(), layouts, resources, roots, visited, resourced);
                    }
                    FieldTy::Array(e, _) | FieldTy::Seq(e) => {
                        field(e, layouts, resources, roots, visited, resourced)
                    }
                    FieldTy::Map(k, v) => {
                        field(k, layouts, resources, roots, visited, resourced);
                        field(v, layouts, resources, roots, visited, resourced);
                    }
                    _ => {}
                }
            }
            if let Some(d) = &layouts[t].drop {
                for f in d.fields.iter().flatten() {
                    field(&f.ty, layouts, resources, roots, visited, resourced);
                }
            }
        }
        let mut visited = BTreeSet::new();
        let mut resourced = BTreeSet::new();
        for root in roots
            .clone()
            .into_iter()
            .chain(values.iter().copied())
            .chain(buffered.iter().copied())
        {
            descendants(
                root,
                layouts,
                &resources,
                &mut roots,
                &mut visited,
                &mut resourced,
            );
        }
        let mut drops = vec![None; layouts.len()];
        let mut cleans = vec![None; layouts.len()];
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
            if values.contains(&i) {
                cleans[i] = Some(
                    object
                        .declare_function(
                            &format!("hirsel.core.clean.{i}"),
                            Linkage::Local,
                            &abi::Signature::c(object, &[P, P, P, P], &[B]).clif,
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
        let mut recipes = vec![None; layouts.len()];
        for &i in &resourced {
            recipes[i] = Some(
                object
                    .declare_function(
                        &format!("hirsel.core.resource.{i}"),
                        Linkage::Local,
                        &abi::Signature::c(object, &[P, P, P, P], &[B]).clif,
                    )
                    .map_err(error)?,
            );
        }
        let mut fixed = vec![None; layouts.len()];
        let mut environment = vec![None; layouts.len()];
        for (i, function) in resumes.iter().enumerate() {
            if let Some(function) = *function {
                // The block of a box or counted resource holds its pointer.
                let logical = if resources.contains(&i) {
                    8
                } else {
                    layouts[i].logical_size.unwrap()
                };
                let mut ids = Vec::new();
                let owners = [
                    ("own", charge::boxed(logical), 0),
                    (
                        "counted",
                        charge::counted(logical),
                        memory_layout::NODE_PAYLOAD as u64,
                    ),
                    // The captures start after the descriptor word.
                    ("env", charge::closure_env(logical), 8),
                ];
                let owners = if environments.contains(&i) {
                    &owners[..]
                } else {
                    &owners[..2]
                };
                for &(owner, charge, offset) in owners {
                    let charge = charge.unwrap();
                    let id = object
                        .declare_data(
                            &format!("hirsel.core.fixed.{owner}.{i}"),
                            Linkage::Local,
                            false,
                            false,
                        )
                        .map_err(error)?;
                    let frames =
                        hirsel_core_layout::frames(&input.types, layouts, TypeRef(i as u32));
                    let points = frames.points.len() as u64;
                    let mut image = vec![0; memory_layout::fixed_point(points + 1)];
                    let endian = object.isa().endianness();
                    let mut put = |at: usize, value: u64| {
                        image[at..at + 8].copy_from_slice(&match endian {
                            cranelift_codegen::ir::Endianness::Little => value.to_le_bytes(),
                            cranelift_codegen::ir::Endianness::Big => value.to_be_bytes(),
                        })
                    };
                    put(memory_layout::FIXED_DESCRIPTOR_CHARGE, charge);
                    put(memory_layout::FIXED_DESCRIPTOR_PAYLOAD_OFFSET, offset);
                    for (k, p) in (1..).zip(&frames.points) {
                        let at = memory_layout::fixed_point(k);
                        put(at + memory_layout::FIXED_POINT_BACK, at as u64);
                        put(
                            at + memory_layout::FIXED_POINT_CURSOR,
                            if p.last {
                                memory_layout::FIXED_POINT_DONE
                            } else {
                                p.cursor
                            },
                        );
                        put(at + memory_layout::FIXED_POINT_A, p.a);
                        put(at + memory_layout::FIXED_POINT_B, p.b);
                        put(
                            at + memory_layout::FIXED_POINT_INDEX,
                            p.index.unwrap_or(memory_layout::FIXED_POINT_NO_INDEX),
                        );
                    }
                    let mut data = DataDescription::new();
                    data.define(image.into_boxed_slice());
                    data.set_align(8);
                    let reference = object.declare_func_in_data(function, &mut data);
                    data.write_function_addr(
                        memory_layout::FIXED_DESCRIPTOR_RECIPE as u32,
                        reference,
                    );
                    object.define_data(id, &data).map_err(error)?;
                    ids.push(id);
                }
                fixed[i] = Some([ids[0], ids[1]]);
                environment[i] = ids.get(2).copied();
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
            if visited.contains(&i) || needed.contains(&i) {
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
            values: cleans,
            resumes,
            fixed,
            environments: environment,
            buffers,
            resources: recipes,
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
        for (i, &function) in self.values.iter().enumerate() {
            if let Some(function) = function {
                self.emit_value(object, tables, (i, function, false), observer, scratch)?;
            }
        }
        for (i, &function) in self.resources.iter().enumerate() {
            if let Some(function) = function {
                self.emit_value(object, tables, (i, function, true), observer, scratch)?;
            }
        }
        Ok(())
    }
    /// A value's cleanup in place, resumable as a drain recipe with no frame
    /// of its own: its holder keeps the cursor. Finalizers inherit no site. With
    /// `resource`, the recipe of a resource's storage instead, whose release
    /// passed its caller site through the worklist.
    fn emit_value(
        &self,
        object: &mut ObjectModule,
        tables: &Tables<'_>,
        definition: (usize, cranelift_module::FuncId, bool),
        observer: &impl profile::Observer,
        scratch: &mut Compilation,
    ) -> Result<()> {
        let (index, function, resource) = definition;
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
        let caller_site = if resource {
            b.ins().load(
                types::I64,
                MemFlagsData::trusted(),
                args[3],
                memory_layout::WORKLIST_SITE as i32,
            )
        } else {
            b.ins().iconst(types::I64, 0)
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
            point: None,
            rank: None,
            list: Some(args[3]),
        };
        let plan = if resource {
            resume::plan_resource(tables, TypeRef(index as u32))?
        } else {
            plan_type(tables, TypeRef(index as u32))?
        };
        if plan.count() != 0 {
            let cursor = g.load(args[2], 0);
            let rank = g.icon(0);
            g.resume(&plan, args[1], rank, cursor, args[2], args[3])?;
        }
        let done = g.b.ins().iconst(types::I8, 1);
        g.b.ins().return_(&[done]);
        g.b.seal_all_blocks();
        g.b.finalize(g.object.target_config());
        drop(build);
        tables.define(observer, object, function, context)
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
        let caller_site = b.ins().load(
            types::I64,
            MemFlagsData::trusted(),
            args[3],
            memory_layout::WORKLIST_SITE as i32,
        );
        let mut g = Glue {
            b,
            object,
            tables,
            glues: self,
            ctx: args[0],
            caller_site,
            failed,
            calls: calls::Calls::new(&tables.runtimes, tables.signatures, 0),
            point: None,
            rank: None,
            list: Some(args[3]),
        };
        g.resume_buffer(ty, args[1], args[2], args[3])?;
        let done = g.b.ins().iconst(types::I8, 1);
        g.b.ins().return_(&[done]);
        g.b.seal_all_blocks();
        g.b.finalize(g.object.target_config());
        drop(build);
        tables.define(observer, object, function, context)
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
            b.ins().load(
                types::I64,
                MemFlagsData::trusted(),
                args[3],
                memory_layout::WORKLIST_SITE as i32,
            )
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
            point: resume.then(Vec::new),
            rank: Some(0),
            list: resume.then(|| args[3]),
        };
        if resume {
            let plan = plan_type(tables, TypeRef(index as u32))?;
            let cursor =
                g.b.ins()
                    .load(types::I64, MemFlagsData::trusted(), args[2], 0);
            let rank = g.icon(0);
            g.resume(&plan, args[1], rank, cursor, args[2], args[3])?;
            let staged = g.point.take().unwrap_or_default();
            if cfg!(debug_assertions) {
                let frames = hirsel_core_layout::frames(
                    &tables.input.types,
                    tables.layouts,
                    TypeRef(index as u32),
                );
                debug_assert_eq!(
                    staged.len(),
                    frames.points.len(),
                    "glue and frames number suspension points alike"
                );
                for (cursor, point) in staged.iter().zip(&frames.points) {
                    debug_assert!(
                        cursor.map_or(point.index.is_some(), |c| c == point.cursor
                            && point.index.is_none()),
                        "glue and frames count steps alike: t{index} stages at {cursor:?}, frames say {point:?}"
                    );
                }
            }
            let done = g.b.ins().iconst(types::I8, 1);
            g.b.ins().return_(&[done]);
        } else {
            if tables.input.types[index].repr == Repr::Resource {
                // LANG-19: a resource is released through the drain by its recipe.
                let p = g.load(args[1], 0);
                let (recipe, storage) = g.resource_recipe(index);
                let status = g.call(
                    "hirsel_core_resource_release",
                    &[P, P, P, U, U],
                    &[B],
                    &[g.ctx, p, recipe, storage, g.caller_site],
                )?[0];
                g.merge(status);
            } else {
                g.finalize(index, args[1])?;
                g.fields(args[1], index)?;
            }
            let status = g.failed;
            g.b.ins().return_(&[status]);
        }
        g.b.seal_all_blocks();
        g.b.finalize(g.object.target_config());
        drop(build);
        tables.define(observer, object, function, context)
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
    /// In fixed resume glue, the cursor after each suspension point numbered
    /// so far (None inside an array), in the order hirsel_core_layout::frames
    /// lists them; None in buffer glue.
    point: Option<Vec<Option<u64>>>,
    /// In fixed resume glue, the step of the plan node being resumed, counted
    /// from the block's first; None inside an array.
    rank: Option<u64>,
    /// In drain recipes, the worklist whose failed byte takes each status as
    /// it happens; ordinary glue returns its status instead.
    list: Option<Value>,
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
        if let Some(list) = self.list {
            let at = memory_layout::WORKLIST_FAILED as i32;
            let flags = MemFlagsData::trusted();
            let before = self.b.ins().load(types::I8, flags, list, at);
            let after = self.b.ins().bor(before, status);
            self.b.ins().store(flags, after, list, at);
        } else {
            self.failed = self.b.ins().bor(self.failed, status);
        }
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
    /// Resource type `index`'s recipe address and logical storage.
    fn resource_recipe(&mut self, index: usize) -> (Value, Value) {
        let function = self.glues.resources[index].expect("a resource recipe");
        let reference = self.calls.reference(self.object, &mut self.b, function);
        let recipe = self.b.ins().func_addr(types::I64, reference);
        let storage = self.icon(self.tables.layouts[index].logical_size.unwrap());
        (recipe, storage)
    }
    /// Row `index`'s finalizer, if it has one, on `base`, its status merged. A
    /// resource's runs under its grant, after the test trace hook; a core slot
    /// kernel's on an own row under the current limit (FORM v1c).
    fn finalize(&mut self, index: usize, base: Value) -> Result<()> {
        let resource = self.tables.input.types[index].repr == Repr::Resource;
        if resource
            && self.tables.trace_drops
            && let hirsel_core_layout::Body::Record(fields) = &self.tables.input.types[index].body
            && let Some(i) = fields
                .iter()
                .position(|f| f.ty == FieldTy::Scalar(Scalar::U64))
        {
            let Layout::Fixed(layout) = &self.tables.layouts[index].layout else {
                unreachable!()
            };
            let id = self.load(base, layout.offsets[0][i] as i32);
            self.call("hirsel_core_test_drop_trace", &[U], &[], &[id])?;
        }
        let Some(finalizer) = self.tables.layouts[index]
            .drop
            .as_ref()
            .and_then(|d| d.finalizer)
        else {
            return Ok(());
        };
        if resource {
            self.call(
                "hirsel_core_finalizer_begin",
                &[P, P],
                &[],
                &[self.ctx, base],
            )?;
        }
        let function = &self.tables.input.functions[finalizer.0 as usize];
        let site =
            if c::source::inherits_site(&self.tables.input.instances[function.instance.index()]) {
                self.caller_site
            } else {
                self.ctx
            };
        let f = self.tables.functions[finalizer.0 as usize];
        let status = self.direct(f, &[self.ctx, base, site]);
        self.merge(status);
        if resource {
            let paths = hirsel_core_layout::foreign_cleanup(
                &self.tables.input.types,
                self.tables.layouts,
                TypeRef(index as u32),
            );
            if !paths.is_empty() {
                let count = self.icon(paths.len() as u64);
                let mut data = DataDescription::new();
                data.define(paths.into_iter().flat_map(u64::to_le_bytes).collect());
                data.set_align(8);
                let id = self
                    .object
                    .declare_anonymous_data(false, false)
                    .map_err(error)?;
                self.object.define_data(id, &data).map_err(error)?;
                let global = self.object.declare_data_in_func(id, self.b.func);
                let paths = self.b.ins().symbol_value(types::I64, global);
                self.call(
                    "hirsel_core_foreign_cleanup",
                    &[P, P, P, U],
                    &[],
                    &[self.ctx, base, paths, count],
                )?;
            }
            self.call("hirsel_core_finalizer_end", &[P, P], &[], &[self.ctx, base])?;
        }
        Ok(())
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
            // LANG-19: a block that owns more is released by the constant-stack
            // drain; a box of a resource from the descriptor of its pointer.
            FieldTy::Box(b)
                if !hirsel_core_layout::terminal(
                    &self.tables.input.types,
                    self.tables.layouts,
                    t,
                ) =>
            {
                let p = self.load(at, 0);
                let descriptor = self.descriptor(*b, false);
                let status = self.call(
                    "hirsel_core_release",
                    &[P, P, P, U],
                    &[B],
                    &[self.ctx, p, descriptor, self.caller_site],
                )?[0];
                self.merge(status);
            }
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
            FieldTy::Seq(_) | FieldTy::Map(..) => self.release_buffer(at, t)?,
            FieldTy::FrozenBytes => {
                self.call(
                    "hirsel_core_drop_frozen_bytes",
                    &[P, P],
                    &[],
                    &[self.ctx, at],
                )?;
            }
            FieldTy::Handle(
                h @ (hirsel_core_layout::Handle::HostFile
                | hirsel_core_layout::Handle::Socket
                | hirsel_core_layout::Handle::Secret),
            ) => {
                let handle = self.load(at, 0);
                let name = match h {
                    hirsel_core_layout::Handle::Socket => "hirsel_core_socket_drop",
                    hirsel_core_layout::Handle::Secret => "hirsel_core_secret_drop",
                    _ => "hirsel_core_hostfile_drop",
                };
                self.call(name, &[P, U], &[], &[self.ctx, handle])?;
            }
            FieldTy::Handle(hirsel_core_layout::Handle::Flow) => {
                let handle = self.load(at, 0);
                self.call("hirsel_core_flow_drop", &[P], &[], &[handle])?;
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
            FieldTy::Handle(hirsel_core_layout::Handle::Latest) => {
                let handle = self.load(at, 0);
                self.call("hirsel_core_latest_drop", &[P, P], &[], &[self.ctx, handle])?;
            }
            FieldTy::Handle(hirsel_core_layout::Handle::Client) => {
                let handle = self.load(at, 0);
                self.call("hirsel_core_client_drop", &[P, P], &[], &[self.ctx, handle])?;
            }
            // CONC-17: the drop seals and waits shielded in place, so it
            // cannot fail.
            FieldTy::Handle(hirsel_core_layout::Handle::Inbox) => {
                let handle = self.load(at, 0);
                self.call(
                    "hirsel_core_inbox_drop",
                    &[P, P],
                    &[Ty::Scalar(Scalar::U8)],
                    &[self.ctx, handle],
                )?;
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
                self.b.append_block_param(done, types::I8);
                let incoming = self.failed;
                self.b.ins().brif(env, yes, &[], done, &[incoming.into()]);
                self.b.switch_to_block(yes);
                // LANG-19: the environment is a drained block; word 0 names
                // its fixed descriptor.
                let descriptor = self.load(env, 0);
                let status = self.call(
                    "hirsel_core_release",
                    &[P, P, P, U],
                    &[B],
                    &[self.ctx, env, descriptor, self.caller_site],
                )?[0];
                self.merge(status);
                self.b.ins().jump(done, &[self.failed.into()]);
                self.b.switch_to_block(done);
                self.failed = self.b.block_params(done)[0];
            }
            FieldTy::Scalar(_)
            | FieldTy::RegionRef(_)
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
    /// Seq or Map storage: terminal storage is an inline free; any other is
    /// released by the drain from its buffer recipe (LANG-19). A static
    /// carrier (capacity 0) owns nothing.
    fn release_buffer(&mut self, carrier: Value, t: &FieldTy) -> Result<()> {
        let logical = match t {
            FieldTy::Seq(e) => field_size(&self.tables.input.types, self.tables.layouts, e).2,
            FieldTy::Map(k, v) => {
                field_size(&self.tables.input.types, self.tables.layouts, k).2
                    + field_size(&self.tables.input.types, self.tables.layouts, v).2
                    + 16
            }
            _ => unreachable!(),
        };
        let p = self.load(carrier, 0);
        let cap = self.load(carrier, 16);
        if hirsel_core_layout::terminal(&self.tables.input.types, self.tables.layouts, t) {
            return self.free_buffer(p, cap, logical);
        }
        let done = self.b.create_block();
        let free = self.b.create_block();
        self.b.append_block_param(done, types::I8);
        let incoming = self.failed;
        let present = self.b.ins().icmp_imm_s(IntCC::NotEqual, cap, 0);
        self.b
            .ins()
            .brif(present, free, &[], done, &[incoming.into()]);
        self.b.switch_to_block(free);
        let bytes = self.b.ins().imul_imm_s(cap, logical as i64);
        let charge = self.round8(bytes);
        let function = self.glues.buffers.iter().find(|(b, _)| b == t).unwrap().1;
        let reference = self.calls.reference(self.object, &mut self.b, function);
        let recipe = self.b.ins().func_addr(types::I64, reference);
        let status = self.call(
            "hirsel_core_release_buffer",
            &[P, P, U, P, P, U],
            &[B],
            &[self.ctx, p, charge, carrier, recipe, self.caller_site],
        )?[0];
        self.merge(status);
        self.b.ins().jump(done, &[self.failed.into()]);
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
            | FieldTy::RegionRef(_)
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
