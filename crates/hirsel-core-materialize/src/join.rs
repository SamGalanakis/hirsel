//! Resolve unit-local tables before copying bodies, as a native linker does.
use hirsel_core_ir::*;
use rustc_hash::FxHashMap as HashMap;
use std::borrow::Cow;

/// One reached instance's emission unit. Generated bodies belong to this unit too.
#[derive(Clone, Copy)]
pub struct Unit<'a> {
    pub identity: &'a Identity,
    pub module: &'a Module,
    pub function: FuncId,
}

/// Native and IR links give code variants their code symbol, and other rows their
/// semantic instance symbol. The caller may memoize the semantic name.
pub fn link_symbol(module: &Module, function: FuncId, semantic: impl FnOnce() -> String) -> String {
    module
        .code_variants
        .get(&function)
        .map_or_else(semantic, |&code| symbol(code))
}

#[derive(Default)]
struct Remap {
    types: Vec<TypeId>,
    fresh_types: Vec<usize>,
    signatures: Vec<SigId>,
    functions: Vec<FuncId>,
    statics: Vec<StaticId>,
    items: Vec<ItemId>,
}
impl Remap {
    fn place(&self, ty: &mut PlaceTy) {
        if let PlaceTy::Declared(t) = ty {
            *t = self.types[t.index()];
        }
    }
    fn signature(&self, signature: &mut Signature) {
        for p in &mut signature.params {
            if let Param::Address { place, .. } = p {
                self.place(place);
            }
        }
        signature.dest = signature.dest.map(|t| self.types[t.index()]);
    }
    fn anchor(&self, anchor: &mut Option<Anchor>) {
        if let Some(a) = anchor {
            a.item = self.items[a.item.index()];
        }
    }
    fn body(&self, body: &mut Body) {
        for ty in &mut body.slots {
            self.place(ty);
        }
        for address in &mut body.addresses {
            self.place(&mut address.ty);
        }
        for block in &mut body.blocks {
            self.anchor(&mut block.anchor);
        }
        for anchor in &mut body.inst_anchors {
            self.anchor(anchor);
        }
        for inst in &mut body.insts {
            match inst {
                Inst::LayoutConst { query, .. } => {
                    query.map_types(|t| TypeRef(self.types[t.index()].0));
                }
                Inst::ReadStatic { data, .. } => *data = self.statics[data.index()],
                Inst::FunctionRef { function, .. } => *function = self.functions[function.index()],
                Inst::DescriptorRef {
                    function, identity, ..
                } => {
                    *function = self.functions[function.index()];
                    *identity = self.statics[identity.index()];
                }
                Inst::Drop { ty, .. }
                | Inst::Release { ty, .. }
                | Inst::CompactWidth { ty, .. } => *ty = self.types[ty.index()],
                Inst::Mark { ty, .. } | Inst::Unmark { ty, .. } => self.place(ty),
                Inst::Alloc {
                    kind: AllocKind::Own { edge },
                    ..
                } => {
                    edge.map_refs(&mut |r| TypeRef(self.types[r.index()].0));
                }
                Inst::Alloc {
                    kind: AllocKind::Environment { ty },
                    ..
                } => *ty = TypeRef(self.types[ty.index()].0),
                _ => {}
            }
        }
        for root in &mut body.compact_roots {
            root.ty = self.types[root.ty.index()];
        }
        for call in &mut body.calls {
            call.signature = self.signatures[call.signature.index()];
            match &mut call.callee {
                Callee::Direct(f) => *f = self.functions[f.index()],
                Callee::Runtime(op) => match op {
                    RtOp::Spawn { x, r } | RtOp::SpawnUnit { x, r } => {
                        *x = self.types[x.index()];
                        *r = self.types[r.index()];
                    }
                    RtOp::Join { r } => *r = self.types[r.index()],
                    RtOp::LaunchTarget { view } => *view = self.types[view.index()],
                    RtOp::ChannelNew { item }
                    | RtOp::ConnNew { item }
                    | RtOp::ConnSend { item }
                    | RtOp::ConnTrySend { item }
                    | RtOp::ConnReceive { item }
                    | RtOp::ConnTryReceive { item }
                    | RtOp::Send { item }
                    | RtOp::TrySend { item }
                    | RtOp::Receive { item }
                    | RtOp::TryReceive { item }
                    | RtOp::LatestNew { item }
                    | RtOp::SharedNew { item }
                    | RtOp::SharedRelease { item } => {
                        *item = self.types[item.index()];
                    }
                    _ => {}
                },
                Callee::Indirect(_) => {}
            }
        }
    }
}

/// Join the same units native engines emit. Signature-only rows resolve by code
/// symbol; conflicting definitions of one symbol are a link error. Body-local
/// values, blocks, slots, addresses and pool ranges retain their numbering.
pub fn join(units: &[Unit<'_>], entry: &Identity) -> Result<(Module, FuncId), String> {
    join_units(
        units
            .iter()
            .map(|unit| (unit.identity, Cow::Borrowed(unit.module), unit.function))
            .collect(),
        entry,
    )
}

/// Join units whose owned bodies can be remapped in place.
pub fn join_units(
    units: Vec<(&Identity, Cow<'_, Module>, FuncId)>,
    entry: &Identity,
) -> Result<(Module, FuncId), String> {
    let mut out = Module::default();
    let mut types = HashMap::default();
    let mut signatures = HashMap::default();
    let mut instances = HashMap::default();
    let mut functions = HashMap::default();
    let mut statics = HashMap::default();
    let mut aggregates = HashMap::default();
    let mut computed = HashMap::default();
    let mut items = HashMap::default();
    let mut remaps = Vec::with_capacity(units.len());
    let mut root = None;
    for (identity, module, root_function) in &units {
        let mut map = Remap::default();
        for (i, shape) in module.types.iter().enumerate() {
            let (row, fresh) = intern_shape(
                &mut out.types,
                &mut types,
                module.closure(TypeId(i as u32)).hash,
                Row::clone(shape),
            );
            map.types.push(TypeId(row.0));
            if fresh {
                map.fresh_types.push(i);
            }
        }
        for signature in &module.signatures {
            let mut signature = signature.clone();
            map.signature(&mut signature);
            let id = *signatures.entry(signature.clone()).or_insert_with(|| {
                let id = SigId(out.signatures.len() as u32);
                out.signatures.push(signature);
                id
            });
            map.signatures.push(id);
        }
        let instance_ids: Vec<_> = module
            .instances
            .iter()
            .map(|key| {
                let mut key = key.clone();
                key.arguments
                    .iter_mut()
                    .for_each(|t| *t = map.types[t.index()]);
                *instances.entry(key.clone()).or_insert_with(|| {
                    let id = InstanceId(out.instances.len() as u32);
                    out.instances.push(key);
                    id
                })
            })
            .collect();
        for (i, function) in module.functions.iter().enumerate() {
            let f = FuncId(i as u32);
            let name = link_symbol(module, f, || {
                if f == *root_function {
                    identity.symbol()
                } else {
                    module.identity(function.instance).symbol()
                }
            });
            let id = *functions.entry(name).or_insert_with(|| {
                let id = FuncId(out.functions.len() as u32);
                out.functions.push(Function {
                    versioned: function.versioned,
                    cold: function.cold,
                    instance: instance_ids[function.instance.index()],
                    signature: map.signatures[function.signature.index()],
                    body: None,
                });
                if let Some(&code) = module.code_variants.get(&f) {
                    out.code_variants.insert(id, code);
                }
                id
            });
            map.functions.push(id);
        }
        for data in &module.statics {
            let mut append = |data| {
                let id = StaticId(out.statics.len() as u32);
                out.statics.push(data);
                id
            };
            let id = match data {
                Static::Bytes { align, bytes } => {
                    let id = *statics
                        .entry(bytes.clone())
                        .or_insert_with(|| append(data.clone()));
                    let Static::Bytes { align: held, .. } = &mut out.statics[id.index()] else {
                        unreachable!()
                    };
                    *held = (*held).max(*align);
                    id
                }
                Static::Aggregate { ty, value } => {
                    let ty = map.types[ty.index()];
                    *aggregates.entry((ty, value.clone())).or_insert_with(|| {
                        append(Static::Aggregate {
                            ty,
                            value: value.clone(),
                        })
                    })
                }
                // One slot per constant: every reader binds the same value.
                Static::Computed { ty, constant } => {
                    let mut ty = ty.clone();
                    ty.map_refs(&mut |r| TypeRef(map.types[r.index()].0));
                    *computed.entry(constant.clone()).or_insert_with(|| {
                        append(Static::Computed {
                            ty,
                            constant: constant.clone(),
                        })
                    })
                }
            };
            map.statics.push(id);
        }
        for item in &module.items {
            let id = *items.entry(item.clone()).or_insert_with(|| {
                let id = ItemId(out.items.len() as u32);
                out.items.push(item.clone());
                id
            });
            map.items.push(id);
        }
        if *identity == entry {
            root = Some(map.functions[root_function.index()]);
        }
        remaps.push(map);
    }
    for ((_, mut module, _), map) in units.into_iter().zip(remaps) {
        for &i in &map.fresh_types {
            if !module.types[i].numbered() {
                continue;
            }
            let mut shape = Shape::clone(&module.types[i]);
            shape.map_refs(&mut |r| TypeRef(map.types[r.index()].0));
            for hook in [
                &mut shape.finalizer,
                &mut shape.clone,
                &mut shape.codec,
                &mut shape.key_codec,
            ] {
                *hook = hook.map(|f| FuncRef(map.functions[f.0 as usize].0));
            }
            out.types[map.types[i].index()] = Row::new(shape);
        }
        for i in 0..module.functions.len() {
            let body = match &mut module {
                Cow::Borrowed(module) => module.functions[i].body.clone(),
                Cow::Owned(module) => module.functions[i].body.take(),
            };
            let Some(mut body) = body else { continue };
            let function = &module.functions[i];
            map.body(&mut body);
            let row = &mut out.functions[map.functions[i].index()];
            if let Some(prior) = &row.body {
                if *prior != body || row.signature != map.signatures[function.signature.index()] {
                    return Err(format!(
                        "conflicting code identity for {}",
                        module.render_instance(function.instance)
                    ));
                }
            } else {
                row.signature = map.signatures[function.signature.index()];
                row.versioned = function.versioned;
                row.body = Some(body);
            }
        }
    }
    Ok((out, root.ok_or("entry has no reached unit")?))
}
