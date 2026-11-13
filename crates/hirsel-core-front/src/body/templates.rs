//! Mechanical specialization of a body checked once with rigid type parameters.
//! Rigid values use places while checking; instances use their concrete convention.
//! Layout rows and size constants are substituted, then scalar addresses become
//! values. Source is never opened here.
use super::*;
use hirsel_core_layout::{Body as ShapeBody, FieldTy, FuncRef, Shape};
use std::collections::BTreeMap;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct CopySize {
    pub value: ValueId,
    pub ty: TypeId,
    pub logical: bool,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct AllocationLayout {
    pub result: ValueId,
    pub ty: TypeId,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct GenericTemplate {
    pub signatures: BTreeMap<SigId, scalar_abi::SignatureAbi>,
    pub parameters: Vec<Option<TypeId>>,
    pub copies: Vec<CopySize>,
    pub allocations: Vec<AllocationLayout>,
    pub numeric_sites: Vec<scalar_abi::NumericSite>,
    pub nested: BTreeMap<FuncId, GenericTemplate>,
    pub types: BTreeMap<TypeId, NameRecipe>,
    pub calls: BTreeMap<FuncId, GenericCall>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum NamePart {
    Text(String),
    Parameter(u32),
}
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct NameRecipe(pub Vec<NamePart>);
impl NameRecipe {
    fn render(&self, arguments: &[String]) -> String {
        let mut result = String::new();
        for part in &self.0 {
            match part {
                NamePart::Text(text) => result.push_str(text),
                NamePart::Parameter(i) => result.push_str(&arguments[*i as usize]),
            }
        }
        result
    }
    pub(super) fn dictionary(bound: &str, argument: &Self) -> Self {
        let mut recipe = Self::default();
        recipe.text(format!("{bound}["));
        recipe.0.extend(argument.0.iter().cloned());
        recipe.text("]");
        recipe
    }
    fn text(&mut self, text: impl Into<String>) {
        self.0.push(NamePart::Text(text.into()));
    }
    fn ty(&mut self, p: &Program, t: &impl TyRead, ty: TyId) {
        if !has_param(t, ty) {
            self.text(crate::decls::canonical(&p.decls, t, ty));
            return;
        }
        match t.kind(ty) {
            TyKind::Param(i) => self.0.push(NamePart::Parameter(*i)),
            TyKind::Tuple(xs) => {
                self.text("(");
                for (i, &x) in xs.iter().enumerate() {
                    if i > 0 {
                        self.text(", ");
                    }
                    self.ty(p, t, x);
                }
                if xs.len() == 1 {
                    self.text(",");
                }
                self.text(")");
            }
            TyKind::Array(x, n) => {
                self.text("Inline[");
                self.ty(p, t, *x);
                self.text(format!(", {n}]"));
            }
            TyKind::View(x) => {
                self.text("View[");
                self.ty(p, t, *x);
                self.text("]");
            }
            TyKind::Adt(d, xs) => {
                self.text(p.decls[d.index()].path.to_string());
                self.text("[");
                for (i, &x) in xs.iter().enumerate() {
                    if i > 0 {
                        self.text(", ");
                    }
                    self.ty(p, t, x);
                }
                self.text("]");
            }
            TyKind::Fn(ps, r, waits, kind) => {
                self.text(match kind {
                    Callable::Plain => "fn(",
                    Callable::Own => "own fn(",
                    Callable::OwnSend => "own[send] fn(",
                    Callable::Scoped => "scoped fn(",
                });
                for (i, &(mode, ty)) in ps.iter().enumerate() {
                    if i > 0 {
                        self.text(", ");
                    }
                    self.text(match mode {
                        Mode::View => "",
                        Mode::Mut => "mut ",
                        Mode::Take => "take ",
                        Mode::Bytes => "bytes ",
                    });
                    self.ty(p, t, ty);
                }
                self.text(") -> ");
                self.ty(p, t, *r);
                if *waits {
                    self.text(" waits");
                }
            }
            TyKind::Anchored(x, r) => {
                self.ty(p, t, *x);
                self.text(" ");
                self.ty(p, t, *r);
            }
            TyKind::Region(_) => self.text(crate::decls::canonical(&p.decls, t, ty)),
            _ => unreachable!("has_param identifies a structural type or a parameter"),
        }
    }
    pub(super) fn of(p: &Program, t: &impl TyRead, ty: TyId) -> Self {
        let mut name = Self::default();
        name.ty(p, t, ty);
        name
    }
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct GenericCall {
    pub operation: Option<key_glue::KeyOp>,
    pub arguments: Vec<NameRecipe>,
    pub dictionaries: Vec<NameRecipe>,
}

/// An instance's type arguments are shapes already published by their owner modules,
/// not source types. This is the only additional input specialization needs from its
/// caller. A resource argument's finalizer is named by its owner's function row and
/// imported by instance key (FAIL-11).
pub fn materialize(
    source: &Module,
    function: FuncId,
    template: &GenericTemplate,
    key: &InstanceKey,
    arguments: &[(&Module, TypeRef)],
) -> Result<(Module, FuncId), String> {
    crate::cost_scope!("instance", format!("{key:?}"));
    if arguments.len() != template.parameters.len() {
        return Err("the entry has undetermined generic type arguments".into());
    }
    let mut module = source.clone();
    let roots = arguments
        .iter()
        .map(|&(owner, root)| import_type(&mut module, owner, root, &mut HashMap::default()))
        .collect::<Vec<_>>();
    // A body that calls its own instance (a region-only generic's recursion erases to
    // the instance's own key, MEM-6) already holds a signature-only row of that key:
    // the body fills it, so the unit defines one function per link name.
    let signature = source.functions[function.index()].signature;
    let target = match module
        .functions
        .iter()
        .position(|g| g.body.is_none() && &module.instances[g.instance.index()] == key)
    {
        Some(row) => {
            module.functions[row].signature = signature;
            FuncId(row as u32)
        }
        None => {
            let instance = InstanceId(module.instances.len() as u32);
            module.instances.push(key.clone());
            module.functions.push(Function {
                instance,
                signature,
                body: None,
            });
            FuncId(module.functions.len() as u32 - 1)
        }
    };
    fill(&mut module, function, target, template, &roots)?;
    fn retire(module: &mut Module, source: FuncId, template: &GenericTemplate) {
        module.functions[source.index()].body = None;
        for (&nested, template) in &template.nested {
            retire(module, nested, template);
        }
    }
    retire(&mut module, function, template);
    // A shape at the template's rigid parameters (`$i`) is dead once substituted; its
    // finalizer's or clone body's rigid instance must not become a dependency of this
    // unit.
    let rigid = |module: &Module, f: Option<FuncRef>| {
        f.is_some_and(|f| {
            let key = &module.instances[module.functions[f.0 as usize].instance.index()];
            key.types.iter().any(|name| name.contains('$'))
        })
    };
    for t in 0..module.types.len() {
        if rigid(&module, module.types[t].finalizer) {
            module.types[t].finalizer = None;
        }
        if rigid(&module, module.types[t].clone) {
            module.types[t].clone = None;
        }
    }
    Ok((module, target))
}

fn import_type(
    module: &mut Module,
    owner: &Module,
    root: TypeRef,
    seen: &mut HashMap<TypeRef, TypeRef>,
) -> TypeId {
    if let Some(&row) = seen.get(&root) {
        return TypeId(row.0);
    }
    let shape = &owner.types[root.index()];
    if let Some(row) = module.types.iter().position(|s| s.name == shape.name) {
        return TypeId(row as u32);
    }
    let row = TypeRef(module.types.len() as u32);
    seen.insert(root, row);
    module.types.push(shape.clone());
    let mut shape = shape.clone();
    map_fields(&mut shape, &mut |id| {
        TypeRef(import_type(module, owner, id, seen).0)
    });
    shape.finalizer = shape
        .finalizer
        .map(|f| FuncRef(import_function(module, owner, FuncId(f.0)).0));
    shape.clone = shape
        .clone
        .map(|f| FuncRef(import_function(module, owner, FuncId(f.0)).0));
    module.types[row.index()] = shape;
    TypeId(row.0)
}

/// The row of `owner`'s function `f` in `module`: the row with its instance key, or a
/// new signature-only row the instance collector resolves as any other callee.
fn import_function(module: &mut Module, owner: &Module, f: FuncId) -> FuncId {
    let from = &owner.functions[f.index()];
    let key = &owner.instances[from.instance.index()];
    if let Some(row) = module
        .functions
        .iter()
        .position(|g| &module.instances[g.instance.index()] == key)
    {
        return FuncId(row as u32);
    }
    let signature = &owner.signatures[from.signature.index()];
    let signature = match module.signatures.iter().position(|s| s == signature) {
        Some(i) => SigId(i as u32),
        None => {
            module.signatures.push(signature.clone());
            SigId(module.signatures.len() as u32 - 1)
        }
    };
    let instance = InstanceId(module.instances.len() as u32);
    module.instances.push(key.clone());
    module.functions.push(Function {
        instance,
        signature,
        body: None,
    });
    FuncId(module.functions.len() as u32 - 1)
}

fn map_fields(shape: &mut Shape, f: &mut impl FnMut(TypeRef) -> TypeRef) {
    fn field(ty: &mut FieldTy, f: &mut impl FnMut(TypeRef) -> TypeRef) {
        match ty {
            FieldTy::Inline(id)
            | FieldTy::Box(id)
            | FieldTy::Rc(id)
            | FieldTy::Arc(id)
            | FieldTy::Stable(id)
            | FieldTy::Link(id) => *id = f(*id),
            FieldTy::Seq(ty)
            | FieldTy::Array(ty, _)
            | FieldTy::View(ty)
            | FieldTy::Locator(ty)
            | FieldTy::FixedSpan(ty)
            | FieldTy::SeqSpan(ty)
            | FieldTy::IndexedSpan(ty) => field(ty, f),
            FieldTy::Map(k, v) => {
                field(k, f);
                field(v, f);
            }
            _ => {}
        }
    }
    match &mut shape.body {
        ShapeBody::Record(fields) => {
            for x in fields {
                field(&mut x.ty, f);
            }
        }
        ShapeBody::Variant(cases) => {
            for c in cases {
                for x in &mut c.fields {
                    field(&mut x.ty, f);
                }
            }
        }
    }
}

struct Substitution<'a> {
    module: &'a mut Module,
    parameters: &'a [Option<TypeId>],
    arguments: &'a [TypeId],
    names: Vec<String>,
    type_names: &'a BTreeMap<TypeId, NameRecipe>,
    call_names: &'a BTreeMap<FuncId, GenericCall>,
    nested: &'a BTreeMap<FuncId, GenericTemplate>,
    error: Option<String>,
    types: HashMap<TypeId, TypeId>,
    functions: HashMap<FuncId, FuncId>,
    signatures: HashMap<SigId, SigId>,
    signature_abis: &'a BTreeMap<SigId, scalar_abi::SignatureAbi>,
    plans: HashMap<SigId, scalar_abi::Plan>,
}
impl Substitution<'_> {
    fn ty(&mut self, old: TypeId) -> TypeId {
        if let Some(&ty) = self.types.get(&old) {
            return ty;
        }
        if let Some(i) = self.parameters.iter().position(|p| *p == Some(old)) {
            return self.arguments[i];
        }
        let Some(name) = self.type_names.get(&old) else {
            return old;
        };
        let mut shape = self.module.types[old.index()].clone();
        shape.name = name.render(&self.names);
        if let Some(row) = self.module.types.iter().position(|s| s.name == shape.name) {
            let ty = TypeId(row as u32);
            self.types.insert(old, ty);
            return ty;
        }
        let row = TypeId(self.module.types.len() as u32);
        self.types.insert(old, row);
        self.module.types.push(shape.clone());
        map_fields(&mut shape, &mut |id| TypeRef(self.ty(TypeId(id.0)).0));
        // A generic row's finalizer and clone body are the instances at the
        // substituted arguments.
        shape.finalizer = shape
            .finalizer
            .map(|f| FuncRef(self.function(FuncId(f.0)).0));
        shape.clone = shape.clone.map(|f| FuncRef(self.function(FuncId(f.0)).0));
        self.module.types[row.index()] = shape;
        row
    }
    fn place(&mut self, ty: &mut PlaceTy) {
        if let PlaceTy::Declared(id) = ty {
            *id = self.ty(*id);
        }
    }
    fn signature(&mut self, old: SigId) -> SigId {
        if let Some(&id) = self.signatures.get(&old) {
            return id;
        }
        let mut sig = self.module.signatures[old.index()].clone();
        for param in &mut sig.params {
            if let Param::Address { ty: id, .. } = param {
                *id = self.ty(*id);
            }
        }
        sig.dest = sig.dest.map(|id| self.ty(id));
        let abi = self.signature_abis.get(&old).cloned().unwrap_or_default();
        let plan = scalar_abi::Plan::signature(self.module, &mut sig, &abi);
        self.plans.insert(old, plan);
        if let Some(row) = self.module.signatures.iter().position(|old| *old == sig) {
            let id = SigId(row as u32);
            self.signatures.insert(old, id);
            return id;
        }
        let id = SigId(self.module.signatures.len() as u32);
        self.module.signatures.push(sig);
        self.signatures.insert(old, id);
        id
    }
    fn function(&mut self, old: FuncId) -> FuncId {
        if let Some(&id) = self.functions.get(&old) {
            return id;
        }
        let function = self.module.functions[old.index()].clone();
        let key = &self.module.instances[function.instance.index()];
        if let Some(template) = self.nested.get(&old) {
            let key = InstanceKey {
                definition: key.definition.clone(),
                types: self.names.clone(),
                dictionaries: vec![],
            };
            if let Some(i) = self
                .module
                .functions
                .iter()
                .position(|f| self.module.instances[f.instance.index()] == key)
            {
                let row = FuncId(i as u32);
                self.functions.insert(old, row);
                return row;
            }
            let instance = InstanceId(self.module.instances.len() as u32);
            self.module.instances.push(key);
            let row = FuncId(self.module.functions.len() as u32);
            self.module.functions.push(Function {
                instance,
                signature: function.signature,
                body: None,
            });
            self.functions.insert(old, row);
            if let Err(error) = fill(self.module, old, row, template, self.arguments) {
                self.error = Some(error);
            }
            return row;
        }
        let Some(call) = self.call_names.get(&old) else {
            return old;
        };
        let key = InstanceKey {
            definition: key.definition.clone(),
            types: call
                .arguments
                .iter()
                .map(|name| name.render(&self.names))
                .collect(),
            dictionaries: call
                .dictionaries
                .iter()
                .map(|name| name.render(&self.names))
                .collect(),
        };
        if let Some(op) = call.operation {
            self.signature(function.signature);
            let Param::Address { ty, .. } =
                self.module.signatures[function.signature.index()].params[0]
            else {
                unreachable!("symbolic key payload")
            };
            let ty = self.ty(ty);
            if !key_glue::inline_scalar(self.module, ty, op) {
                return match key_glue::ensure(self.module, ty, op) {
                    Ok(row) => {
                        self.functions.insert(old, row);
                        row
                    }
                    Err(error) => {
                        self.error = Some(error.into());
                        old
                    }
                };
            }
        }
        if let Some(id) = self
            .module
            .functions
            .iter()
            .position(|f| self.module.instances[f.instance.index()] == key)
        {
            let id = FuncId(id as u32);
            self.functions.insert(old, id);
            return id;
        }
        let signature = self.signature(function.signature);
        let instance = InstanceId(self.module.instances.len() as u32);
        self.module.instances.push(key);
        let id = FuncId(self.module.functions.len() as u32);
        self.module.functions.push(Function {
            instance,
            signature,
            body: None,
        });
        self.functions.insert(old, id);
        id
    }
    fn runtime(&mut self, op: hirsel_core_ir::RtOp) -> hirsel_core_ir::RtOp {
        use hirsel_core_ir::RtOp;
        match op {
            RtOp::Spawn { x, r } => RtOp::Spawn {
                x: self.ty(x),
                r: self.ty(r),
            },
            RtOp::Join { r } => RtOp::Join { r: self.ty(r) },
            RtOp::ChannelNew { item } => RtOp::ChannelNew {
                item: self.ty(item),
            },
            RtOp::Send { item } => RtOp::Send {
                item: self.ty(item),
            },
            RtOp::TrySend { item } => RtOp::TrySend {
                item: self.ty(item),
            },
            RtOp::Receive { item } => RtOp::Receive {
                item: self.ty(item),
            },
            op => op,
        }
    }
}

fn fill(
    module: &mut Module,
    source: FuncId,
    target: FuncId,
    template: &GenericTemplate,
    arguments: &[TypeId],
) -> Result<(), String> {
    let mut body = module.functions[source.index()]
        .body
        .as_ref()
        .ok_or("generic source body is incomplete")?
        .clone();
    let names = arguments
        .iter()
        .map(|ty| module.types[ty.index()].name.clone())
        .collect();
    let mut s = Substitution {
        module,
        parameters: &template.parameters,
        arguments,
        names,
        type_names: &template.types,
        call_names: &template.calls,
        nested: &template.nested,
        error: None,
        types: HashMap::default(),
        functions: [(source, target)].into_iter().collect(),
        signatures: HashMap::default(),
        signature_abis: &template.signatures,
        plans: HashMap::default(),
    };
    let old_signature = s.module.functions[source.index()].signature;
    let old_calls = body.calls.iter().map(|c| c.signature).collect::<Vec<_>>();
    let signature = s.signature(old_signature);
    s.module.functions[target.index()].signature = signature;
    for ty in &mut body.slots {
        s.place(ty);
    }
    for address in &mut body.addresses {
        s.place(&mut address.ty);
    }
    for call in &mut body.calls {
        call.callee = match call.callee {
            Callee::Direct(f) => Callee::Direct(s.function(f)),
            Callee::Runtime(op) => Callee::Runtime(s.runtime(op)),
            callee => callee,
        };
        call.signature = s.signature(call.signature);
    }
    for inst in &mut body.insts {
        match inst {
            Inst::FunctionRef { function, .. } => *function = s.function(*function),
            Inst::Drop { ty, .. } | Inst::Release { ty, .. } => *ty = s.ty(*ty),
            Inst::Mark { ty, .. } => s.place(ty),
            _ => {}
        }
    }
    let sizes = template
        .copies
        .iter()
        .map(|copy| (copy.value, (s.ty(copy.ty), copy.logical)))
        .collect::<HashMap<_, _>>();
    let allocations = template
        .allocations
        .iter()
        .map(|a| (a.result, s.ty(a.ty)))
        .collect::<HashMap<_, _>>();
    let recipes = hirsel_core_layout::compute(&s.module.types)
        .map_err(|e| format!("instance layout: {e:?}"))?;
    // A constructor allocates its block, rather than its carried holder. Its
    // requested charge is also returned by the Capacity handback on refusal.
    let mut allocation_constants = HashMap::default();
    for inst in &mut body.insts {
        let Inst::Alloc {
            result,
            charge,
            size,
            align,
            kind,
            overflow,
            ..
        } = inst
        else {
            continue;
        };
        let Some(&ty) = allocations.get(result) else {
            continue;
        };
        let recipe = &recipes[ty.index()];
        let Layout::Fixed(fixed) = &recipe.layout else {
            return Err("a generic constructor requires a fixed block layout".into());
        };
        let logical = recipe
            .logical_size
            .ok_or("a generic constructor requires a logical block size")?;
        let amount = match kind {
            AllocKind::Resource { grant } => {
                *grant = s.module.types[ty.index()].grant;
                hirsel_core_layout::charge::resource(logical, *grant)
            }
            AllocKind::Plain => logical.checked_add(7).map(|n| n & !7),
            _ => unreachable!("only typed resource and region constructors use block relocation"),
        };
        allocation_constants.insert(*charge, amount.unwrap_or(u64::MAX));
        allocation_constants.insert(*size, fixed.size);
        allocation_constants.insert(overflow.unwrap(), u64::from(amount.is_none()));
        // The closed charge is known here. Keep only a true overflow operand;
        // an admitted resource uses its resource allocation and cause cell.
        if amount.is_some() {
            *overflow = None;
        }
        *align = fixed.align;
    }
    for inst in &mut body.insts {
        if let Inst::Const { result, bits } = inst {
            if let Some(value) = allocation_constants.get(result) {
                *bits = *value;
            }
            let Some(&(ty, logical)) = sizes.get(result) else {
                continue;
            };
            *bits = if s.module.types[ty.index()].repr == LRepr::Resource {
                hirsel_core_layout::POINTER
            } else if logical {
                recipes[ty.index()]
                    .logical_size
                    .ok_or("logical size of a compact generic value is not built")?
            } else {
                match &recipes[ty.index()].layout {
                    Layout::Fixed(f) => f.size,
                    Layout::Compact(_) => {
                        return Err("copying a compact generic value is not built".into());
                    }
                }
            };
        }
    }
    if let Some(error) = s.error {
        return Err(error);
    }
    scalar_abi::rewrite(
        s.module,
        &recipes,
        &mut body,
        old_signature,
        &s.plans,
        &old_calls,
        &template.numeric_sites,
    );
    s.module.functions[target.index()].body = Some(body);
    Ok(())
}

#[cfg(any(test, feature = "test-assembly"))]
impl Emitter {
    /// Test/evaluator assembly. Production materializes one immutable definition
    /// product at a time, using the same `materialize` transformation.
    pub fn materialize_instances(&mut self) -> Result<(), String> {
        let mut i = 0;
        while i < self.module.functions.len() {
            let f = FuncId(i as u32);
            i += 1;
            let function = &self.module.functions[f.index()];
            if function.body.is_some() {
                continue;
            }
            let key = &self.module.instances[function.instance.index()];
            if key.types.is_empty() || self.generic_calls.contains_key(&f) {
                continue;
            }
            let Some((source, template)) = self.templates.get(&key.definition).cloned() else {
                continue;
            };
            let arguments = key
                .types
                .iter()
                .map(|name| {
                    self.module
                        .types
                        .iter()
                        .position(|ty| &ty.name == name)
                        .map(|i| TypeId(i as u32))
                        .ok_or_else(|| format!("unpublished instance type {name}"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            fill(&mut self.module, source, f, &template, &arguments)?;
        }
        for (source, _) in self.templates.values() {
            self.module.functions[source.index()].body = None;
        }
        Ok(())
    }
}

impl Emitter {
    /// Record semantic name parts for symbolic layout rows once, including a generic
    /// declaration's substituted fields. Closed names remain literal text.
    pub(super) fn remember_generic_types(&mut self, p: &Program, t: &mut impl TyRead, ty: TyId) {
        if !has_param(t, ty) {
            return;
        }
        let canonical = crate::decls::canonical(&p.decls, t, ty);
        let Some(&row) = self.types.get(&canonical) else {
            return;
        };
        let row = TypeId(row.0);
        if self.generic_names.contains_key(&row) {
            return;
        }
        self.generic_names.insert(row, NameRecipe::of(p, t, ty));
        match t.kind(ty).clone() {
            TyKind::Tuple(xs) => {
                for x in xs {
                    self.remember_generic_types(p, t, x);
                }
            }
            TyKind::Array(x, _) | TyKind::View(x) => self.remember_generic_types(p, t, x),
            TyKind::Adt(d, arguments) => {
                for &x in &arguments {
                    self.remember_generic_types(p, t, x);
                }
                for field in p.decls[d.index()].all_fields() {
                    let ty = subst(t, field.ty, &arguments);
                    self.remember_generic_types(p, t, ty);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hirsel_core_ir::wf;

    /// FAIL-11 through materialization: a resource argument's finalizer is its owner's
    /// function row, which the generic's own module does not have, at an index of the
    /// owner's numbering. The instance's shape must name the owner's finalizer by its
    /// instance key; an unbuilt instance, or a row left at the owner's index (another
    /// function, or none), loses the finalizer.
    #[test]
    fn resource_argument_keeps_its_finalizer() {
        let (index, modules) = crate::tests::program_inputs(
            "resource type R { id: u64, d: u64 }\n  drop(t) { let _ = 100 / t.d }\n\
             fn eat[T](take x: T) -> u64 { 5 }\n\
             fn go(d: u64) -> u64 { match R(id: 1, d: d) { Ok(take r) => eat(take r), Error(_) => 0 } }",
        );
        let p = Program::declare(&index, &modules);
        assert!(p.refusals.is_empty(), "{:?}", p.refusals);
        // As the driver does: the generic's module holds only its own definition.
        let (mut generic, mut owner) = (Emitter::new(), Emitter::new());
        let mut eat = None;
        for (f, fd) in p.fns.iter().enumerate() {
            if !fd.path.package_is("app", "1") {
                continue;
            }
            let into = if fd.path.0.name == "eat" {
                &mut generic
            } else {
                &mut owner
            };
            let check = p.check_lower(f, into);
            assert!(
                check.refusals.is_empty() && check.gaps.is_empty(),
                "{check:?}"
            );
            if fd.path.0.name == "eat" {
                eat = Some((check.function.unwrap(), check.template.unwrap()));
            }
        }
        let (source, template) = eat.unwrap();
        let mut owner = owner.module;
        let key = owner
            .instances
            .iter()
            .find(|k| k.definition.0.name == "eat" && !k.types.is_empty())
            .unwrap()
            .clone();
        // The owner's rows sit one function later than its own numbering would put them.
        owner.functions.insert(0, owner.functions[0].clone());
        for shape in &mut owner.types {
            if let Some(f) = &mut shape.finalizer {
                f.0 += 1;
            }
        }
        let root = owner
            .types
            .iter()
            .position(|s| s.name == key.types[0])
            .unwrap();
        let finalizer = |m: &Module, ty: usize| {
            let f = m.types[ty].finalizer.unwrap();
            m.instances[m.functions[f.0 as usize].instance.index()].clone()
        };
        let (module, _) = materialize(
            &generic.module,
            source,
            &template,
            &key,
            &[(&owner, TypeRef(root as u32))],
        )
        .unwrap();
        wf::module(&module).unwrap();
        let imported = module
            .types
            .iter()
            .position(|s| s.name == key.types[0])
            .unwrap();
        assert_eq!(finalizer(&module, imported), finalizer(&owner, root));
    }

    #[test]
    fn logical_and_storage_constants() {
        let (index, modules) = crate::tests::program_inputs("fn size[T](x: T) -> u64 { 0 }");
        let p = Program::declare(&index, &modules);
        let f = p.fns.iter().position(|f| f.path.0.name == "size").unwrap();
        let mut emitter = Emitter::new();
        let check = p.check_lower(f, &mut emitter);
        assert!(
            check.refusals.is_empty() && check.gaps.is_empty(),
            "{check:?}"
        );
        let source = check.function.unwrap();
        let mut template = check.template.unwrap();
        let body = emitter.module.functions[source.index()]
            .body
            .as_ref()
            .unwrap();
        let value = body
            .insts
            .iter()
            .find_map(|i| match i {
                Inst::Const { result, bits: 0 }
                    if body.values[result.index()] == Ty::Scalar(Scalar::U64) =>
                {
                    Some(*result)
                }
                _ => None,
            })
            .unwrap();
        let ty = template.parameters[0].unwrap();
        let shape = Shape {
            name: "Padded".into(),
            repr: LRepr::Own,
            carried: hirsel_core_layout::Carried::Address,
            body: ShapeBody::Record(vec![
                hirsel_core_layout::Field {
                    name: "a".into(),
                    ty: FieldTy::Scalar(Scalar::U8),
                },
                hirsel_core_layout::Field {
                    name: "b".into(),
                    ty: FieldTy::Scalar(Scalar::U64),
                },
            ]),
            finalizer: None,
            clone: None,
            grant: 0,
        };
        // This template constant is the producer's size query, independent of any
        // source-level sizeof feature. The record's logical 9 bytes pad to 16.
        for (logical, expected) in [(true, 9), (false, 16)] {
            template.copies = vec![CopySize { value, ty, logical }];
            let key = InstanceKey {
                definition: p.fns[f].path.clone(),
                types: vec!["Padded".into()],
                dictionaries: vec![],
            };
            let owner = Module {
                types: vec![shape.clone()],
                ..Module::default()
            };
            let (module, target) = materialize(
                &emitter.module,
                source,
                &template,
                &key,
                &[(&owner, TypeRef(0))],
            )
            .unwrap();
            let report = hirsel_core_ir::eval::run(&module, target, &[0], u64::MAX);
            assert_eq!(
                report.outcome,
                hirsel_core_ir::eval::Outcome::Returned(vec![expected])
            );
        }

        let empty = Shape {
            name: "Empty".into(),
            repr: LRepr::Own,
            carried: hirsel_core_layout::Carried::Address,
            body: ShapeBody::Record(vec![]),
            finalizer: None,
            clone: None,
            grant: 0,
        };
        template.copies = vec![CopySize {
            value,
            ty,
            logical: true,
        }];
        let key = InstanceKey {
            definition: p.fns[f].path.clone(),
            types: vec!["Empty".into()],
            dictionaries: vec![],
        };
        let owner = Module {
            types: vec![empty],
            ..Module::default()
        };
        let (module, target) = materialize(
            &emitter.module,
            source,
            &template,
            &key,
            &[(&owner, TypeRef(0))],
        )
        .unwrap();
        assert_eq!(
            hirsel_core_ir::eval::run(&module, target, &[0], u64::MAX).outcome,
            hirsel_core_ir::eval::Outcome::Returned(vec![0])
        );
    }
}
