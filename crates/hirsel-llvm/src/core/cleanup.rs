//! Recipe cleanup and resumable counted destruction.
use super::storage::zeroext;
use super::*;
use hirsel_core_layout::{FieldTy, Repr, TypeRef, field_size};

fn cleanup_dependencies(field: &FieldTy, counted: bool, pending: &mut Vec<(usize, bool)>) {
    match field {
        FieldTy::Inline(t) | FieldTy::Box(t) => pending.push((t.index(), counted)),
        FieldTy::Rc(t) | FieldTy::Arc(t) => pending.push((t.index(), true)),
        FieldTy::Map(key, value) => {
            cleanup_dependencies(key, counted, pending);
            cleanup_dependencies(value, counted, pending);
        }
        FieldTy::Array(element, _) | FieldTy::Seq(element) => {
            cleanup_dependencies(element, counted, pending)
        }
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
        facts,
        aliases: None,
        caller_site,
        debug_location: None,
        anchor: None,
        relocatable_sites: false,
        value_slots: Vec::new(),
        address_slots: Vec::new(),
        empty_bounds: empty::EmptyBounds::default(),
        sink,
        items,
        next: 0,
    }
}

/// `i8 core_drop_N(ptr context, ptr payload, i64 caller_site)`.
pub(super) fn drop_signature() -> FnSig {
    FnSig::plain(Type::I8, [Type::Ptr, Type::Ptr, Type::I64])
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
pub(super) fn drop_glue(
    module: &Module,
    target: Target,
    recipes: &[hirsel_core_layout::Recipe],
    failures: &failure::Failures,
    facts: bool,
    sink: &mut dyn Sink,
    items: &mut Items,
) -> Result<()> {
    let mut normal = BTreeSet::new();
    let mut counted = BTreeSet::new();
    let mut callbacks = BTreeSet::new();
    let mut pending = Vec::new();
    for body in module.functions.iter().filter_map(|f| f.body.as_ref()) {
        for inst in &body.insts {
            match inst {
                Inst::Drop { ty, .. } => pending.push((ty.index(), false)),
                Inst::Release { ty, .. } => pending.push((ty.index(), true)),
                Inst::Call { call } => match body.calls[call.index()].callee {
                    Callee::Runtime(RtOp::Spawn { x, r }) => {
                        callbacks.insert(r.index());
                        pending.push((x.index(), false));
                        pending.push((r.index(), false));
                    }
                    Callee::Runtime(RtOp::ChannelNew { item }) => {
                        callbacks.insert(item.index());
                        pending.push((item.index(), false))
                    }
                    _ => {}
                },
                _ => {}
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
            // FORM v1c: counted cleanup runs a row with a finalizer whole, by its
            // ordinary glue.
            if mechanical && drop.finalizer.is_some() {
                pending.push((t, false));
                continue;
            }
            for field in drop.fields.iter().flatten() {
                cleanup_dependencies(&field.ty, mechanical, &mut pending);
            }
        }
    }
    let empty = Body::default();
    let payload = Value::Local(Local::PAYLOAD);
    for t in normal {
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
            let status = e.temp(Type::I8);
            e.store(&Type::I8, &int(0), &status, 0);
            let resource = module.types[t].repr == Repr::Resource;
            let base = if resource {
                e.load(&Type::Ptr, &payload, 1)
            } else {
                payload.clone()
            };
            #[cfg(test)]
            if resource
                && let hirsel_core_layout::Body::Record(fields) = &module.types[t].body
                && let Some(i) = fields
                    .iter()
                    .position(|f| f.ty == FieldTy::Scalar(Scalar::U64))
            {
                let offset = e.fixed(TypeId(t as u32)).offsets[0][i];
                let field = e.offset(&base, offset);
                let id = e.load(&Type::I64, &field, 1);
                e.runtime_void("hirsel_core_test_drop_trace", &[word(id)]);
            }
            // A resource's finalizer runs under its grant; a core slot kernel's on an
            // own row runs under the current limit (FORM v1c).
            if let Some(f) = module.types[t].finalizer {
                let finalizer_site = e.call_site_word(Callee::Direct(FuncId(f.0)));
                if resource {
                    e.runtime_void(
                        "hirsel_core_finalizer_begin",
                        &[ptr(CONTEXT), ptr(base.clone())],
                    );
                }
                let symbol = symbol(module, FuncId(f.0));
                let args = [CONTEXT, base.clone(), finalizer_site];
                if failures.may_fail[f.0 as usize] {
                    let failed = e.call(
                        &drop_signature(),
                        &Value::global(symbol),
                        &args,
                        &CallAttrs::default(),
                    );
                    e.store(&Type::I8, &failed, &status, 0);
                } else {
                    e.call(
                        &FnSig::plain(Type::Void, [Type::Ptr, Type::Ptr, Type::I64]),
                        &Value::global(format!("{symbol}.infallible")),
                        &args,
                        &CallAttrs::default(),
                    );
                }
                if resource {
                    e.runtime_void(
                        "hirsel_core_finalizer_end",
                        &[ptr(CONTEXT), ptr(base.clone())],
                    );
                }
            }
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
            if resource {
                let storage = recipes[t].logical_size.expect("resource storage");
                e.runtime_void(
                    "hirsel_core_resource_free",
                    &[ptr(base), word(int(storage))],
                );
            }
            let failed = e.load(&Type::I8, &status, 0);
            e.ret(&Type::I8, &failed);
        }
        sink.end_function();
        if callbacks.contains(&t) {
            sink.begin_function(
                &format!("core_drop_c_{t}"),
                &FnSig::plain(Type::I8, [Type::Ptr, Type::Ptr]),
                &[Local::CONTEXT, Local::PAYLOAD],
                &FnAttrs::private(),
            );
            let r = Local::Named("r");
            sink.instruction(
                Some(r),
                Operation::Call(
                    &drop_signature(),
                    &Value::global(drop_name),
                    &[CONTEXT, payload.clone(), int(0)],
                    &CallAttrs::default(),
                ),
                None,
            );
            sink.instruction(None, Operation::Return(Some((&Type::I8, &r.into()))), None);
            sink.end_function();
        }
    }
    let mut sequences = Vec::new();
    for &t in &counted {
        if let Some(drop) = &recipes[t].drop {
            for field in drop.fields.iter().flatten() {
                collections::collect(&field.ty, &mut sequences);
            }
        }
    }
    counted_glue(
        module, target, recipes, failures, facts, &counted, sink, items,
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
            FieldTy::Box(t) => {
                let block = self.load(&Type::Ptr, p, 1);
                let failed = self.call(
                    &drop_signature(),
                    &Value::global(format!("core_drop_{}", t.0)),
                    &[CONTEXT, block.clone(), site],
                    &CallAttrs::default(),
                );
                let logical = if self.module.types[t.index()].repr == Repr::Resource {
                    8
                } else {
                    self.recipes[t.index()].logical_size.unwrap()
                };
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
            FieldTy::Seq(_) | FieldTy::Map(_, _) => {
                self.clean_collection(field, p, status)?;
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
                let glue = self.load(&Type::Ptr, &environment, 1);
                self.effect(Operation::Call(
                    &drop_signature(),
                    &glue,
                    &[CONTEXT, environment, site],
                    &CallAttrs::default(),
                ));
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
            | FieldTy::Stable(_)
            | FieldTy::Fn
            | FieldTy::Borrow
            | FieldTy::View(_)
            | FieldTy::Locator(_) => None,
            FieldTy::Handle(hirsel_core_layout::Handle::HostFile) => {
                let handle = self.load(&Type::I64, p, 1);
                self.runtime_void("hirsel_core_hostfile_drop", &[ptr(CONTEXT), word(handle)]);
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
        let logical = self.recipes[t.index()].logical_size.unwrap();
        let charge = hirsel_core_layout::charge::counted(logical).unwrap();
        let edge = if atomic {
            FieldTy::Arc(t)
        } else {
            FieldTy::Rc(t)
        };
        let recipe = (!hirsel_core_layout::terminal(&self.module.types, self.recipes, &edge))
            .then(|| Value::global(format!("core_static_counted_{}", t.0)));
        self.release_fast(node, charge, recipe.as_ref(), atomic);
        Ok(())
    }
}

pub(super) fn mechanical_count(
    module: &Module,
    recipes: &[hirsel_core_layout::Recipe],
    field: &FieldTy,
) -> Result<u64> {
    Ok(match field {
        FieldTy::Box(_)
        | FieldTy::Rc(_)
        | FieldTy::Arc(_)
        | FieldTy::Seq(_)
        | FieldTy::Map(_, _) => 1,
        FieldTy::Inline(t) => {
            let Some(drop) = &recipes[t.index()].drop else {
                return Ok(0);
            };
            // FORM v1c: a row with a finalizer is one step, cleaned whole.
            if drop.finalizer.is_some() {
                return Ok(1);
            }
            let mut max = 0;
            for row in &drop.fields {
                let mut n = 0;
                for f in row {
                    n += mechanical_count(module, recipes, &f.ty)?;
                }
                max = max.max(n);
            }
            max
        }
        FieldTy::Array(e, n) => mechanical_count(module, recipes, e)? * n,
        FieldTy::Scalar(_) | FieldTy::Fn | FieldTy::Stable(_) => 0,
        other => {
            return Err(Error::Unsupported(format!(
                "counted payload field {other:?}"
            )));
        }
    })
}
impl FunctionEmitter<'_> {
    pub(super) fn mechanical_field(
        &mut self,
        field: &FieldTy,
        p: &Value,
        cursor: &Value,
        base: u64,
    ) -> Result<()> {
        let i64 = Type::I64;
        let site = self.site_word();
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
            // Its ordinary glue: the finalizer, then the fields. A counted payload
            // holds no resource, so this cannot fail.
            FieldTy::Inline(t)
                if self.recipes[t.index()]
                    .drop
                    .as_ref()
                    .is_some_and(|d| d.finalizer.is_some()) =>
            {
                self.effect(Operation::Call(
                    &drop_signature(),
                    &Value::global(format!("core_drop_{}", t.0)),
                    &[CONTEXT, p.clone(), site],
                    &CallAttrs::default(),
                ));
                self.ret(&Type::I1, &FALSE);
            }
            FieldTy::Inline(t) => {
                let drop = self.recipes[t.index()].drop.as_ref().unwrap();
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
                for (row, fields) in drop.fields.iter().enumerate() {
                    self.block(row_label(row));
                    let mut offset = 0;
                    for f in fields {
                        let at = self.offset(p, f.offset);
                        self.mechanical_field(&f.ty, &at, &local, offset)?;
                        offset += mechanical_count(self.module, self.recipes, &f.ty)?;
                    }
                    self.ret(&Type::I1, &FALSE);
                }
            }
            FieldTy::Array(e, _) => {
                let per = mechanical_count(self.module, self.recipes, e)?;
                let index = self.bin(Binary::UDiv, &i64, &local, &int(per));
                let inner = self.bin(Binary::URem, &i64, &local, &int(per));
                let stride = field_size(&self.module.types, self.recipes, e).0;
                let off = self.bin(Binary::Mul, &i64, &index, &int(stride));
                let at = self.offset_by(p, &off);
                self.mechanical_field(e, &at, &inner, 0)?;
                self.ret(&Type::I1, &FALSE);
            }
            FieldTy::Seq(_) | FieldTy::Map(_, _) => self.enqueue_collection(field, p),
            FieldTy::Box(t) | FieldTy::Rc(t) | FieldTy::Arc(t) => {
                let node = self.load(&Type::Ptr, p, 1);
                let logical = self.recipes[t.index()].logical_size.unwrap();
                let (name, charge) = if matches!(field, FieldTy::Box(_)) {
                    (
                        "hirsel_core_release_static_own_enqueue",
                        hirsel_core_layout::charge::boxed(logical).unwrap(),
                    )
                } else {
                    (
                        "hirsel_core_release_static_enqueue",
                        hirsel_core_layout::charge::counted(logical).unwrap(),
                    )
                };
                if hirsel_core_layout::terminal(&self.module.types, self.recipes, field) {
                    if matches!(field, FieldTy::Box(_)) {
                        self.free_block(&node, &int(charge));
                    } else {
                        self.release_node(&node, *t, matches!(field, FieldTy::Arc(_)))?;
                    }
                    self.ret(&Type::I1, &FALSE);
                    self.block(skip);
                    return Ok(());
                }
                let mut args = vec![ptr(Local::WORKLIST), ptr(node)];
                if !matches!(field, FieldTy::Box(_)) {
                    args.push(zeroext(Type::I1, matches!(field, FieldTy::Arc(_))));
                }
                let owner = if matches!(field, FieldTy::Box(_)) {
                    "own"
                } else {
                    "counted"
                };
                args.push(ptr(Value::global(format!("core_static_{owner}_{}", t.0))));
                self.runtime_void(name, &args);
                self.ret(&Type::I1, &FALSE);
            }
            _ => unreachable!(),
        }
        self.block(skip);
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
    sink: &mut dyn Sink,
    items: &mut Items,
) -> Result<()> {
    let empty = Body::default();
    for &t in demanded {
        if module.types[t].repr == Repr::Resource {
            continue;
        }
        let logical = recipes[t].logical_size.unwrap();
        let drop_name = format!("core_counted_drop_{t}");
        for (owner, charge, offset) in [
            (
                "own",
                hirsel_core_layout::charge::boxed(logical).unwrap(),
                0,
            ),
            (
                "counted",
                hirsel_core_layout::charge::counted(logical).unwrap(),
                memory_layout::NODE_PAYLOAD as u64,
            ),
        ] {
            let ty = Type::structure([Type::Ptr, Type::I64, Type::I64]);
            sink.global(&Global {
                name: format!("core_static_{owner}_{t}").into(),
                linkage: Linkage::Private,
                constant: true,
                externally_initialized: false,
                ty,
                init: Some(Value::Struct {
                    packed: false,
                    fields: vec![
                        (Type::Ptr, Value::global(drop_name.clone())),
                        (Type::I64, int(charge)),
                        (Type::I64, int(offset)),
                    ],
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
