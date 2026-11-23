//! Mechanical specialization of a body checked once with rigid type parameters.
//! Rigid values use places while checking; instances use their concrete convention.
//! Layout rows and size constants are substituted, then scalar addresses become
//! values. Source is never opened here.
use super::*;
use hirsel_core_ir::{Closure, ClosureHook, Identity};
use hirsel_core_layout::{Body as ShapeBody, FieldTy, FuncRef};
use std::collections::BTreeMap;

/// Potential codec hooks are recorded without creating a constrained function
/// instance. Once closed, a recipe is admitted only when all its arguments are codec.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum CodecRecipe {
    Structural {
        definition: DefPath,
        arguments: Vec<TypeId>,
        signature: SigId,
        dictionaries: Vec<NameRecipe>,
    },
    Tuple {
        of: DefPath,
        rows: super::codec_glue::Rows,
    },
}

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
    pub codecs: BTreeMap<TypeId, CodecRecipe>,
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
            self.text(crate::decls::layout_name(&p.decls, t, ty));
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
                    Callable::ScopedView => "scoped[view] fn(",
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
                        Mode::Steps => "steps ",
                    });
                    self.ty(p, t, ty);
                }
                self.text(") -> ");
                self.ty(p, t, *r);
                if *waits {
                    self.text(" waits");
                }
            }
            TyKind::Anchored(x, _) => self.ty(p, t, *x),
            TyKind::Region(_) => self.text(crate::decls::layout_name(&p.decls, t, ty)),
            _ => unreachable!("has_param identifies a structural type or a parameter"),
        }
    }
    pub(super) fn of(p: &Program, t: &impl TyRead, mut ty: TyId) -> Self {
        while let TyKind::Anchored(x, _) | TyKind::View(x) = *t.kind(ty) {
            ty = x;
        }
        let mut name = Self::default();
        name.ty(p, t, ty);
        name
    }
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct GenericCall {
    pub operation: Option<CallOperation>,
    pub dictionaries: Vec<NameRecipe>,
}
/// A generic body's call whose callee is chosen by its instance's type arguments.
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
pub enum CallOperation {
    /// Key glue `op` at the row: derived where it is materialised, or inline.
    Key(key_glue::KeyOp, TypeId),
    /// `std.codec.of[T]()` at T's row: the call is of T's codec maker ([`codec_maker`]).
    CodecOf(TypeId),
}

/// The instance `identity` of the generic body `function` of `source`, whose type
/// arguments are `arguments` (the closure of each, in parameter order): the closures'
/// rows are imported into a copy of `source` and the body is substituted. No shape is
/// read from any other module, and none is found by a name alone.
pub fn materialize(
    source: &Module,
    function: FuncId,
    template: &GenericTemplate,
    identity: &Identity,
    arguments: &[&Closure],
) -> Result<(Module, FuncId), String> {
    crate::cost_scope!("instance", format!("{:?}", identity.definition));
    if arguments.len() != template.parameters.len() {
        return Err("the entry has undetermined generic type arguments".into());
    }
    let mut module = source.clone();
    let mut names = names_of(&module);
    let roots = arguments
        .iter()
        .map(|closure| import(&mut module, &mut names, closure))
        .collect::<Result<Vec<_>, _>>()?;
    let key = InstanceKey {
        definition: identity.definition.clone(),
        arguments: roots.clone(),
        dictionaries: identity.dictionaries.clone(),
    };
    // A body that calls its own instance (a region-only generic's recursion erases to
    // the instance's own key, MEM-6) already holds a signature-only row of that key:
    // the body fills it, so the unit defines one function per link name.
    let signature = source.functions[function.index()].signature;
    let target = match module
        .functions
        .iter()
        .position(|g| g.body.is_none() && module.instances[g.instance.index()] == key)
    {
        Some(row) => {
            module.functions[row].signature = signature;
            FuncId(row as u32)
        }
        None => {
            let instance = InstanceId(module.instances.len() as u32);
            module.instances.push(key);
            module.functions.push(Function {
                versioned: false,
                instance,
                signature,
                body: None,
            });
            FuncId(module.functions.len() as u32 - 1)
        }
    };
    fill(&mut module, &mut names, function, target, template, &roots)?;
    fn retire(module: &mut Module, source: FuncId, template: &GenericTemplate) {
        module.functions[source.index()].body = None;
        for (&nested, template) in &template.nested {
            retire(module, nested, template);
        }
    }
    retire(&mut module, function, template);
    clear_rigid(&mut module);
    Ok((module, target))
}

/// The glue instance `identity` (an operation of [`key_glue::KeyOp::of`]): a module
/// of its arguments' closures and the operation's one derived body. Every other
/// operation the body calls is a signature-only row, an instance of its own.
pub fn materialize_glue(
    identity: &Identity,
    arguments: &[&Closure],
) -> Result<(Module, FuncId), String> {
    let mut module = Module::default();
    let mut names = names_of(&module);
    let roots = arguments
        .iter()
        .map(|closure| import(&mut module, &mut names, closure))
        .collect::<Result<Vec<_>, _>>()?;
    let key = InstanceKey {
        definition: identity.definition.clone(),
        arguments: roots,
        dictionaries: identity.dictionaries.clone(),
    };
    if let Some((op, rows)) = codec_glue::CodecOp::of(&key) {
        let f = codec_glue::derive(&mut module, &key.definition, &rows, op)?;
        return Ok((module, f));
    }
    let (op, ty) = key_glue::KeyOp::of(&key)
        .ok_or_else(|| format!("{} is not a glue operation", identity.definition))?;
    let f = key_glue::derive_glue(&mut module, ty, op).map_err(str::to_owned)?;
    Ok((module, f))
}

/// Whether instance `key` names a template's rigid parameter (`$i`).
fn rigid(module: &Module, key: &InstanceKey) -> bool {
    key.arguments
        .iter()
        .any(|t| module.types[t.index()].name.contains('$'))
}

/// The row of instance `identity` in `module`, its arguments' closures imported:
/// the module's row of that instance, or a new one. A row is never copied from
/// another module, whose type rows are numbered otherwise.
pub fn import_instance(
    module: &mut Module,
    identity: &Identity,
    arguments: &[&Closure],
) -> Result<InstanceId, String> {
    let mut names = names_of(module);
    let key = InstanceKey {
        definition: identity.definition.clone(),
        arguments: arguments
            .iter()
            .map(|closure| import(module, &mut names, closure))
            .collect::<Result<_, _>>()?,
        dictionaries: identity.dictionaries.clone(),
    };
    Ok(match module.instances.iter().position(|k| *k == key) {
        Some(row) => InstanceId(row as u32),
        None => {
            module.instances.push(key);
            InstanceId(module.instances.len() as u32 - 1)
        }
    })
}

/// A shape at a template's rigid parameters (`$i`) is dead once substituted; its
/// finalizer's or clone body's rigid instance must not become a dependency of the
/// unit. Closures are taken only after this clear.
fn clear_rigid(module: &mut Module) {
    let rigid = |module: &Module, f: Option<FuncRef>| {
        f.is_some_and(|f| {
            rigid(
                module,
                &module.instances[module.functions[f.0 as usize].instance.index()],
            )
        })
    };
    for t in 0..module.types.len() {
        if rigid(module, module.types[t].finalizer) {
            std::sync::Arc::make_mut(&mut module.types[t]).finalizer = None;
        }
        if rigid(module, module.types[t].clone) {
            std::sync::Arc::make_mut(&mut module.types[t]).clone = None;
        }
        if rigid(module, module.types[t].codec) {
            std::sync::Arc::make_mut(&mut module.types[t]).codec = None;
        }
    }
}

/// The materialising module's own row names. A module's producer gives each name one
/// row (lowering's `of`); rows that enter from a closure are checked against the
/// module's row of their name before they share it ([`import`]).
type Names = crate::layout::ShapeNames;
fn names_of(module: &Module) -> Names {
    module
        .types
        .iter()
        .enumerate()
        .map(|(i, shape)| (shape.name.clone(), TypeRef(i as u32)))
        .collect()
}

/// The rows of `closure` in `module`: each position is the module's row of its name or
/// a new row; then the module's closure of the root must be `closure` itself, so a row
/// of the same name that is another type is refused rather than shared.
fn import(module: &mut Module, names: &mut Names, closure: &Closure) -> Result<TypeId, String> {
    let mut rows = Vec::with_capacity(closure.rows.len());
    let mut fresh = vec![];
    for (position, row) in closure.rows.iter().enumerate() {
        let (id, added) = crate::layout::intern_shape(&mut module.types, names, row.shape.clone());
        let id = TypeId(id.0);
        if added {
            fresh.push((position, id));
        }
        rows.push(id);
    }
    for (position, id) in fresh {
        let row = &closure.rows[position];
        let mut shape = row.shape.clone();
        shape.map_refs(&mut |r| TypeRef(rows[r.index()].0));
        let [finalizer, clone, codec] = &row.hooks;
        shape.finalizer = finalizer
            .as_ref()
            .map(|hook| FuncRef(import_hook(module, &rows, hook).0));
        shape.clone = clone
            .as_ref()
            .map(|hook| FuncRef(import_hook(module, &rows, hook).0));
        shape.codec = codec
            .as_ref()
            .map(|hook| FuncRef(import_hook(module, &rows, hook).0));
        module.types[id.index()] = hirsel_core_layout::Row::new(shape);
    }
    let root = rows[0];
    if module.closure(root).hash != closure.hash {
        return Err(format!(
            "instance argument {}: this module's row of that name is another type",
            closure.rows[0].shape.name
        ));
    }
    Ok(root)
}

/// The row of a closure row's hook in `module`: the row of its instance, or a new
/// signature-only row the instance collector resolves as any other callee.
fn import_hook(module: &mut Module, rows: &[TypeId], hook: &ClosureHook) -> FuncId {
    let key = InstanceKey {
        definition: hook.definition.clone(),
        arguments: hook.arguments.iter().map(|&p| rows[p as usize]).collect(),
        dictionaries: hook.dictionaries.clone(),
    };
    if let Some(row) = module
        .functions
        .iter()
        .position(|g| module.instances[g.instance.index()] == key)
    {
        return FuncId(row as u32);
    }
    let mut signature = hook.signature.clone();
    for param in &mut signature.params {
        if let Param::Address {
            place: PlaceTy::Declared(id),
            ..
        } = param
        {
            *id = rows[id.index()];
        }
    }
    signature.dest = signature.dest.map(|id| rows[id.index()]);
    let signature = match module.signatures.iter().position(|s| *s == signature) {
        Some(i) => SigId(i as u32),
        None => {
            module.signatures.push(signature);
            SigId(module.signatures.len() as u32 - 1)
        }
    };
    let instance = InstanceId(module.instances.len() as u32);
    module.instances.push(key);
    module.functions.push(Function {
        versioned: false,
        instance,
        signature,
        body: None,
    });
    FuncId(module.functions.len() as u32 - 1)
}

/// The codec maker `of[T]()` calls at T's closed row `ty` (EXPANSION 9.1), as a row of
/// `module` with `signature`, `of`'s at T and every maker's: `() -> Codec[T]`. A
/// built-in type's is std's `of_X` beside `of` (`of` names that package); any other
/// type has none, which a checked program never asks for, since `of` requires the
/// `codec` bound.
pub(super) fn codec_maker(
    module: &mut Module,
    of: &DefPath,
    ty: TypeId,
    signature: SigId,
) -> Result<FuncId, String> {
    let name = &module.types[ty.index()].name;
    let Some(maker) = builtin_codec(name) else {
        // A declared codec type's row carries its derived maker.
        return match module.types[ty.index()].codec {
            Some(maker) => Ok(FuncId(maker.0)),
            None => Err(format!("{name} has no codec maker")),
        };
    };
    let mut definition = of.clone();
    definition.0.name = maker.into();
    let key = InstanceKey {
        definition,
        arguments: vec![],
        dictionaries: vec![],
    };
    if let Some(row) = module
        .functions
        .iter()
        .position(|f| module.instances[f.instance.index()] == key)
    {
        return Ok(FuncId(row as u32));
    }
    let instance = InstanceId(module.instances.len() as u32);
    module.instances.push(key);
    module.functions.push(Function {
        versioned: false,
        instance,
        signature,
        body: None,
    });
    Ok(FuncId(module.functions.len() as u32 - 1))
}

/// std's maker of each built-in type, by the type's row name: a scalar, `()`, String
/// Never or DecodeError. `of` and every maker are siblings in std's `codec` module.
const BUILTIN_CODECS: &[(&str, &str, bool)] = &[
    ("()", "of_unit", true),
    ("Never", "of_never", true),
    ("bool", "of_bool", true),
    ("char", "of_char", true),
    ("u8", "of_u8", true),
    ("u16", "of_u16", true),
    ("u32", "of_u32", true),
    ("u64", "of_u64", true),
    ("i8", "of_i8", true),
    ("i16", "of_i16", true),
    ("i32", "of_i32", true),
    ("i64", "of_i64", true),
    ("f32", "of_f32", true),
    ("f64", "of_f64", true),
    ("core@1/core/String", "of_string", true),
    ("std@1/codec/DecodeError", "of_decode_error", false),
];
/// The makers of [`BUILTIN_CODECS`], which a body naming `of` reads
/// ([`crate::SIBLING_READS`]).
pub const BUILTIN_CODEC_MAKERS: [&str; BUILTIN_CODECS.len()] = {
    let mut makers = [""; BUILTIN_CODECS.len()];
    let mut i = 0;
    while i < makers.len() {
        makers[i] = BUILTIN_CODECS[i].1;
        i += 1;
    }
    makers
};

/// std's maker of the built-in type whose row is named `name`.
pub(super) fn builtin_codec(name: &str) -> Option<&'static str> {
    BUILTIN_CODECS
        .iter()
        .find_map(|&(row, maker, _)| (row == name).then_some(maker))
}

/// Built-in makers witness their implicit codec bound. A legacy
/// protocol helper alone does not; nominal types get the bound from declarations.
fn builtin_codec_bound(name: &str) -> bool {
    BUILTIN_CODECS
        .iter()
        .any(|&(row, _, bound)| row == name && bound)
}

struct Substitution<'a> {
    module: &'a mut Module,
    rows: &'a mut Names,
    parameters: &'a [Option<TypeId>],
    arguments: &'a [TypeId],
    names: Vec<String>,
    type_names: &'a BTreeMap<TypeId, NameRecipe>,
    call_names: &'a BTreeMap<FuncId, GenericCall>,
    nested: &'a BTreeMap<FuncId, GenericTemplate>,
    codecs: &'a BTreeMap<TypeId, CodecRecipe>,
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
        let mut shape = hirsel_core_layout::Shape::clone(&self.module.types[old.index()]);
        shape.name = name.render(&self.names);
        let (row, added) =
            crate::layout::intern_shape(&mut self.module.types, self.rows, shape.clone());
        let row = TypeId(row.0);
        self.types.insert(old, row);
        if !added {
            return row;
        }
        shape.held = shape.held.map(|held| self.field(&held, false));
        std::sync::Arc::make_mut(&mut self.module.types[row.index()]).held = shape.held.clone();
        let compact = matches!(shape.repr, hirsel_core_layout::Repr::Compact(_));
        let fields = |fields: &mut Vec<hirsel_core_layout::Field>, s: &mut Self| {
            for x in fields {
                x.ty = s.field(&x.ty, compact);
            }
        };
        match &mut shape.body {
            ShapeBody::Record(xs) => fields(xs, self),
            ShapeBody::Variant(cases) => cases.iter_mut().for_each(|c| fields(&mut c.fields, self)),
        }
        // A generic row's finalizer, clone body and codec maker are the instances at
        // the substituted arguments.
        shape.finalizer = shape
            .finalizer
            .map(|f| FuncRef(self.function(FuncId(f.0)).0));
        shape.clone = shape.clone.map(|f| FuncRef(self.function(FuncId(f.0)).0));
        shape.codec = shape.codec.map(|f| FuncRef(self.function(FuncId(f.0)).0));
        if shape.codec.is_none()
            && let Some(recipe) = self.codecs.get(&old).cloned()
        {
            shape.codec = self.codec(recipe).map(|f| FuncRef(f.0));
        }
        self.module.types[row.index()] = hirsel_core_layout::Row::new(shape);
        crate::layout::word_carriage(&mut self.module.types, row.index());
        row
    }
    fn codec(&mut self, recipe: CodecRecipe) -> Option<FuncId> {
        let available = |module: &Module, ty: TypeId| {
            let shape = &module.types[ty.index()];
            shape.codec.is_some() || builtin_codec_bound(&shape.name)
        };
        match recipe {
            CodecRecipe::Structural {
                definition,
                arguments,
                signature,
                dictionaries,
            } => {
                let arguments = arguments
                    .into_iter()
                    .map(|a| self.ty(a))
                    .collect::<Vec<_>>();
                if !arguments.iter().all(|&a| available(self.module, a)) {
                    return None;
                }
                let mut dictionaries: Vec<String> =
                    dictionaries.iter().map(|d| d.render(&self.names)).collect();
                let (mut definition, mut arguments) = (definition, arguments);
                // A List reaching u8 by substitution takes the one byte-list codec,
                // as a closed List[u8] does (`StructuralCodecs::maker`).
                if definition.0.name == crate::STRUCTURAL_CODECS[0].1
                    && let [element] = arguments[..]
                    && self.module.types[element.index()].name == "u8"
                {
                    definition.0.name = crate::BYTES_CODEC.into();
                    arguments.clear();
                    dictionaries.clear();
                }
                let key = InstanceKey {
                    definition,
                    arguments,
                    dictionaries,
                };
                if let Some(f) = self
                    .module
                    .functions
                    .iter()
                    .position(|f| self.module.instances[f.instance.index()] == key)
                {
                    return Some(FuncId(f as u32));
                }
                let signature = self.signature(signature);
                let instance = InstanceId(self.module.instances.len() as u32);
                self.module.instances.push(key);
                let f = FuncId(self.module.functions.len() as u32);
                self.module.functions.push(Function {
                    versioned: false,
                    instance,
                    signature,
                    body: None,
                });
                Some(f)
            }
            CodecRecipe::Tuple { of, mut rows } => {
                rows.tuple = self.ty(rows.tuple);
                for part in &mut rows.parts {
                    part.ty = self.ty(part.ty);
                }
                if !rows.parts.iter().all(|p| available(self.module, p.ty)) {
                    return None;
                }
                rows.codec = self.ty(rows.codec);
                rows.decoded = self.ty(rows.decoded);
                rows.written = self.ty(rows.written);
                rows.bytes = self.ty(rows.bytes);
                rows.schema = self.ty(rows.schema);
                for part in &mut rows.parts {
                    part.codec = self.ty(part.codec);
                    part.decoded = self.ty(part.decoded);
                }
                Some(super::codec_glue::row(
                    self.module,
                    &of,
                    &rows,
                    super::codec_glue::CodecOp::Of,
                ))
            }
        }
    }
    /// The template field `ty` at the instance's arguments, written as a source row
    /// writes it (FORM #109): a row held inline whose type a holder writes otherwise
    /// (a parameter's scalar, list, box, region value, ..) becomes its `held` field,
    /// without the locator in a compact context (`compact`: a compact row's own
    /// fields, or a span's elements), so one name is one closure.
    fn field(&mut self, ty: &FieldTy, compact: bool) -> FieldTy {
        let row = |id: &TypeRef, s: &mut Self| TypeRef(s.ty(TypeId(id.0)).0);
        match ty {
            FieldTy::Inline(id) => {
                let id = row(id, self);
                match self.module.types[id.index()].held.clone() {
                    Some(FieldTy::Locator(x)) if compact => *x,
                    Some(FieldTy::RegionRef(x)) if compact => FieldTy::Inline(x),
                    Some(held) => held,
                    None => FieldTy::Inline(id),
                }
            }
            FieldTy::Box(id) => FieldTy::Box(row(id, self)),
            FieldTy::Rc(id) => FieldTy::Rc(row(id, self)),
            FieldTy::Arc(id) => FieldTy::Arc(row(id, self)),
            FieldTy::Shared(id) => FieldTy::Shared(row(id, self)),
            FieldTy::RegionRef(id) => FieldTy::RegionRef(row(id, self)),
            FieldTy::Link(id) => FieldTy::Link(row(id, self)),
            FieldTy::Seq(x) => FieldTy::Seq(Box::new(self.field(x, false))),
            FieldTy::Array(x, n) => FieldTy::Array(Box::new(self.field(x, false)), *n),
            FieldTy::Map(k, v) => FieldTy::Map(
                Box::new(self.field(k, false)),
                Box::new(self.field(v, false)),
            ),
            FieldTy::View(x) => crate::layout::view_field(self.field(x, compact)),
            FieldTy::Locator(x) => FieldTy::Locator(Box::new(self.field(x, compact))),
            FieldTy::FixedSpan(x) => FieldTy::FixedSpan(Box::new(self.field(x, true))),
            FieldTy::SeqSpan(x) => FieldTy::SeqSpan(Box::new(self.field(x, true))),
            FieldTy::IndexedSpan(x) => FieldTy::IndexedSpan(Box::new(self.field(x, true))),
            other => other.clone(),
        }
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
            if let Param::Address {
                place: PlaceTy::Declared(id),
                ..
            } = param
            {
                *id = self.ty(*id);
            }
        }
        sig.dest = sig.dest.map(|id| self.ty(id));
        let abi = self.signature_abis.get(&old).cloned().unwrap_or_default();
        let plan = scalar_abi::Plan::signature(self.module, &mut sig, &abi);
        self.plans.insert(old, plan);
        let row = self.module.signatures.iter().position(|old| *old == sig);
        if let Some(row) = row {
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
        let key = self.module.instances[function.instance.index()].clone();
        if let Some(template) = self.nested.get(&old) {
            let key = InstanceKey {
                definition: key.definition.clone(),
                arguments: self.arguments.to_vec(),
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
                versioned: function.versioned,
                instance,
                signature: function.signature,
                body: None,
            });
            self.functions.insert(old, row);
            if let Err(error) = fill(self.module, self.rows, old, row, template, self.arguments) {
                self.error = Some(error);
            }
            return row;
        }
        let Some(call) = self.call_names.get(&old) else {
            return old;
        };
        let key = InstanceKey {
            definition: key.definition.clone(),
            arguments: key.arguments.iter().map(|&t| self.ty(t)).collect(),
            dictionaries: call
                .dictionaries
                .iter()
                .map(|name| name.render(&self.names))
                .collect(),
        };
        match call.operation {
            Some(CallOperation::Key(op, ty)) => {
                self.signature(function.signature);
                let ty = self.ty(ty);
                if !key_glue::inline_scalar(self.module, ty, op) {
                    let row = key_glue::glue_row(self.module, ty, op);
                    self.functions.insert(old, row);
                    return row;
                }
            }
            Some(CallOperation::CodecOf(ty)) => {
                let signature = self.signature(function.signature);
                let ty = self.ty(ty);
                return match codec_maker(self.module, &key.definition, ty, signature) {
                    Ok(row) => {
                        self.functions.insert(old, row);
                        row
                    }
                    Err(error) => {
                        self.error = Some(error);
                        old
                    }
                };
            }
            None => {}
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
            versioned: false,
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
            RtOp::SpawnUnit { x, r } => RtOp::SpawnUnit {
                x: self.ty(x),
                r: self.ty(r),
            },
            RtOp::ConnNew { item } => RtOp::ConnNew {
                item: self.ty(item),
            },
            RtOp::ConnSend { item } => RtOp::ConnSend {
                item: self.ty(item),
            },
            RtOp::ConnTrySend { item } => RtOp::ConnTrySend {
                item: self.ty(item),
            },
            RtOp::ConnReceive { item } => RtOp::ConnReceive {
                item: self.ty(item),
            },
            RtOp::ConnTryReceive { item } => RtOp::ConnTryReceive {
                item: self.ty(item),
            },
            RtOp::Join { r } => RtOp::Join { r: self.ty(r) },
            RtOp::ChannelNew { item } => RtOp::ChannelNew {
                item: self.ty(item),
            },
            RtOp::LatestNew { item } => RtOp::LatestNew {
                item: self.ty(item),
            },
            RtOp::SharedNew { item } => RtOp::SharedNew {
                item: self.ty(item),
            },
            RtOp::SharedRelease { item } => RtOp::SharedRelease {
                item: self.ty(item),
            },
            RtOp::LaunchTarget { view } => RtOp::LaunchTarget {
                view: self.ty(view),
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
            RtOp::TryReceive { item } => RtOp::TryReceive {
                item: self.ty(item),
            },
            op => op,
        }
    }
}

fn fill(
    module: &mut Module,
    rows: &mut Names,
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
        rows,
        parameters: &template.parameters,
        arguments,
        names,
        type_names: &template.types,
        call_names: &template.calls,
        nested: &template.nested,
        codecs: &template.codecs,
        error: None,
        types: HashMap::default(),
        functions: [(source, target)].into_iter().collect(),
        signatures: HashMap::default(),
        signature_abis: &template.signatures,
        plans: HashMap::default(),
    };
    let old_signature = s.module.functions[source.index()].signature;
    let signature = s.signature(old_signature);
    s.module.functions[target.index()].signature = signature;
    for ty in &mut body.slots {
        s.place(ty);
    }
    for address in &mut body.addresses {
        s.place(&mut address.ty);
    }
    let mut old_calls = Vec::with_capacity(body.calls.len());
    for call in &mut body.calls {
        if let Callee::Runtime(op) = call.callee {
            let op = s.runtime(op);
            let signature = op.signature();
            let id = match s.module.signatures.iter().position(|old| *old == signature) {
                Some(row) => SigId(row as u32),
                None => {
                    let row = SigId(s.module.signatures.len() as u32);
                    s.module.signatures.push(signature);
                    row
                }
            };
            call.callee = Callee::Runtime(op);
            call.signature = id;
            old_calls.push(scalar_abi::CallPlan::fixed(s.module, id));
        } else {
            let old = call.signature;
            if let Callee::Direct(f) = call.callee {
                call.callee = Callee::Direct(s.function(f));
            }
            call.signature = s.signature(old);
            old_calls.push(scalar_abi::CallPlan {
                signature: old,
                conversion: s.plans[&old].clone(),
            });
        }
    }
    for inst in &mut body.insts {
        match inst {
            Inst::LayoutConst { query, .. } => {
                query.map_types(|t| TypeRef(s.ty(TypeId(t.0)).0));
            }
            Inst::FunctionRef { function, .. } | Inst::DescriptorRef { function, .. } => {
                *function = s.function(*function)
            }
            Inst::Drop { ty, .. } | Inst::Release { ty, .. } | Inst::CompactWidth { ty, .. } => {
                *ty = s.ty(*ty)
            }
            Inst::Mark { ty, .. } | Inst::Unmark { ty, .. } => s.place(ty),
            _ => {}
        }
    }
    for root in &mut body.compact_roots {
        root.ty = s.ty(root.ty);
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
            align,
            kind,
            overflow,
            ..
        } = inst
        else {
            continue;
        };
        // The alloc names its owning edge; its cleanup class is derived from the
        // instance's recipes wherever it is needed, so only the edge is remapped.
        match kind {
            AllocKind::Own { edge } => *edge = s.field(edge, false),
            AllocKind::Environment { ty } => {
                let FieldTy::Inline(row) = s.field(&FieldTy::Inline(*ty), false) else {
                    unreachable!("an inline row remaps to an inline row")
                };
                *ty = row;
                // The template's literal charge used the symbolic capture widths.
                // Admission and cleanup must both use the closed capture layout.
                let captures = recipes[row.index()]
                    .logical_size
                    .ok_or("a closure environment requires a logical capture size")?;
                let amount = hirsel_core_layout::charge::closure_env(captures)
                    .ok_or("a closure environment charge exceeds u64")?;
                allocation_constants.insert(*charge, amount);
            }
            _ => {}
        }
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
            AllocKind::Region => logical.checked_add(7).map(|n| n & !7),
            _ => unreachable!("only typed resource and region constructors use block relocation"),
        };
        allocation_constants.insert(*charge, amount.unwrap_or(u64::MAX));
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
            *bits = held_size(
                s.module.types[ty.index()].repr,
                &recipes[ty.index()],
                logical,
            )?;
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
        &s.plans[&old_signature],
        &old_calls,
        &template.numeric_sites,
    );
    s.module.functions[target.index()].versioned = s.module.functions[source.index()].versioned;
    s.module.functions[target.index()].body = Some(body);
    Ok(())
}

impl Emitter {
    /// Record semantic name parts for symbolic layout rows once, including a generic
    /// declaration's substituted fields. Closed names remain literal text.
    pub(super) fn remember_generic_types(
        &mut self,
        p: &Program,
        t: &mut impl TyRead,
        mut ty: TyId,
    ) {
        crate::cost_scope!("query", "generic type names");
        if !has_param(t, ty) {
            return;
        }
        while let TyKind::Anchored(x, _) | TyKind::View(x) = *t.kind(ty) {
            ty = x;
            if !has_param(t, ty) {
                return;
            }
        }
        let row = crate::decls::with_layout_name(&p.decls, t, ty, |name| {
            self.types.get(name.as_ref()).copied()
        });
        let Some(row) = row else {
            // A type with no row of its own (a `List[Slot[T]]` is a Seq field) still
            // names rows through its arguments: without their recipes, substitution
            // leaves them rigid under a concrete row (`Pool[A].slots` at `Slot[$0]`).
            let arguments = match t.kind(ty).clone() {
                TyKind::Tuple(xs) | TyKind::Adt(_, xs) => xs,
                TyKind::Array(x, _) => vec![x],
                _ => vec![],
            };
            for x in arguments {
                self.remember_generic_types(p, t, x);
            }
            return;
        };
        let row = TypeId(row.0);
        if self.generic_names.contains_key(&row) {
            return;
        }
        self.generic_names.insert(row, NameRecipe::of(p, t, ty));
        let arity = self.generic_names[&row]
            .0
            .iter()
            .filter_map(|part| {
                if let NamePart::Parameter(i) = part {
                    Some(*i as usize + 1)
                } else {
                    None
                }
            })
            .max()
            .unwrap_or(0);
        // Preserve semantic exclusions (views, callables, resource payloads) before
        // layout erases a view to the bytes of its referent. Parameters are optimistic
        // here; the closed recipe checks their actual codec makers during fill.
        let possible = crate::decls::has_bounds(&p.decls, t, ty, &vec![crate::decls::CODEC; arity])
            & crate::decls::CODEC
            != 0;
        if possible {
            match t.kind(ty).clone() {
                TyKind::Tuple(_) => {
                    if let Some((of, rows)) = self.tuple_codec_rows(p, t, ty) {
                        self.generic_codecs
                            .insert(row, CodecRecipe::Tuple { of, rows });
                    }
                }
                TyKind::Adt(d, args) => {
                    if let Some((maker, args)) = p.structural_codecs().maker(&*t, d, &args) {
                        let arguments = args
                            .iter()
                            .map(|&x| self.type_row(p, t, x))
                            .collect::<Option<Vec<_>>>();
                        if let Some(arguments) = arguments
                            && let Ok((sig, abi)) = self.signature_inst(p, t, maker as usize, &args)
                        {
                            let signature = self.sig_abi(sig, abi);
                            let mut dictionaries = vec![];
                            for (generic, &arg) in p.fns[maker as usize].generics.iter().zip(&args)
                            {
                                for bit in 0..u16::BITS {
                                    if generic.bounds & (1 << bit) != 0 {
                                        dictionaries.push(NameRecipe::dictionary(
                                            &crate::decls::bound_names(1 << bit),
                                            &NameRecipe::of(p, t, arg),
                                        ));
                                    }
                                }
                            }
                            self.generic_codecs.insert(
                                row,
                                CodecRecipe::Structural {
                                    definition: p.fns[maker as usize].path.clone(),
                                    arguments,
                                    signature,
                                    dictionaries,
                                },
                            );
                        }
                    }
                }
                _ => {}
            }
        }

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
    use hirsel_core_layout::Shape;

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
            if !fd.path.package_is("app", "1") || !matches!(fd.path.0.name.as_str(), "eat" | "go") {
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
        let instance = owner
            .instances
            .iter()
            .position(|k| k.definition.0.name == "eat" && !k.arguments.is_empty())
            .unwrap();
        let identity = owner.identity(InstanceId(instance as u32));
        let root = owner.instances[instance].arguments[0];
        // The owner's rows sit one function later than its own numbering would put them.
        owner.functions.insert(0, owner.functions[0].clone());
        for shape in owner.types.iter_mut().map(std::sync::Arc::make_mut) {
            if let Some(f) = &mut shape.finalizer {
                f.0 += 1;
            }
        }
        let finalizer = |m: &Module, ty: TypeId| {
            let f = m.types[ty.index()].finalizer.unwrap();
            m.render_instance(m.functions[f.0 as usize].instance)
        };
        let (module, target) = materialize(
            &generic.module,
            source,
            &template,
            &identity,
            &[&owner.closure(root)],
        )
        .unwrap();
        wf::module(&module).unwrap();
        let key = &module.instances[module.functions[target.index()].instance.index()];
        assert_eq!(
            finalizer(&module, key.arguments[0]),
            finalizer(&owner, root)
        );
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
            codec: None,
            grant: 0,
            send: false,
            held: None,
        };
        // This template constant is the producer's size query, independent of any
        // source-level sizeof feature. The record's logical 9 bytes pad to 16.
        for (logical, expected) in [(true, 9), (false, 16)] {
            template.copies = vec![CopySize { value, ty, logical }];
            let owner = Module {
                types: vec![hirsel_core_layout::Row::new(shape.clone())],
                ..Module::default()
            };
            let closure = owner.closure(TypeId(0));
            let identity = Identity {
                definition: p.fns[f].path.clone(),
                dictionaries: vec![],
                closures: vec![closure.hash],
            };
            let (module, target) =
                materialize(&emitter.module, source, &template, &identity, &[&closure]).unwrap();
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
            codec: None,
            grant: 0,
            send: false,
            held: None,
        };
        template.copies = vec![CopySize {
            value,
            ty,
            logical: true,
        }];
        let owner = Module {
            types: vec![hirsel_core_layout::Row::new(empty)],
            ..Module::default()
        };
        let closure = owner.closure(TypeId(0));
        let identity = Identity {
            definition: p.fns[f].path.clone(),
            dictionaries: vec![],
            closures: vec![closure.hash],
        };
        let (module, target) =
            materialize(&emitter.module, source, &template, &identity, &[&closure]).unwrap();
        assert_eq!(
            hirsel_core_ir::eval::run(&module, target, &[0], u64::MAX).outcome,
            hirsel_core_ir::eval::Outcome::Returned(vec![0])
        );
    }
}
