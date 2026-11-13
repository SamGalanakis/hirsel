//! Mechanical specialization of a body checked once with rigid type parameters.
//! Generic values use places, including when their closed type is a scalar. Layout
//! rows and copy-size constants are substituted; source is never opened here.
use super::*;
use hirsel_core_layout::{Body as ShapeBody, FieldTy, Shape};
use std::collections::BTreeMap;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct CopySize {
    pub value: ValueId,
    pub ty: TypeId,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct GenericTemplate {
    pub parameters: Vec<Option<TypeId>>,
    pub copies: Vec<CopySize>,
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
                    });
                    self.ty(p, t, ty);
                }
                self.text(") -> ");
                self.ty(p, t, *r);
                if *waits {
                    self.text(" waits");
                }
            }
            // Rooted/scoped values do not yet have a template representation.
            TyKind::Anchored(_, _) | TyKind::Region(_) => {
                self.text(crate::decls::canonical(&p.decls, t, ty))
            }
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
    pub arguments: Vec<NameRecipe>,
    pub dictionaries: Vec<NameRecipe>,
}

/// An instance's type arguments are already published shapes, not source types.
/// This is the only additional input specialization needs from its caller.
pub fn materialize(
    source: &Module,
    function: FuncId,
    template: &GenericTemplate,
    key: &InstanceKey,
    arguments: &[(&[Shape], TypeRef)],
) -> Result<(Module, FuncId), String> {
    if arguments.len() != template.parameters.len() {
        return Err("the entry has undetermined generic type arguments".into());
    }
    fn resource(
        shapes: &[Shape],
        root: TypeRef,
        seen: &mut std::collections::HashSet<TypeRef>,
    ) -> bool {
        if !seen.insert(root) {
            return false;
        }
        let mut shape = shapes[root.index()].clone();
        if shape.finalizer.is_some() || shape.repr == hirsel_core_layout::Repr::Resource {
            return true;
        }
        let mut found = false;
        map_fields(&mut shape, &mut |r| {
            found |= resource(shapes, r, seen);
            r
        });
        found
    }
    if arguments
        .iter()
        .any(|&(shapes, root)| resource(shapes, root, &mut Default::default()))
    {
        return Err("generic resource carrier materialization is not built".into());
    }
    let mut module = source.clone();
    let roots = arguments
        .iter()
        .map(|&(shapes, root)| import_type(&mut module, shapes, root, &mut HashMap::new()))
        .collect::<Vec<_>>();
    let target = FuncId(module.functions.len() as u32);
    let instance = InstanceId(module.instances.len() as u32);
    module.instances.push(key.clone());
    module.functions.push(Function {
        instance,
        signature: source.functions[function.index()].signature,
        body: None,
    });
    fill(&mut module, function, target, template, &roots)?;
    module.functions[function.index()].body = None;
    Ok((module, target))
}

fn import_type(
    module: &mut Module,
    source: &[Shape],
    root: TypeRef,
    seen: &mut HashMap<TypeRef, TypeRef>,
) -> TypeId {
    if let Some(&row) = seen.get(&root) {
        return TypeId(row.0);
    }
    let shape = &source[root.index()];
    if let Some(row) = module.types.iter().position(|s| s.name == shape.name) {
        return TypeId(row as u32);
    }
    let row = TypeRef(module.types.len() as u32);
    seen.insert(root, row);
    module.types.push(shape.clone());
    let mut shape = shape.clone();
    map_fields(&mut shape, &mut |id| {
        TypeRef(import_type(module, source, id, seen).0)
    });
    module.types[row.index()] = shape;
    TypeId(row.0)
}

fn map_fields(shape: &mut Shape, f: &mut impl FnMut(TypeRef) -> TypeRef) {
    fn field(ty: &mut FieldTy, f: &mut impl FnMut(TypeRef) -> TypeRef) {
        match ty {
            FieldTy::Inline(id)
            | FieldTy::Box(id)
            | FieldTy::Rc(id)
            | FieldTy::Arc(id)
            | FieldTy::Stable(id)
            | FieldTy::Link(id)
            | FieldTy::OptLink(id) => *id = f(*id),
            FieldTy::Seq(ty)
            | FieldTy::Array(ty, _)
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
    types: HashMap<TypeId, TypeId>,
    functions: HashMap<FuncId, FuncId>,
    signatures: HashMap<SigId, SigId>,
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
            if let Param::Address(id) = param {
                *id = self.ty(*id);
            }
        }
        sig.dest = sig.dest.map(|id| self.ty(id));
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
        types: HashMap::new(),
        functions: HashMap::from([(source, target)]),
        signatures: HashMap::new(),
    };
    let signature = s.signature(s.module.functions[source.index()].signature);
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
            _ => {}
        }
    }
    let sizes = template
        .copies
        .iter()
        .map(|copy| (copy.value, s.ty(copy.ty)))
        .collect::<HashMap<_, _>>();
    let recipes = hirsel_core_layout::compute(&s.module.types)
        .map_err(|e| format!("instance layout: {e:?}"))?;
    for inst in &mut body.insts {
        if let Inst::Const { result, bits } = inst
            && let Some(ty) = sizes.get(result)
        {
            *bits = match &recipes[ty.index()].layout {
                Layout::Fixed(f) => f.size,
                Layout::Compact(_) => {
                    return Err("copying a compact generic value is not built".into());
                }
            };
        }
    }
    s.module.functions[target.index()].body = Some(body);
    Ok(())
}

#[cfg(test)]
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
