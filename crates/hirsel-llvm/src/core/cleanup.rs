//! Recipe cleanup and resumable counted destruction.
use super::storage::zeroext;
use super::*;
use hirsel_core_layout::{FieldTy, Repr, TypeRef, field_size};

fn cleanup_dependencies(field: &FieldTy, counted: bool, pending: &mut Vec<(usize, bool)>) {
    match field {
        FieldTy::Inline(t) => pending.push((t.index(), counted)),
        FieldTy::Box(t) => {
            pending.push((t.index(), counted));
            pending.push((t.index(), true));
        }
        FieldTy::Rc(t) | FieldTy::Arc(t) => pending.push((t.index(), true)),
        // Seq and Map storage is released by the drain from its buffer recipe
        // (LANG-19), so its elements are counted cleanup wherever it is held.
        FieldTy::Map(key, value) => {
            cleanup_dependencies(key, true, pending);
            cleanup_dependencies(value, true, pending);
        }
        FieldTy::Seq(element) => cleanup_dependencies(element, true, pending),
        FieldTy::Array(element, _) => cleanup_dependencies(element, counted, pending),
        _ => {}
    }
}

/// An emitter for module-level glue: no CoreIR body, no debug locations.
#[allow(clippy::too_many_arguments)]
pub(super) fn glue_emitter<'a>(
    module: &'a Module,
    recipes: &'a [hirsel_core_layout::Recipe],
    failures: &'a failure::Failures,
    facts: bool,
    body: &'a Body,
    abi: abi::Return,
    has_status: bool,
    caller_site: Value,
    sink: &'a mut dyn Sink,
    items: &'a mut Items,
) -> FunctionEmitter<'a> {
    FunctionEmitter {
        function: None,
        call_layouts: None,
        append_fast: false,
        shadow_callee: None,
        module,
        body,
        recipes,
        abi,
        has_status,
        failures,
        constants: &[],
        observe_charge: false,
        receive_loops: Default::default(),
        facts,
        aliases: None,
        caller_site,
        debug_location: None,
        anchor: None,
        relocatable_sites: false,
        test_hooks: cfg!(test),
        value_slots: Vec::new(),
        static_vectors: None,
        literals: Vec::new(),
        address_slots: Vec::new(),
        scoped: hirsel_core_ir::extent::ScopedSlots::none(body.slots.len()),
        live: None,
        scoped_ended: false,
        step_labels: Default::default(),
        unchecked_steps: false,
        precharge: None,
        step_neutral: Some(&[]),
        step_counter: None,
        neutral_step_call: false,
        view_invariants: Vec::new(),
        empty_bounds: empty::EmptyBounds::default(),
        sink,
        items,
        next: 0,
        fixed_point: None,
        fixed_rank: None,
    }
}

/// `zeroext i8 core_drop_N(ptr context, ptr payload, i64 caller_site)`.
pub(super) fn drop_signature() -> FnSig {
    // The status byte is an internal result: zero-extended like every CoreIR
    // status (abi::Return::core), so a finalizer's calls agree with its definition.
    FnSig {
        extension: Extension::Zero,
        ..FnSig::plain(Type::I8, [Type::Ptr, Type::Ptr, Type::I64])
    }
}
/// `zeroext i1 (ptr context, ptr payload, ptr cursor, ptr worklist)`: true when done.
pub(super) fn step_signature() -> FnSig {
    FnSig {
        extension: Extension::Zero,
        ..FnSig::plain(Type::I1, [Type::Ptr, Type::Ptr, Type::Ptr, Type::Ptr])
    }
}
pub(super) const STEP_PARAMS: [Local; 4] = [
    Local::CONTEXT,
    Local::PAYLOAD,
    Local::CURSOR,
    Local::WORKLIST,
];

#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_arguments)]
pub(super) fn drop_glue(
    module: &Module,
    target: Target,
    recipes: &[hirsel_core_layout::Recipe],
    failures: &failure::Failures,
    facts: bool,
    test_hooks: bool,
    sink: &mut dyn Sink,
    items: &mut Items,
) -> Result<()> {
    let mut normal = BTreeSet::new();
    let mut counted = BTreeSet::new();
    // The values runtime holders keep (a task's result, a channel's item, a
    // Latest's root), cleaned in place on the active drain.
    let mut values = BTreeSet::new();
    // The capture rows of own-fn environments, released by the drain from
    // their descriptors.
    let mut environments = BTreeSet::new();
    // Resource types, each released through the drain by its recipe
    // (`core_resource_T`) over its storage (LANG-19).
    let mut resources = BTreeSet::new();
    let mut pending = Vec::new();
    for body in module.functions.iter().filter_map(|f| f.body.as_ref()) {
        for inst in &body.insts {
            match inst {
                Inst::Alloc {
                    kind: AllocKind::Environment { ty },
                    ..
                } => {
                    environments.insert(ty.index());
                    pending.push((ty.index(), true));
                }
                Inst::Drop { ty, .. } => pending.push((ty.index(), false)),
                Inst::Release { ty, .. } => pending.push((ty.index(), true)),
                Inst::Call { call } => match body.calls[call.index()].callee {
                    Callee::Runtime(RtOp::Spawn { x, r }) => {
                        values.insert(r.index());
                        pending.push((x.index(), false));
                        pending.push((r.index(), false));
                    }
                    Callee::Runtime(RtOp::ChannelNew { item } | RtOp::LatestNew { item }) => {
                        values.insert(item.index());
                        pending.push((item.index(), false))
                    }
                    _ => {}
                },
                _ => {}
            }
        }
    }
    for &t in &values {
        if let Some(drop) = &recipes[t].drop
            && module.types[t].repr != Repr::Resource
        {
            for field in drop.fields.iter().flatten() {
                cleanup_dependencies(&field.ty, true, &mut pending);
            }
        }
    }
    while let Some((t, mechanical)) = pending.pop() {
        let seen = if mechanical {
            &mut counted
        } else {
            &mut normal
        };
        if !seen.insert(t) {
            continue;
        }
        if let Some(drop) = &recipes[t].drop {
            // Every release of a resource runs its recipe on the drain, so its
            // fields are counted cleanup.
            let resource = module.types[t].repr == Repr::Resource;
            if resource {
                resources.insert(t);
            }
            for field in drop.fields.iter().flatten() {
                cleanup_dependencies(&field.ty, mechanical || resource, &mut pending);
            }
        }
    }
    let empty = Body::default();
    let payload = Value::Local(Local::PAYLOAD);
    for &t in &normal {
        let drop_name = format!("core_drop_{t}");
        sink.begin_function(
            &drop_name,
            &drop_signature(),
            &[Local::CONTEXT, Local::PAYLOAD, Local::CALLER_SITE],
            &FnAttrs::private(),
        );
        {
            let mut e = glue_emitter(
                module,
                recipes,
                failures,
                facts,
                &empty,
                abi::Return::internal_on_target(target, &[]),
                true,
                Value::Local(Local::CALLER_SITE),
                &mut *sink,
                &mut *items,
            );
            if module.types[t].repr == Repr::Resource {
                // LANG-19: a resource is released through the drain by its recipe.
                let resource = e.load(&Type::Ptr, &payload, 1);
                let failed = e.release_resource(t, &resource);
                e.ret(&Type::I8, &failed);
                drop(e);
                sink.end_function();
                continue;
            }
            let status = e.temp(Type::I8);
            e.store(&Type::I8, &int(0), &status, 0);
            let base = payload.clone();
            e.test_hooks = test_hooks;
            e.finalize(t, &base, &status);
            if let Some(drop) = &recipes[t].drop {
                let tag = e.fixed(TypeId(t as u32)).tag;
                if let Some(tag) = tag {
                    let which = e.case_value(&tag, &base);
                    let arms: Vec<_> = (1..drop.fields.len())
                        .map(|row| (row as i128, Label::new("case", row)))
                        .collect();
                    e.effect(Operation::Switch(
                        &Type::I32,
                        &which,
                        Label::new("case", 0),
                        &arms,
                    ));
                } else {
                    e.br(Label::new("case", 0));
                }
                let settle = Label::plain("settle");
                for (row, fields) in drop.fields.iter().enumerate() {
                    e.block(Label::new("case", row));
                    for field in fields {
                        let p = e.offset(&base, field.offset);
                        e.clean_field(&field.ty, &p, &status)?;
                    }
                    e.br(settle);
                }
                e.block(settle);
            }
            let failed = e.load(&Type::I8, &status, 0);
            e.ret(&Type::I8, &failed);
        }
        sink.end_function();
    }
    let mut sequences = Vec::new();
    for &t in counted.iter().chain(&values).chain(&normal) {
        if let Some(drop) = &recipes[t].drop {
            for field in drop.fields.iter().flatten() {
                collections::collect(&field.ty, &mut sequences);
            }
        }
    }
    counted_glue(
        module,
        target,
        recipes,
        failures,
        facts,
        &counted,
        &environments,
        sink,
        items,
    )?;
    value_glue(
        module, target, recipes, failures, facts, &values, sink, items,
    )?;
    resource_glue(
        module, target, recipes, failures, facts, test_hooks, &resources, sink, items,
    )?;
    collections::glue(
        module, target, recipes, failures, facts, &sequences, sink, items,
    )?;
    Ok(())
}

impl FunctionEmitter<'_> {
    pub(super) fn clean_field(&mut self, field: &FieldTy, p: &Value, status: &Value) -> Result<()> {
        let site = self.site_word();
        let failed = match field {
            FieldTy::Inline(t) => Some(self.call(
                &drop_signature(),
                &Value::global(format!("core_drop_{}", t.0)),
                &[CONTEXT, p.clone(), site],
                &CallAttrs::default(),
            )),
            // LANG-19: a block that owns more is released by the constant-stack
            // drain; a box of a resource from the descriptor of its pointer.
            FieldTy::Box(t)
                if !hirsel_core_layout::terminal(&self.module.types, self.recipes, field) =>
            {
                let block = self.load(&Type::Ptr, p, 1);
                Some(self.runtime(
                    "hirsel_core_release",
                    Type::I8,
                    &[
                        ptr(CONTEXT),
                        ptr(block),
                        ptr(Value::global(format!("core_fixed_own_{}", t.0))),
                        word(site),
                    ],
                    &CallAttrs::default(),
                ))
            }
            FieldTy::Box(t) => {
                let block = self.load(&Type::Ptr, p, 1);
                let failed = self.call(
                    &drop_signature(),
                    &Value::global(format!("core_drop_{}", t.0)),
                    &[CONTEXT, block.clone(), site],
                    &CallAttrs::default(),
                );
                let logical = self.recipes[t.index()].logical_size.unwrap();
                let charge = hirsel_core_layout::charge::boxed(logical).unwrap();
                self.free_block(&block, &int(charge));
                Some(failed)
            }
            FieldTy::Array(e, n) => {
                let i64 = Type::I64;
                let stride = field_size(&self.module.types, self.recipes, e).0;
                let index = self.temp(Type::I64);
                self.store(&i64, &int(0), &index, 0);
                let label = self.counter();
                let head = Label::new("array_head", label);
                let body = Label::new("array_body", label);
                let done = Label::new("array_done", label);
                self.br(head);
                self.block(head);
                let i = self.load(&i64, &index, 0);
                let more = self.cmp(Predicate::Ult, &i64, &i, &int(*n));
                self.cond(&more, body, done);
                self.block(body);
                let off = self.bin(Binary::Mul, &i64, &i, &int(stride));
                let element = self.offset_by(p, &off);
                self.clean_field(e, &element, status)?;
                let next = self.bin(Binary::Add, &i64, &i, &int(1));
                self.store(&i64, &next, &index, 0);
                self.br(head);
                self.block(done);
                None
            }
            FieldTy::Seq(_) | FieldTy::Map(_, _) => self.release_collection(field, p),
            FieldTy::FrozenBytes => {
                self.runtime_void(
                    "hirsel_core_drop_frozen_bytes",
                    &[ptr(CONTEXT), ptr(p.clone())],
                );
                None
            }
            FieldTy::OwnFn => {
                let at = self.offset(p, 8);
                let environment = self.load(&Type::Ptr, &at, 1);
                let allocated = self.cmp(Predicate::Ne, &Type::Ptr, &environment, &Value::Null);
                let label = self.counter();
                let drop = Label::new("closure_drop", label);
                let done = Label::new("closure_done", label);
                self.cond(&allocated, drop, done);
                self.block(drop);
                // LANG-19: the environment is a drained block; word 0 names
                // its fixed descriptor.
                let descriptor = self.load(&Type::Ptr, &environment, 8);
                let failed = self.runtime(
                    "hirsel_core_release",
                    Type::I8,
                    &[ptr(CONTEXT), ptr(environment), ptr(descriptor), word(site)],
                    &CallAttrs::default(),
                );
                let old = self.load(&Type::I8, status, 0);
                let both = self.bin(Binary::Or, &Type::I8, &old, &failed);
                self.store(&Type::I8, &both, status, 0);
                self.br(done);
                self.block(done);
                None
            }
            FieldTy::Rc(t) | FieldTy::Arc(t) => {
                let node = self.load(&Type::Ptr, p, 1);
                self.release_node(&node, *t, matches!(field, FieldTy::Arc(_)))?;
                None
            }
            FieldTy::Scalar(_)
            | FieldTy::RegionRef(_)
            | FieldTy::Fn
            | FieldTy::Borrow
            | FieldTy::View(_)
            | FieldTy::Locator(_) => None,
            FieldTy::Handle(
                h @ (hirsel_core_layout::Handle::HostFile
                | hirsel_core_layout::Handle::Tape
                | hirsel_core_layout::Handle::Socket),
            ) => {
                let handle = self.load(&Type::I64, p, 1);
                let name = if *h == hirsel_core_layout::Handle::Tape {
                    "hirsel_core_tape_drop"
                } else if *h == hirsel_core_layout::Handle::Socket {
                    "hirsel_core_socket_drop"
                } else {
                    "hirsel_core_hostfile_drop"
                };
                self.runtime_void(name, &[ptr(CONTEXT), word(handle)]);
                None
            }
            FieldTy::Handle(hirsel_core_layout::Handle::Flow) => {
                let handle = self.load(&Type::Ptr, p, 1);
                self.runtime_void("hirsel_core_flow_drop", &[ptr(handle)]);
                None
            }
            FieldTy::Handle(hirsel_core_layout::Handle::Causes) => {
                let chain = self.load(&Type::Ptr, p, 1);
                self.runtime_void("hirsel_core_causes_drop", &[ptr(chain)]);
                None
            }
            FieldTy::Handle(
                hirsel_core_layout::Handle::Task | hirsel_core_layout::Handle::Receiver,
            ) => {
                let handle = self.load(&Type::Ptr, p, 1);
                let name = if *field == FieldTy::Handle(hirsel_core_layout::Handle::Task) {
                    "hirsel_core_task_drop"
                } else {
                    "hirsel_core_receiver_drop"
                };
                Some(
                    self.native_call(
                        name,
                        &[Ty::Scalar(Scalar::U8)],
                        &[ptr(CONTEXT), ptr(handle)],
                    )
                    .remove(0),
                )
            }
            FieldTy::Handle(hirsel_core_layout::Handle::Sender) => {
                let handle = self.load(&Type::Ptr, p, 1);
                self.runtime_void("hirsel_core_sender_drop", &[ptr(handle)]);
                None
            }
            FieldTy::Handle(hirsel_core_layout::Handle::Latest) => {
                let handle = self.load(&Type::Ptr, p, 1);
                self.runtime_void("hirsel_core_latest_drop", &[ptr(CONTEXT), ptr(handle)]);
                None
            }
            FieldTy::Handle(hirsel_core_layout::Handle::Capability) => None,
            other => return Err(Error::Unsupported(format!("drop field {other:?}"))),
        };
        if let Some(failed) = failed {
            let old = self.load(&Type::I8, status, 0);
            let both = self.bin(Binary::Or, &Type::I8, &old, &failed);
            self.store(&Type::I8, &both, status, 0);
        }
        Ok(())
    }
    pub(super) fn release_node(&mut self, node: &Value, t: TypeRef, atomic: bool) -> Result<()> {
        let last = self.items.release_last(t.0, atomic);
        self.release_fast(node, &last, atomic);
        Ok(())
    }
}

/// Define each requested counted type's last release (fast.rs
/// `release_last_body`), private and never inlined.
#[allow(clippy::too_many_arguments)]
pub(super) fn release_lasts(
    module: &Module,
    target: Target,
    recipes: &[hirsel_core_layout::Recipe],
    failures: &failure::Failures,
    facts: bool,
    lasts: &[(u32, bool)],
    sink: &mut dyn Sink,
    items: &mut Items,
) -> Result<()> {
    let empty = Body::default();
    for &(t, atomic) in lasts {
        let name = release_last_name(t, atomic);
        sink.begin_function(
            &name,
            &super::fast::release_last_signature(),
            &[Local::CONTEXT, Local::PAYLOAD],
            &FnAttrs {
                inline: Inline::Never,
                ..FnAttrs::private()
            },
        );
        let mut e = glue_emitter(
            module,
            recipes,
            failures,
            facts,
            &empty,
            abi::Return::internal_on_target(target, &[]),
            true,
            int(0),
            &mut *sink,
            &mut *items,
        );
        let t = TypeRef(t);
        let logical = held_logical(module, recipes, t.index());
        let charge = hirsel_core_layout::charge::counted(logical).unwrap();
        let edge = if atomic {
            FieldTy::Arc(t)
        } else {
            FieldTy::Rc(t)
        };
        let recipe = (!hirsel_core_layout::terminal(&module.types, recipes, &edge))
            .then(|| Value::global(format!("core_fixed_counted_{}", t.0)));
        e.release_last_body(
            &Value::Local(Local::PAYLOAD),
            charge,
            recipe.as_ref(),
            atomic,
        );
        e.effect(Operation::Return(None));
        e.sink.end_function();
    }
    Ok(())
}

pub(super) fn mechanical_count(
    module: &Module,
    recipes: &[hirsel_core_layout::Recipe],
    field: &FieldTy,
) -> Result<u64> {
    Ok(match field {
        FieldTy::Seq(_) if hirsel_core_layout::storage_free(&module.types, field) => 0,
        FieldTy::Box(_)
        | FieldTy::Rc(_)
        | FieldTy::Arc(_)
        | FieldTy::Seq(_)
        | FieldTy::Map(_, _)
        | FieldTy::FrozenBytes => 1,
        FieldTy::Inline(t) => {
            let Some(drop) = &recipes[t.index()].drop else {
                return Ok(0);
            };
            // A resource is one step, cleaned whole.
            if module.types[t.index()].repr == Repr::Resource {
                return Ok(1);
            }
            // A finalizer is step 0 of its row.
            let mut max = 0;
            for row in &drop.fields {
                let mut n = 0;
                for f in row {
                    n += mechanical_count(module, recipes, &f.ty)?;
                }
                max = max.max(n);
            }
            u64::from(drop.finalizer.is_some()) + max
        }
        FieldTy::Array(e, n) => mechanical_count(module, recipes, e)? * n,
        FieldTy::Handle(hirsel_core_layout::Handle::Capability) => 0,
        FieldTy::Handle(_) | FieldTy::OwnFn => 1,
        FieldTy::Scalar(_)
        | FieldTy::Fn
        | FieldTy::RegionRef(_)
        | FieldTy::Borrow
        | FieldTy::View(_)
        | FieldTy::Locator(_)
        | FieldTy::Text
        | FieldTy::FixedSpan(_)
        | FieldTy::SeqSpan(_)
        | FieldTy::IndexedSpan(_)
        | FieldTy::Link(_) => 0,
    })
}
impl FunctionEmitter<'_> {
    /// The next suspension point of fixed glue, or 0 in buffer glue.
    pub(super) fn next_point(&mut self) -> u64 {
        let cursor = self.fixed_rank.map(|r| r + 1);
        self.fixed_point.as_mut().map_or(0, |points| {
            points.push(cursor);
            points.len() as u64
        })
    }
    pub(super) fn mechanical_field(
        &mut self,
        field: &FieldTy,
        p: &Value,
        cursor: &Value,
        base: u64,
    ) -> Result<()> {
        let i64 = Type::I64;
        let count = mechanical_count(self.module, self.recipes, field)?;
        if count == 0 {
            return Ok(());
        }
        let local = self.bin(Binary::Sub, &i64, cursor, &int(base));
        let here = self.cmp(Predicate::Ult, &i64, &local, &int(count));
        let n = self.counter();
        let clean = Label::new("clean", n);
        let skip = Label::new("skip", n);
        self.cond(&here, clean, skip);
        self.block(clean);
        match field {
            // A resource is handed over whole; its recipe cleans it from its
            // own frame.
            FieldTy::Inline(t) if self.module.types[t.index()].repr == Repr::Resource => {
                let resource = self.load(&Type::Ptr, p, 1);
                let point = self.next_point();
                let storage = self.recipes[t.index()].logical_size.unwrap();
                self.runtime_void(
                    "hirsel_core_resource_stage",
                    &[
                        ptr(resource),
                        ptr(Value::global(format!("core_resource_{}", t.0))),
                        word(int(storage)),
                        ptr(Value::Local(Local::WORKLIST)),
                        word(int(point)),
                    ],
                );
                self.ret(&Type::I1, &FALSE);
            }
            // One step cleaned whole by ordinary glue, under the release's
            // site. A failure goes into the worklist's byte and cleaning goes
            // on (FAIL-10).
            // A runtime holder begins its cleanup and hands it over (CONC-8,
            // CONC-9, CONC-13); a Latest only at its last handle.
            FieldTy::Handle(
                h @ (hirsel_core_layout::Handle::Receiver
                | hirsel_core_layout::Handle::Task
                | hirsel_core_layout::Handle::Latest),
            ) => {
                let handle = self.load(&Type::Ptr, p, 1);
                let point = self.next_point();
                let list = Value::Local(Local::WORKLIST);
                let bool_ = Ty::Scalar(Scalar::Bool);
                let staged = if *h == hirsel_core_layout::Handle::Latest {
                    self.native_call(
                        "hirsel_core_latest_release",
                        &[bool_],
                        &[ptr(handle), ptr(list), word(int(point))],
                    )
                } else {
                    let name = if *h == hirsel_core_layout::Handle::Task {
                        "hirsel_core_task_release"
                    } else {
                        "hirsel_core_receiver_release"
                    };
                    self.native_call(
                        name,
                        &[bool_],
                        &[ptr(CONTEXT), ptr(handle), ptr(list), word(int(point))],
                    )
                }
                .remove(0);
                let staged = self.cmp(Predicate::Ne, &Type::I8, &staged, &int(0));
                let handed = Label::new("handed", n);
                self.cond(&staged, handed, Label::plain("resume"));
                self.block(handed);
                self.ret(&Type::I1, &FALSE);
            }
            FieldTy::Handle(_) => self.clean_whole(field, p)?,
            // An environment is a fixed block whose word 0 names its
            // descriptor; a captureless closure has none.
            FieldTy::OwnFn => {
                let at = self.offset(p, 8);
                let environment = self.load(&Type::Ptr, &at, 1);
                let present = self.cmp(Predicate::Ne, &Type::Ptr, &environment, &Value::Null);
                let staged = Label::new("environment", n);
                self.cond(&present, staged, Label::plain("resume"));
                self.block(staged);
                let descriptor = self.load(&Type::Ptr, &environment, 8);
                let point = self.next_point();
                let list = Value::Local(Local::WORKLIST);
                let at = self.offset(&list, memory_layout::WORKLIST_NODE as u64);
                self.store(&Type::Ptr, &environment, &at, 0);
                let at = self.offset(&list, memory_layout::WORKLIST_DESCRIPTOR as u64);
                self.store(&Type::Ptr, &descriptor, &at, 0);
                let at = self.offset(&list, memory_layout::WORKLIST_POINT as u64);
                self.store(&Type::I64, &int(point), &at, 0);
                self.ret(&Type::I1, &FALSE);
            }
            FieldTy::Inline(t) => {
                let drop = self.recipes[t.index()].drop.as_ref().unwrap();
                // Step 0 of a row with a finalizer runs it and yields nothing;
                // it may fail, which goes into the worklist's byte (FAIL-10).
                let first = u64::from(drop.finalizer.is_some());
                if first == 1 {
                    let now = self.cmp(Predicate::Eq, &i64, &local, &int(0));
                    let finalize = Label::new("finalize", n);
                    let fields = Label::new("fields", n);
                    self.cond(&now, finalize, fields);
                    self.block(finalize);
                    let list = Value::Local(Local::WORKLIST);
                    let failed = self.offset(&list, memory_layout::WORKLIST_FAILED as u64);
                    self.finalize(t.index(), p, &failed);
                    self.br(Label::plain("resume"));
                    self.block(fields);
                }
                let tag = self.fixed(TypeId(t.0)).tag;
                let row_label = |row: usize| Label {
                    name: "row",
                    n: n as u32,
                    edge: row as u32,
                };
                if let Some(tag) = tag {
                    let tagvalue = self.case_value(&tag, p);
                    let arms: Vec<_> = (1..drop.fields.len())
                        .map(|row| (row as i128, row_label(row)))
                        .collect();
                    self.effect(Operation::Switch(
                        &Type::I32,
                        &tagvalue,
                        row_label(0),
                        &arms,
                    ));
                } else {
                    self.br(row_label(0));
                }
                let rank = self.fixed_rank;
                for (row, fields) in drop.fields.iter().enumerate() {
                    self.block(row_label(row));
                    let mut offset = first;
                    for f in fields {
                        let at = self.offset(p, f.offset);
                        self.fixed_rank = rank.map(|r| r + offset);
                        self.mechanical_field(&f.ty, &at, &local, offset)?;
                        offset += mechanical_count(self.module, self.recipes, &f.ty)?;
                    }
                    self.br(Label::plain("resume"));
                }
                self.fixed_rank = rank;
            }
            FieldTy::Array(e, _) => {
                let per = mechanical_count(self.module, self.recipes, e)?;
                let index = self.bin(Binary::UDiv, &i64, &local, &int(per));
                let inner = self.bin(Binary::URem, &i64, &local, &int(per));
                let stride = field_size(&self.module.types, self.recipes, e).0;
                let off = self.bin(Binary::Mul, &i64, &index, &int(stride));
                let at = self.offset_by(p, &off);
                let rank = self.fixed_rank.take();
                self.mechanical_field(e, &at, &inner, 0)?;
                self.fixed_rank = rank;
                self.br(Label::plain("resume"));
            }
            FieldTy::Seq(_) | FieldTy::Map(_, _) => self.enqueue_collection(field, p),
            FieldTy::FrozenBytes => self.clean_whole(field, p)?,
            FieldTy::Box(t) | FieldTy::Rc(t) | FieldTy::Arc(t) => {
                let node = self.load(&Type::Ptr, p, 1);
                let logical = held_logical(self.module, self.recipes, t.index());
                let charge = if matches!(field, FieldTy::Box(_)) {
                    hirsel_core_layout::charge::boxed(logical).unwrap()
                } else {
                    hirsel_core_layout::charge::counted(logical).unwrap()
                };
                if hirsel_core_layout::terminal(&self.module.types, self.recipes, field) {
                    if matches!(field, FieldTy::Box(_)) {
                        self.free_block(&node, &int(charge));
                    } else {
                        self.release_node(&node, *t, matches!(field, FieldTy::Arc(_)))?;
                    }
                    self.br(Label::plain("resume"));
                    self.block(skip);
                    return Ok(());
                }
                if matches!(field, FieldTy::Box(_)) {
                    self.clean_flat_cases(&node, *t, charge, n)?;
                }
                let owner = if matches!(field, FieldTy::Box(_)) {
                    "own"
                } else {
                    let atomic = matches!(field, FieldTy::Arc(_));
                    let last = self.decrement_node(&node, atomic);
                    let retained = self.bin(Binary::Xor, &Type::I1, &last, &true.into());
                    let nonlast = Label::new("child_nonlast", n);
                    let last_label = Label::new("child_last", n);
                    self.likely(&retained, nonlast, last_label);
                    self.block(nonlast);
                    if atomic {
                        self.arc_event("hirsel_core_arc_released", &[zeroext(Type::I1, false)]);
                    }
                    self.br(Label::plain("resume"));
                    self.block(last_label);
                    if atomic {
                        self.effect(Operation::Fence(Ordering::Acquire));
                        self.arc_event("hirsel_core_arc_released", &[zeroext(Type::I1, true)]);
                    }
                    "counted"
                };
                // Stage the descendant inline (abi_layout WORKLIST_*).
                let point = self.next_point();
                let list = Value::Local(Local::WORKLIST);
                let at = self.offset(&list, memory_layout::WORKLIST_NODE as u64);
                self.store(&Type::Ptr, &node, &at, 0);
                let at = self.offset(&list, memory_layout::WORKLIST_DESCRIPTOR as u64);
                self.store(
                    &Type::Ptr,
                    &Value::global(format!("core_fixed_{owner}_{}", t.0)),
                    &at,
                    0,
                );
                let at = self.offset(&list, memory_layout::WORKLIST_POINT as u64);
                self.store(&Type::I64, &int(point), &at, 0);
                self.ret(&Type::I1, &FALSE);
            }
            _ => unreachable!(),
        }
        self.block(skip);
        Ok(())
    }
}
impl FunctionEmitter<'_> {
    /// A drain step that runs `field`'s ordinary cleanup, recording a failure
    /// in the worklist's failed byte.
    fn clean_whole(&mut self, field: &FieldTy, p: &Value) -> Result<()> {
        let list = Value::Local(Local::WORKLIST);
        let failed = self.offset(&list, memory_layout::WORKLIST_FAILED as u64);
        self.clean_field(field, p, &failed)?;
        self.br(Label::plain("resume"));
        Ok(())
    }
    /// Row `t`'s finalizer, if it has one, on `base`, its failure ORed into the
    /// byte at `status`. A resource's runs under its grant; a core slot
    /// kernel's on an own row under the current limit (FORM v1c).
    fn finalize(&mut self, t: usize, base: &Value, status: &Value) {
        let Some(f) = self.module.types[t].finalizer else {
            return;
        };
        let resource = self.module.types[t].repr == Repr::Resource;
        let site = self.call_site_word(Callee::Direct(FuncId(f.0)));
        if resource {
            self.runtime_void(
                "hirsel_core_finalizer_begin",
                &[ptr(CONTEXT), ptr(base.clone())],
            );
        }
        let symbol = self.items.function_symbol(FuncId(f.0));
        let args = [CONTEXT, base.clone(), site];
        if self.failures.may_fail[f.0 as usize] {
            let failed = self.call(
                &drop_signature(),
                &Value::global(symbol.to_owned()),
                &args,
                &CallAttrs::default(),
            );
            let before = self.load(&Type::I8, status, 0);
            let after = self.bin(Binary::Or, &Type::I8, &before, &failed);
            self.store(&Type::I8, &after, status, 0);
        } else {
            self.call(
                &FnSig::plain(Type::Void, [Type::Ptr, Type::Ptr, Type::I64]),
                &Value::global(format!("{symbol}.infallible")),
                &args,
                &CallAttrs::default(),
            );
        }
        if resource {
            self.runtime_void(
                "hirsel_core_finalizer_end",
                &[ptr(CONTEXT), ptr(base.clone())],
            );
        }
    }
    /// Recipes clean under the site their release was called with.
    pub(super) fn release_site(&mut self) {
        let list = Value::Local(Local::WORKLIST);
        let at = self.offset(&list, memory_layout::WORKLIST_SITE as u64);
        self.caller_site = self.load(&Type::I64, &at, 8);
    }
    /// Whether cleaning `field` frees at most terminal storage and yields nothing.
    fn flat(&self, field: &FieldTy) -> bool {
        match field {
            FieldTy::Box(_)
            | FieldTy::Rc(_)
            | FieldTy::Arc(_)
            | FieldTy::Seq(_)
            | FieldTy::Map(..) => {
                hirsel_core_layout::terminal(&self.module.types, self.recipes, field)
            }
            FieldTy::Inline(t) => self.recipes[t.index()].drop.is_none(),
            FieldTy::Array(e, _) => self.flat(e),
            FieldTy::Scalar(_)
            | FieldTy::RegionRef(_)
            | FieldTy::Fn
            | FieldTy::Borrow
            | FieldTy::View(_)
            | FieldTy::Locator(_)
            | FieldTy::FrozenBytes => true,
            _ => false,
        }
    }
    /// A boxed child in a case that owns only terminal storage is that case's
    /// whole recipe: its fields are freed and the block refunded here, with no
    /// hand-over. Other cases fall through to the hand-over.
    fn clean_flat_cases(&mut self, node: &Value, t: TypeRef, charge: u64, n: usize) -> Result<()> {
        let Some(drop) = &self.recipes[t.index()].drop else {
            return Ok(());
        };
        let Some(tag) = self.fixed(TypeId(t.0)).tag else {
            return Ok(());
        };
        if drop.finalizer.is_some() || self.module.types[t.index()].repr == Repr::Resource {
            return Ok(());
        }
        let rows: Vec<Vec<hirsel_core_layout::FieldDrop>> = drop.fields.clone();
        let flat: Vec<usize> = (0..rows.len())
            .filter(|&r| rows[r].iter().all(|f| self.flat(&f.ty)))
            .collect();
        if flat.is_empty() {
            return Ok(());
        }
        let deep = Label::new("deep", n);
        let arms: Vec<_> = flat
            .iter()
            .map(|&r| {
                (
                    r as i128,
                    Label {
                        name: "flat",
                        n: n as u32,
                        edge: r as u32,
                    },
                )
            })
            .collect();
        let which = self.case_value(&tag, node);
        self.effect(Operation::Switch(&Type::I32, &which, deep, &arms));
        let status = self.temp(Type::I8);
        for &r in &flat {
            self.block(Label {
                name: "flat",
                n: n as u32,
                edge: r as u32,
            });
            for f in &rows[r] {
                let at = self.offset(node, f.offset);
                self.clean_field(&f.ty, &at, &status)?;
            }
            self.free_block(node, &int(charge));
            self.br(Label::plain("resume"));
        }
        self.block(deep);
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
fn counted_glue(
    module: &Module,
    target: Target,
    recipes: &[hirsel_core_layout::Recipe],
    failures: &failure::Failures,
    facts: bool,
    demanded: &BTreeSet<usize>,
    environments: &BTreeSet<usize>,
    sink: &mut dyn Sink,
    items: &mut Items,
) -> Result<()> {
    let empty = Body::default();
    for &t in demanded {
        let logical = held_logical(module, recipes, t);
        let drop_name = format!("core_counted_drop_{t}");
        let frames = hirsel_core_layout::frames(&module.types, recipes, TypeRef(t as u32));
        let owners = [
            ("own", hirsel_core_layout::charge::boxed(logical), 0),
            (
                "counted",
                hirsel_core_layout::charge::counted(logical),
                memory_layout::NODE_PAYLOAD as u64,
            ),
            // The captures start after the descriptor word.
            ("env", hirsel_core_layout::charge::closure_env(logical), 8),
        ];
        let owners = if environments.contains(&t) {
            &owners[..]
        } else {
            &owners[..2]
        };
        for &(owner, charge, offset) in owners {
            let charge = charge.unwrap();
            // The descriptor, then its points inline (abi_layout::fixed_point).
            let mut fields = vec![
                (Type::Ptr, Value::global(drop_name.clone())),
                (Type::I64, int(charge)),
                (Type::I64, int(offset)),
            ];
            for (k, p) in (1..).zip(&frames.points) {
                fields.extend(
                    [
                        memory_layout::fixed_point(k) as u64,
                        if p.last {
                            memory_layout::FIXED_POINT_DONE
                        } else {
                            p.cursor
                        },
                        p.a,
                        p.b,
                        p.index.unwrap_or(memory_layout::FIXED_POINT_NO_INDEX),
                    ]
                    .map(|w| (Type::I64, int(w))),
                );
            }
            let ty = Type::structure(fields.iter().map(|(t, _)| t.clone()).collect::<Vec<_>>());
            sink.global(&Global {
                name: format!("core_fixed_{owner}_{t}").into(),
                linkage: Linkage::Private,
                constant: true,
                externally_initialized: false,
                ty,
                init: Some(Value::Struct {
                    packed: false,
                    fields,
                }),
                align: 8,
                absolute_symbol: false,
            });
        }
        sink.begin_function(
            &drop_name,
            &step_signature(),
            &STEP_PARAMS,
            &FnAttrs::private(),
        );
        let mut e = glue_emitter(
            module,
            recipes,
            failures,
            facts,
            &empty,
            abi::Return::on_target(target, &[]),
            false,
            int(0),
            &mut *sink,
            &mut *items,
        );
        let field = FieldTy::Inline(TypeRef(t as u32));
        let cursor_at = Value::Local(Local::CURSOR);
        e.release_site();
        e.br(Label::plain("resume"));
        e.block(Label::plain("resume"));
        let cursor = e.load(&Type::I64, &cursor_at, 0);
        let next = e.bin(Binary::Add, &Type::I64, &cursor, &int(1));
        e.store(&Type::I64, &next, &cursor_at, 0);
        e.fixed_point = Some(Vec::new());
        e.fixed_rank = Some(0);
        e.mechanical_field(&field, &Local::PAYLOAD.into(), &cursor, 0)?;
        let staged = e.fixed_point.take().unwrap_or_default();
        debug_assert_eq!(
            staged.len(),
            frames.points.len(),
            "glue and frames number suspension points alike"
        );
        for (cursor, point) in staged.iter().zip(&frames.points) {
            debug_assert!(
                cursor.map_or(point.index.is_some(), |c| c == point.cursor
                    && point.index.is_none()),
                "glue and frames count steps alike: t{t} stages at {cursor:?}, frames say {point:?}"
            );
        }
        e.ret(&Type::I1, &true.into());
        drop(e);
        sink.end_function();
    }
    Ok(())
}

/// The logical bytes a box or counted block of type `t` holds: a resource's
/// block holds its pointer.
fn held_logical(module: &Module, recipes: &[hirsel_core_layout::Recipe], t: usize) -> u64 {
    if module.types[t].repr == Repr::Resource {
        8
    } else {
        recipes[t].logical_size.unwrap()
    }
}

impl FunctionEmitter<'_> {
    /// The one ordinary release of `resource`, of resource type `t`: its
    /// recipe runs on a drain of its own (LANG-19). Returns the failure byte.
    fn release_resource(&mut self, t: usize, resource: &Value) -> Value {
        let storage = self.recipes[t].logical_size.expect("resource storage");
        let site = self.site_word();
        self.runtime(
            "hirsel_core_resource_release",
            Type::I8,
            &[
                ptr(CONTEXT),
                ptr(resource.clone()),
                ptr(Value::global(format!("core_resource_{t}"))),
                word(int(storage)),
                word(site),
            ],
            &CallAttrs::default(),
        )
    }
}

/// A resource's recipe over its storage (`core_resource_T`): step 0 runs the
/// test trace hook and its finalizer under its grant, then its fields are
/// cleaned resumably; its frame is the resource's native state (LANG-19).
#[allow(clippy::too_many_arguments)]
fn resource_glue(
    module: &Module,
    target: Target,
    recipes: &[hirsel_core_layout::Recipe],
    failures: &failure::Failures,
    facts: bool,
    test_hooks: bool,
    resources: &BTreeSet<usize>,
    sink: &mut dyn Sink,
    items: &mut Items,
) -> Result<()> {
    let empty = Body::default();
    let i64 = Type::I64;
    for &t in resources {
        sink.begin_function(
            &format!("core_resource_{t}"),
            &step_signature(),
            &STEP_PARAMS,
            &FnAttrs::private(),
        );
        let mut e = glue_emitter(
            module,
            recipes,
            failures,
            facts,
            &empty,
            abi::Return::on_target(target, &[]),
            false,
            int(0),
            &mut *sink,
            &mut *items,
        );
        e.test_hooks = test_hooks;
        let recipe = recipes[t].drop.as_ref().expect("a resource drops");
        let base = Value::Local(Local::PAYLOAD);
        let cursor_at = Value::Local(Local::CURSOR);
        e.release_site();
        e.br(Label::plain("resume"));
        e.block(Label::plain("resume"));
        let cursor = e.load(&i64, &cursor_at, 0);
        let next = e.bin(Binary::Add, &i64, &cursor, &int(1));
        e.store(&i64, &next, &cursor_at, 0);
        let n = e.counter();
        let begin = Label::new("begin", n);
        let fields = Label::new("fields", n);
        let now = e.cmp(Predicate::Eq, &i64, &cursor, &int(0));
        e.cond(&now, begin, fields);
        e.block(begin);
        if test_hooks
            && let hirsel_core_layout::Body::Record(row) = &module.types[t].body
            && let Some(i) = row
                .iter()
                .position(|f| f.ty == FieldTy::Scalar(Scalar::U64))
        {
            let offset = e.fixed(TypeId(t as u32)).offsets[0][i];
            let field = e.offset(&base, offset);
            let id = e.load(&i64, &field, 1);
            e.runtime_void("hirsel_core_test_drop_trace", &[word(id)]);
        }
        let list = Value::Local(Local::WORKLIST);
        let failed = e.offset(&list, memory_layout::WORKLIST_FAILED as u64);
        e.finalize(t, &base, &failed);
        e.br(Label::plain("resume"));
        e.block(fields);
        let row_label = |row: usize| Label {
            name: "row",
            n: n as u32,
            edge: row as u32,
        };
        if let Some(tag) = e.fixed(TypeId(t as u32)).tag {
            let which = e.case_value(&tag, &base);
            let arms: Vec<_> = (1..recipe.fields.len())
                .map(|row| (row as i128, row_label(row)))
                .collect();
            e.effect(Operation::Switch(&Type::I32, &which, row_label(0), &arms));
        } else {
            e.br(row_label(0));
        }
        for (row, fs) in recipe.fields.iter().enumerate() {
            e.block(row_label(row));
            let mut offset = 1;
            for f in fs {
                let at = e.offset(&base, f.offset);
                e.mechanical_field(&f.ty, &at, &cursor, offset)?;
                offset += mechanical_count(module, recipes, &f.ty)?;
            }
            e.ret(&Type::I1, &true.into());
        }
        drop(e);
        sink.end_function();
    }
    Ok(())
}

/// A held value's cleanup in place (`core_clean_T`), resumable as a drain
/// recipe with no frame of its own: its holder keeps the cursor. Finalizers
/// inherit no site.
#[allow(clippy::too_many_arguments)]
fn value_glue(
    module: &Module,
    target: Target,
    recipes: &[hirsel_core_layout::Recipe],
    failures: &failure::Failures,
    facts: bool,
    values: &BTreeSet<usize>,
    sink: &mut dyn Sink,
    items: &mut Items,
) -> Result<()> {
    let empty = Body::default();
    for &t in values {
        sink.begin_function(
            &format!("core_clean_{t}"),
            &step_signature(),
            &STEP_PARAMS,
            &FnAttrs::private(),
        );
        let mut e = glue_emitter(
            module,
            recipes,
            failures,
            facts,
            &empty,
            abi::Return::on_target(target, &[]),
            false,
            int(0),
            &mut *sink,
            &mut *items,
        );
        let field = FieldTy::Inline(TypeRef(t as u32));
        let cursor_at = Value::Local(Local::CURSOR);
        e.br(Label::plain("resume"));
        e.block(Label::plain("resume"));
        let cursor = e.load(&Type::I64, &cursor_at, 0);
        let next = e.bin(Binary::Add, &Type::I64, &cursor, &int(1));
        e.store(&Type::I64, &next, &cursor_at, 0);
        e.mechanical_field(&field, &Local::PAYLOAD.into(), &cursor, 0)?;
        e.ret(&Type::I1, &true.into());
        drop(e);
        sink.end_function();
    }
    Ok(())
}
