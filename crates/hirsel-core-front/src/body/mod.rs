//! Bodies: one forward visit per function that types its body against the published
//! signatures and summaries, checks ownership and emits CoreIR as it goes (LANG-8:
//! nothing else is read).
//!
//! Emitted so far: scalars and unit, blocks, `let` and `let mut`, assignment and compound
//! assignment, the operators of LANG-21 (SEM-1, SEM-2, SEM-7), `if`, `loop`, `while`, labels,
//! `break`, `continue`, `return`, direct calls of non-generic functions and the core
//! integer operations; and plain values of declared types ([`Rep::Place`]): records,
//! variants and tuples built in a new place, passed by address, returned through the
//! caller's destination, copied into `let mut` slots and read field by field.
//! Capabilities and `Args` are `u64` provider handles. Checked but not yet emitted:
//! values that need cleanup (String, List, resources), `mut` arguments, index places, and
//! the ownership rules OWN-5 to OWN-17 over them. Anything else is a [`Gap`]: the body is
//! still checked, but no code is emitted for it. An operation that can fail names no
//! failure successor, so the failure leaves the function with its cause (FORM v1a).
//!
//! Ownership state is per body and indexed by binder id: the slot table, an undo log of
//! availability changes (a branch's arms run from one state and join by OWN-16 without
//! copying the table), and the freeze and reservation records (crate::own).

use crate::decls::{
    DeclBody, FnDecl, KEY, Kind, ORDERED, Repr, Role, copyable, has_bounds, is_core_prelude, kind,
};
use crate::own::{Act, Avail, Extent, Freeze, Hold, Origin, PStep, Place, breaks, join, overlap};
use crate::types::*;
use crate::{Def, Gap, ItemRef, Program, refusal};
use hirsel_core_ids::DefPath;
use hirsel_core_ir::{
    AddressBase, AddressId, AddressInfo, AllocKind, AllocPlace, Anchor, Arg, ArithMode, ArithOp,
    BinaryOp, Block, BlockId, Body, CallId, CallSite, Callee, CompareOp, ConvertMode, Edge, EdgeId,
    FuncId, Function, Inst, InstanceId, InstanceKey, ItemId, Loan, Module, Param, PlaceTy,
    PoolRange, ReadMode, SigId, Signature, SlotId, Step, Terminator, Ty, TypeId, UnaryOp, ValueId,
};
use hirsel_core_layout::{
    Carried, FieldTy, Handle, Layout, Recipe, Repr as LRepr, Scalar, TypeRef,
};
use hirsel_core_resolve::{Refusal, Target};
use hirsel_core_syntax::{Item, NodeId, NodeKind, Span, TokenKind, byte_value, character_value};
use rustc_hash::FxHashMap as HashMap;

mod admission;
mod anchors;
mod builtins;
mod callable_abi;
mod calls;
mod cleanup;
mod clone_glue;
mod closures;
mod compact;
mod compare;
mod constants;
mod control;
pub(crate) mod core_ops;
mod key_glue;
mod lists;
mod maps;
mod numeric_glue;
mod ops;
mod patterns;
mod places;
mod scalar_abi;
mod slots;
mod spans;
mod stable;
mod templates;
mod temporaries;
mod types;
mod writers;
use templates::{AllocationLayout, CopySize, GenericCall, NameRecipe};
pub use templates::{GenericTemplate, materialize};
mod values;
use types::*;
pub(crate) use types::{float_bits, float_suffix, int_magnitude, int_suffix};

/// Work one body check did, for the cost measurements (core/MEASURE.md).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Counts {
    /// Expression, statement and pattern nodes visited, and steps deciding whether an
    /// expression synthesises its type.
    pub nodes: u64,
    /// Names read.
    pub uses: u64,
    /// Place reads and writes of `let mut` slots.
    pub places: u64,
    pub calls: u64,
    /// Type comparisons and type nodes lowered.
    pub types: u64,
    /// Bytes of the emitted body's tables.
    pub bytes: u64,
}
impl std::ops::AddAssign for Counts {
    fn add_assign(&mut self, o: Self) {
        self.nodes += o.nodes;
        self.uses += o.uses;
        self.places += o.places;
        self.calls += o.calls;
        self.types += o.types;
        self.bytes += o.bytes;
    }
}

/// The outcome of checking and lowering one definition.
#[derive(Debug, Default)]
pub struct CoreCheck {
    pub refusals: Vec<Refusal>,
    pub gaps: Vec<Gap>,
    pub counts: Counts,
    /// The function row whose body was emitted; None when the body was refused, reached
    /// a gap, or the definition has no body.
    pub function: Option<FuncId>,
    /// Layout/type relocations for a rigid generic body, checked once.
    pub template: Option<GenericTemplate>,
}

/// The module being emitted: one row per instance, made on first mention, so a call
/// can name a function whose body is checked later.
#[derive(Default)]
pub struct Emitter {
    pub module: Module,
    funcs: HashMap<InstanceKey, FuncId>,
    callable_adapters: HashMap<(FuncId, SigId), FuncId>,
    #[cfg(test)]
    pub(crate) callable_signature_builds: usize,
    sigs: HashMap<(Signature, scalar_abi::SignatureAbi), SigId>,
    signature_abis: std::collections::BTreeMap<SigId, scalar_abi::SignatureAbi>,
    /// Rows of generic templates (nested ones included): [`Emitter::seal`] leaves them
    /// to the instances that fill them.
    generic_rows: std::collections::BTreeSet<FuncId>,
    items: HashMap<DefPath, ItemId>,
    /// Per non-generic definition of the program, its function row once mentioned.
    rows: HashMap<usize, (FuncId, SigId)>,
    /// `module.types` rows by canonical text.
    types: HashMap<String, TypeRef>,
    /// The recipes of `module.types`, recomputed when rows were added.
    recipes: Vec<Recipe>,
    generic_names: std::collections::BTreeMap<TypeId, NameRecipe>,
    generic_calls: std::collections::BTreeMap<FuncId, GenericCall>,
    #[cfg(any(test, feature = "test-assembly"))]
    templates: HashMap<DefPath, (FuncId, GenericTemplate)>,
    /// Per row, whether cleaning it may run code (memoised [`Self::runs_code`]).
    runs: Vec<[Option<bool>; 2]>,
    /// `module.statics` rows by their bytes.
    statics: HashMap<Vec<u8>, hirsel_core_ir::StaticId>,
    constant_statics: HashMap<crate::ConstValue, hirsel_core_ir::StaticId>,
}

/// A literal placed by `T(..) in r` (MEM-2): its node, the region's value and root.
#[derive(Clone, Copy)]
struct Placing {
    lit: NodeId,
    region: Option<ValueId>,
    root: TyId,
    /// The literal's own expected type, `T` of an expected `Result[T in r, Capacity]`.
    want: Option<Want>,
}
/// How a value of a type is carried in a body (crate docs, "Values, slots and
/// addresses"): a scalar, nothing (unit, Never), or a pointer to a place of a declared
/// type, which a parameter takes by address and a result returns through `dest`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Rep {
    None,
    Scalar(Ty),
    Place(TypeId),
}
impl Rep {
    /// The CoreIR type of the value: a place travels as its pointer.
    fn ty(self) -> Option<Ty> {
        match self {
            Rep::None => None,
            Rep::Scalar(t) => Some(t),
            Rep::Place(_) => Some(Ty::Ptr),
        }
    }
    /// The place a value of this representation is stored in.
    fn stored(self) -> Option<PlaceTy> {
        match self {
            Rep::None => None,
            Rep::Scalar(t) => Some(PlaceTy::Value(t)),
            Rep::Place(id) => Some(PlaceTy::Declared(id)),
        }
    }
    /// The row of the place a value of this representation points at.
    fn pointee(self) -> Option<TypeId> {
        match self {
            Rep::Place(id) => Some(id),
            _ => None,
        }
    }
}
/// A parameter remains rigid while checking its source body. Its instance follows
/// the concrete row's carried representation, including a view of the parameter.
fn rigid(t: &impl TyRead, ty: TyId) -> bool {
    match t.kind(ty) {
        TyKind::Param(_) => true,
        TyKind::View(x) => matches!(t.kind(*x), TyKind::Param(_)),
        _ => false,
    }
}
/// Generic `i` of `fd` as its own argument: a type parameter, or a region's root.
fn generic_arg(fd: &crate::decls::FnDecl, i: usize) -> TyKind {
    if fd.generics[i].region {
        TyKind::Region(Root::Generic(i as u32))
    } else {
        TyKind::Param(i as u32)
    }
}
impl Emitter {
    pub fn new() -> Self {
        Self::default()
    }
    /// Convert every concrete signature to its rows' carriage at the boundary (FORM
    /// #78), once all of a module's bodies are emitted: before the module is published
    /// or its instances are filled.
    pub fn seal(&mut self) {
        scalar_abi::seal(
            &mut self.module,
            &self.signature_abis,
            &self.generic_rows,
            &self.generic_names,
        );
    }
    fn sig(&mut self, s: Signature) -> SigId {
        self.sig_abi(s, scalar_abi::SignatureAbi::default())
    }
    fn sig_abi(&mut self, s: Signature, abi: scalar_abi::SignatureAbi) -> SigId {
        let key = (s, abi);
        if let Some(&id) = self.sigs.get(&key) {
            return id;
        }
        let id = SigId(self.module.signatures.len() as u32);
        self.module.signatures.push(key.0.clone());
        if key.1.result || key.1.params.iter().any(|x| *x) {
            self.signature_abis.insert(id, key.1.clone());
        }
        self.sigs.insert(key, id);
        id
    }
    /// The read-only static row holding `bytes`, shared by equal literals.
    fn static_bytes(&mut self, bytes: &[u8]) -> hirsel_core_ir::StaticId {
        if let Some(&id) = self.statics.get(bytes) {
            return id;
        }
        let id = hirsel_core_ir::StaticId(self.module.statics.len() as u32);
        self.module.statics.push(hirsel_core_ir::Static {
            align: 1,
            bytes: bytes.to_vec(),
        });
        self.statics.insert(bytes.to_vec(), id);
        id
    }
    fn item(&mut self, path: &DefPath) -> ItemId {
        if let Some(&id) = self.items.get(path) {
            return id;
        }
        let id = ItemId(self.module.items.len() as u32);
        self.module.items.push(path.clone());
        self.items.insert(path.clone(), id);
        id
    }
    /// The function row and signature of the program's definition `f`.
    fn row(&mut self, p: &Program, f: usize) -> Result<(FuncId, SigId), &'static str> {
        if let Some(row) = self.rows.get(&f) {
            return Ok(*row);
        }
        let (sig, abi) = self.signature_recipe(p, f)?;
        let key = InstanceKey {
            definition: p.fns[f].path.clone(),
            types: vec![],
            dictionaries: vec![],
        };
        let func = self.add_function_abi(key, sig, abi);
        let row = (func, self.module.functions[func.index()].signature);
        self.rows.insert(f, row);
        Ok(row)
    }
    /// A closed generic call row. Source checking is definition-local; a row names
    /// the concrete signature and instance the materializer must supply later.
    fn instance(
        &mut self,
        p: &Program,
        t: &mut impl TyRead,
        f: usize,
        arguments: &[TyId],
    ) -> Result<(FuncId, SigId), &'static str> {
        if arguments.is_empty() {
            return self.row(p, f);
        }
        let fd = &p.fns[f];
        // A region argument names no layout, so it is erased: every region
        // instantiation shares one instance, its argument `()`, and the signature keeps
        // the callee's own generic root (MEM-6).
        let erased: Vec<_> = (0..arguments.len())
            .map(|i| {
                if fd.generics[i].region {
                    crate::types::UNIT
                } else {
                    arguments[i]
                }
            })
            .collect();
        let rooted: Vec<_> = (0..arguments.len())
            .map(|i| {
                if fd.generics[i].region {
                    t.mk(generic_arg(fd, i))
                } else {
                    arguments[i]
                }
            })
            .collect();
        let (arguments, signed) = (&erased[..], &rooted[..]);
        // An anchor bounds lifetime without changing a layout. Instance identities
        // name the published row, exactly as the materializer looks it up.
        let types: Vec<_> = arguments
            .iter()
            .map(|&ty| {
                let row = self
                    .type_row(p, t, ty)
                    .ok_or("values of this type are not built")?;
                Ok(self.module.types[row.index()].name.clone())
            })
            .collect::<Result<_, &'static str>>()?;
        let mut dictionaries = vec![];
        for (generic, ty) in fd.generics.iter().zip(&types) {
            for bit in 0..u16::BITS {
                if generic.bounds & (1 << bit) != 0 {
                    dictionaries.push(format!("{}[{ty}]", crate::decls::bound_names(1 << bit)));
                }
            }
        }
        let key = InstanceKey {
            definition: fd.path.clone(),
            types,
            dictionaries,
        };
        if let Some(&function) = self.funcs.get(&key) {
            return Ok((function, self.module.functions[function.index()].signature));
        }
        let (signature, abi) = self.signature_inst(p, t, f, signed)?;
        let function = self.add_function_abi(key, signature, abi);
        if arguments.iter().any(|&ty| has_param(t, ty)) {
            let names: Vec<_> = arguments
                .iter()
                .map(|&ty| NameRecipe::of(p, t, ty))
                .collect();
            let mut dictionaries = vec![];
            for (generic, name) in fd.generics.iter().zip(&names) {
                for bit in 0..u16::BITS {
                    if generic.bounds & (1 << bit) != 0 {
                        dictionaries.push(NameRecipe::dictionary(
                            &crate::decls::bound_names(1 << bit),
                            name,
                        ));
                    }
                }
            }
            self.generic_calls.insert(
                function,
                GenericCall {
                    arguments: names,
                    dictionaries,
                    operation: None,
                },
            );
        }
        Ok((function, self.module.functions[function.index()].signature))
    }

    /// The function row of a non-generic definition.
    pub fn function(&mut self, path: &DefPath, sig: Signature) -> FuncId {
        let key = InstanceKey {
            definition: path.clone(),
            types: vec![],
            dictionaries: vec![],
        };
        self.add_function(key, sig)
    }
    fn add_function(&mut self, key: InstanceKey, sig: Signature) -> FuncId {
        self.add_function_abi(key, sig, scalar_abi::SignatureAbi::default())
    }
    fn add_function_abi(
        &mut self,
        key: InstanceKey,
        sig: Signature,
        abi: scalar_abi::SignatureAbi,
    ) -> FuncId {
        crate::cost_scope!("instance row request", format!("{key:?}"));
        if let Some(&f) = self.funcs.get(&key) {
            return f;
        }
        let signature = self.sig_abi(sig, abi);
        let instance = InstanceId(self.module.instances.len() as u32);
        self.module.instances.push(key.clone());
        let f = FuncId(self.module.functions.len() as u32);
        self.module.functions.push(Function {
            instance,
            signature,
            body: None,
        });
        self.funcs.insert(key, f);
        f
    }
}

impl Emitter {
    /// The row of closed type `ty` in `module.types`, made with the rows it holds
    /// inline; None when some part has no field type yet.
    fn type_row(&mut self, p: &Program, t: &mut impl TyRead, ty: TyId) -> Option<TypeId> {
        crate::cost_scope!("query", "type row");
        let row = self.shaped(p, t, |s| s.shape(ty))?;
        let row = TypeId(row.0);
        self.remember_generic_types(p, t, ty);
        Some(row)
    }
    /// `ty` as a field, with the rows it names added to the module.
    fn field_ty(&mut self, p: &Program, t: &mut impl TyRead, ty: TyId) -> Option<FieldTy> {
        let edge = self.shaped(p, t, |s| s.field(ty))?;
        self.remember_generic_types(p, t, ty);
        Some(edge)
    }
    /// Run `f` over the module's shapes, taking back the rows it added when it is
    /// refused, and bind the finalizer and clone hooks of the rows it added.
    fn shaped<R, T: TyRead>(
        &mut self,
        p: &Program,
        t: &mut T,
        f: impl FnOnce(&mut crate::layout::Shapes<'_, T>) -> Option<R>,
    ) -> Option<R> {
        crate::cost_scope!("phase", "layout/drop recipes");
        let mark = self.module.types.len();
        let mut s = crate::layout::Shapes {
            decls: &p.decls,
            types: t,
            shapes: &mut self.module.types,
            of: &mut self.types,
            module: true,
            hooks: vec![],
        };
        let r = f(&mut s);
        if r.is_none() {
            s.truncate(mark);
        }
        let hooks = std::mem::take(&mut s.hooks);
        for (row, hook, f, arguments) in hooks {
            // A generic row's finalizer or clone body is its template's instance at the
            // row's arguments, materialized as any other generic callee.
            let func = if arguments.is_empty() {
                self.hook_row(p, f as usize)
            } else {
                self.instance(p, t, f as usize, &arguments).ok()?.0
            };
            let func = Some(hirsel_core_layout::FuncRef(func.0));
            let shape = &mut self.module.types[row.index()];
            match hook {
                crate::layout::Hook::Finalizer => shape.finalizer = func,
                crate::layout::Hook::Clone => shape.clone = func,
            }
        }
        r
    }
    /// The function row of the program's finalizer or clone body `f`, at its hook
    /// signature (FAIL-11, MEM-17, FORM).
    fn hook_row(&mut self, p: &Program, f: usize) -> FuncId {
        if let Some((func, _)) = self.rows.get(&f) {
            return *func;
        }
        let sig = if p.fns[f].clone_of.is_some() {
            clone_body_signature()
        } else {
            finalizer_signature()
        };
        let func = self.function(&p.fns[f].path, sig);
        let row = (func, self.module.functions[func.index()].signature);
        self.rows.insert(f, row);
        func
    }
    /// The recipe of row `id`, or None when the table's layout is refused, in which case
    /// the rows from `mark` on are taken back so the table stays computable.
    fn recipe(&mut self, id: TypeId, mark: usize) -> Option<&Recipe> {
        crate::cost_scope!("phase", "layout/drop recipes");
        if self.recipes.len() != self.module.types.len() {
            match hirsel_core_layout::extend(&self.module.types, &mut self.recipes) {
                Ok(()) => {}
                Err(_) => {
                    self.module.types.truncate(mark);
                    self.types.retain(|_, r| (r.0 as usize) < mark);
                    self.generic_names.retain(|id, _| id.index() < mark);
                    self.runs.truncate(mark);
                    return None;
                }
            }
        }
        self.recipes.get(id.index())
    }
    /// How a value of `ty` is carried: a scalar, or a place of a fixed layout. A value
    /// whose cleanup may run a finalizer (a resource, or one held inside) is not built.
    fn rep(&mut self, p: &Program, t: &mut impl TyRead, ty: TyId) -> Result<Rep, &'static str> {
        crate::cost_scope!("phase", "layout/drop recipes");
        match t.kind(ty) {
            TyKind::Anchored(x, _) | TyKind::View(x) => {
                let x = *x;
                return self.rep(p, t, x);
            }
            _ => {}
        }
        // The one rule every signature follows, also published on the row (FORM #64).
        match crate::layout::carried(&p.decls, t, ty) {
            Some(Carried::Value(v)) => return Ok(Rep::Scalar(v)),
            Some(Carried::Nothing) => return Ok(Rep::None),
            Some(Carried::Address) => {}
            None => return Err("values of this type are not built"),
        }
        let mark = self.module.types.len();
        let id = self
            .type_row(p, t, ty)
            .ok_or("values of this type are not built")?;
        match self.recipe(id, mark) {
            Some(r) if matches!(r.layout, Layout::Fixed(_)) => Ok(Rep::Place(id)),
            _ => Err("values of this type are not built"),
        }
    }
    /// Row `id` is a resource: a value of it is a pointer to its block, which holds
    /// its fields, cause cell and grant (MEM-24, layout: a holder counts 8).
    fn is_resource(&self, id: TypeId) -> bool {
        self.module.types[id.index()].repr == LRepr::Resource
    }
    /// A value of row `id` is held as a pointer to its block: a resource (MEM-24) or a
    /// stable value, which never moves once published (MEM-5).
    fn by_block(&self, id: TypeId) -> bool {
        matches!(
            self.module.types[id.index()].repr,
            LRepr::Resource | LRepr::Stable
        )
    }
    /// The admission of a resource of row `id`: its storage, cause cell and grant
    /// (MEM-24, MEM-27).
    fn resource_charge(&self, id: TypeId) -> Option<u64> {
        let storage = self.recipes[id.index()].logical_size?;
        hirsel_core_layout::charge::resource(storage, self.module.types[id.index()].grant)
    }
    /// Cleaning a value of row `id` does something (FAIL-8).
    fn needs_drop(&self, id: TypeId) -> bool {
        self.recipes[id.index()].drop.is_some()
    }
    /// Cleaning a value of row `id` may run a finalizer; a resource without one is
    /// cleaned mechanically (FAIL-13). Rows reach themselves through sequences and
    /// maps, so each row is visited once.
    fn runs_code(&mut self, id: TypeId) -> bool {
        self.cleanup_code(id, true)
    }
    fn runs_known_code(&mut self, id: TypeId) -> bool {
        self.cleanup_code(id, false)
    }
    fn cleanup_code(&mut self, id: TypeId, rigid_may_run: bool) -> bool {
        let mode = usize::from(rigid_may_run);
        if let Some(r) = self.runs.get(id.index()).and_then(|row| row[mode]) {
            return r;
        }
        let r = self.runs_code_walk(id, rigid_may_run);
        self.runs
            .resize(self.runs.len().max(id.index() + 1), [None; 2]);
        self.runs[id.index()][mode] = Some(r);
        r
    }
    fn runs_code_walk(&self, id: TypeId, rigid_may_run: bool) -> bool {
        let mut seen = vec![id];
        let mut work = vec![id];
        while let Some(id) = work.pop() {
            // A rigid row may acquire a finalizer at materialization. Keep its
            // failure successor in the once-checked template.
            if rigid_may_run && self.generic_names.contains_key(&id) {
                return true;
            }
            let Some(d) = &self.recipes[id.index()].drop else {
                continue;
            };
            if d.finalizer.is_some() {
                return true;
            }
            for f in d.fields.iter().flatten() {
                if field_runs_code(&f.ty, &mut |r| {
                    if !seen.contains(&r) {
                        seen.push(r);
                        work.push(r);
                    }
                }) {
                    return true;
                }
            }
        }
        false
    }
    /// The byte size of a value of row `id`, whose recipe exists: a resource's or a
    /// stable value's holder is its block pointer.
    fn size(&self, id: TypeId) -> u64 {
        if self.by_block(id) {
            return hirsel_core_layout::POINTER;
        }
        self.block(id).0
    }
    /// The size and alignment of row `id`'s fields laid out together (a resource's
    /// block).
    fn block(&self, id: TypeId) -> (u64, u64) {
        match &self.recipes[id.index()].layout {
            Layout::Fixed(f) => (f.size, f.align),
            Layout::Compact(_) => unreachable!("only fixed layouts are carried"),
        }
    }
    /// The CoreIR signature of the program's function `f`, or the gap that keeps it
    /// from having one: scalars by value, declared types by address, a declared result
    /// through `dest`.
    pub fn signature(&mut self, p: &Program, f: usize) -> Result<Signature, &'static str> {
        self.signature_recipe(p, f).map(|x| x.0)
    }
    fn signature_recipe(
        &mut self,
        p: &Program,
        f: usize,
    ) -> Result<(Signature, scalar_abi::SignatureAbi), &'static str> {
        let fd = &p.fns[f];
        let mut t = Local::with_decls(&p.types, &p.decls);
        let arguments: Vec<_> = (0..fd.generics.len())
            .map(|i| t.mk(generic_arg(fd, i)))
            .collect();
        self.signature_inst(p, &mut t, f, &arguments)
    }
    fn signature_inst(
        &mut self,
        p: &Program,
        t: &mut impl TyRead,
        f: usize,
        arguments: &[TyId],
    ) -> Result<(Signature, scalar_abi::SignatureAbi), &'static str> {
        crate::cost_scope!("phase", "layout/drop recipes");
        let fd = &p.fns[f];
        if fd.drop_of.is_some() {
            return Ok((finalizer_signature(), scalar_abi::SignatureAbi::default()));
        }
        if fd.clone_of.is_some() {
            return Ok((clone_body_signature(), scalar_abi::SignatureAbi::default()));
        }
        let mut params = vec![];
        let mut abi = scalar_abi::SignatureAbi {
            params: vec![],
            result: true,
        };
        for q in &fd.params {
            let ty = subst(t, q.ty, arguments);
            let rep = self.rep(p, t, ty)?;
            if q.mode == Mode::Mut {
                let id = self
                    .type_row(p, t, ty)
                    .ok_or("values of this type are not built")?;
                params.push(Param::Address {
                    ty: id,
                    loan: Loan::Mut,
                });
                abi.params.push(false);
                continue;
            }
            match rep {
                Rep::None => {}
                Rep::Scalar(s) => {
                    params.push(Param::Value(s));
                    abi.params.push(false);
                }
                Rep::Place(id) => {
                    params.push(Param::Address {
                        ty: id,
                        loan: loan(q.mode),
                    });
                    abi.params.push(true);
                }
            }
        }
        let result = subst(t, fd.result, arguments);
        let never = matches!(t.kind(result), TyKind::Never);
        let result_rep = self.rep(p, t, result)?;
        let (results, dest) = match result_rep {
            Rep::None => (vec![], None),
            Rep::Scalar(s) => (vec![s], None),
            Rep::Place(id) => (vec![], Some(id)),
        };
        Ok((
            Signature {
                params,
                results,
                dest,
                never,
            },
            abi,
        ))
    }
}

/// A clone body's: `(ptr from, ptr to) -> (u64 requested, u64 available)` (MEM-17).
fn clone_body_signature() -> Signature {
    Signature {
        params: vec![Param::Value(Ty::Ptr), Param::Value(Ty::Ptr)],
        results: vec![Ty::Scalar(Scalar::U64), Ty::Scalar(Scalar::U64)],
        dest: None,
        never: false,
    }
}

/// The loan a by-address parameter of this source mode carries (crate docs of
/// hirsel-core-ir, "Loans"). `bytes` parameters are `u64` values, never places.
pub(super) fn loan(mode: Mode) -> Loan {
    match mode {
        Mode::Mut => Loan::Mut,
        Mode::Take => Loan::Take,
        Mode::View | Mode::Bytes => Loan::View,
    }
}

fn finalizer_signature() -> Signature {
    Signature {
        params: vec![Param::Value(Ty::Ptr)],
        results: vec![],
        dest: None,
        never: false,
    }
}

/// A field whose cleanup runs code by itself; the rows it holds go to `row`.
fn field_runs_code(t: &FieldTy, row: &mut impl FnMut(TypeId)) -> bool {
    match t {
        FieldTy::Inline(r) | FieldTy::Box(r) => {
            row(TypeId(r.0));
            false
        }
        FieldTy::Seq(e) | FieldTy::Array(e, _) => field_runs_code(e, row),
        FieldTy::Map(k, v) => field_runs_code(k, row) || field_runs_code(v, row),
        // An abandoned task or receiver cleans results or items whose type the handle
        // does not name, which may run finalizers (CONC-8).
        FieldTy::Handle(Handle::Task | Handle::Receiver) => true,
        _ => false,
    }
}

/// The CoreIR type of a scalar value of `ty`, nothing for unit and Never; an error for
/// a type carried as a place or not built (see [`Rep`]).
fn value_ty(t: &impl TyRead, ty: TyId) -> Result<Option<Ty>, &'static str> {
    match t.kind(ty) {
        TyKind::Scalar(s) => Ok(Some(Ty::Scalar(*s))),
        TyKind::Fn(_, _, _, Callable::Plain) => Ok(Some(Ty::Ptr)),
        TyKind::Unit | TyKind::Never | TyKind::Error => Ok(None),
        _ => Err("values of this type are not built"),
    }
}

fn int_of(t: &impl TyRead, ty: TyId) -> Option<(bool, u32)> {
    match t.kind(ty) {
        TyKind::Scalar(s) => Ty::Scalar(*s).int(),
        _ => None,
    }
}

fn is_float(t: &impl TyRead, ty: TyId) -> bool {
    matches!(t.kind(ty), TyKind::Scalar(Scalar::F32 | Scalar::F64))
}

/// Lay out emitted instructions in block order after control-flow construction.
fn order_instructions(b: &mut Body) {
    let mut old = std::mem::take(&mut b.insts);
    let mut insts = Vec::with_capacity(old.len());
    let mut anchors = Vec::with_capacity(old.len());
    for block in &mut b.blocks {
        let (start, len) = (block.insts.start as usize, block.insts.len as usize);
        let at = insts.len() as u32;
        // The emitter gives every instruction to one block. Move it once rather
        // than dispatching Inst::clone for every variant during this permutation.
        insts.extend(old[start..start + len].iter_mut().map(|inst| {
            std::mem::replace(
                inst,
                Inst::Const {
                    result: ValueId(0),
                    bits: 0,
                },
            )
        }));
        anchors.extend_from_slice(&b.inst_anchors[start..start + len]);
        block.insts = PoolRange::new(at, len as u32);
    }
    b.insts = insts;
    b.inst_anchors = anchors;
}

impl Program<'_> {
    /// Check function `f`'s body and emit it into `e` (LANG-8: one visit, reading only
    /// this body, signatures and summaries).
    pub fn check_lower(&self, f: usize, e: &mut Emitter) -> CoreCheck {
        crate::cost_scope!("body", self.fns[f].path.to_string());
        let fd = &self.fns[f];
        if self.item(fd.at).range.has_errors {
            return CoreCheck::default();
        }
        let Some(body) = fd.body else {
            return CoreCheck::default();
        };
        crate::cost_scope!("phase", "body typing");
        let mut cx = Cx::new(self, f, e);
        cx.run(f, body);
        let mut out = cx.finish();
        if self.entry.as_ref() == Some(&fd.path) {
            self.check_entry(fd, &mut out.refusals);
        }
        out
    }

    /// PROG-3: the entry's parameters are unmarked capability types or `Args`, and its
    /// result is `Result[(), E]`.
    fn check_entry(&self, fd: &FnDecl, refusals: &mut Vec<crate::Refusal>) {
        let core = |t: TyId, names: &[&str]| match self.types.kind(t) {
            TyKind::Adt(d, args) => {
                is_core_prelude(&self.decls[d.0 as usize].path, names).then_some(args.as_slice())
            }
            _ => None,
        };
        const GRANTS: [&str; 11] = [
            "Args", "Dir", "Net", "Clock", "Random", "Stdout", "Stderr", "Stdin", "Env", "Process",
            "Tasks",
        ];
        for param in &fd.params {
            if param.ty != ERROR && (param.mode != Mode::View || core(param.ty, &GRANTS).is_none())
            {
                refusals.push(refusal(
                    "PROG-3",
                    param.site,
                    "an entry parameter is an unmarked capability or `Args`",
                ));
            }
        }
        if fd.result != ERROR && core(fd.result, &["Result"]).is_none_or(|a| a[0] != UNIT) {
            refusals.push(refusal(
                "PROG-3",
                fd.result_site,
                "the entry returns `Result[(), E]`",
            ));
        }
    }
}

/// What an expression is checked against; every mismatch is LANG-11, at the innermost
/// expression the expected type reached.
#[derive(Clone, Copy)]
struct Want {
    ty: TyId,
    /// An argument for a view parameter: a view of `ty` is also accepted (OWN-7).
    view: bool,
    /// Number of success payloads expected by enclosing `?` operators. This
    /// constrains neither Result/Option shape nor any propagated error type.
    payload: u32,
}
fn expect(ty: TyId) -> Option<Want> {
    Some(Want {
        ty,
        view: false,
        payload: 0,
    })
}

/// A local's value: an immutable binding holds its value, a `let mut` binding a slot.
#[derive(Clone, Copy)]
enum Binding {
    Value(Option<ValueId>),
    Slot(Option<AddressId>),
}
/// One row of the slot table (ownership.md, Checker state).
#[derive(Clone, Copy)]
struct Var {
    ty: TyId,
    binding: Binding,
    origin: Origin,
    avail: Avail,
    /// Where its availability last changed.
    at: NodeId,
    /// Declaration order, so a loop tells the slots declared outside it (OWN-17).
    born: u32,
    depth: u32,
    /// It reached maybe at a join (OWN-18).
    flagged: bool,
    /// Freeze and reservation records naming it; with none, no record is searched.
    holds: u32,
    /// How it is cleaned, when it owns a value that needs cleanup.
    own: Option<cleanup::Own>,
    /// The immutable provenance fact; zero has no allocated set.
    anchors: anchors::Fact,
    /// A borrowed mut parameter, including an alias captured by a nested callback.
    mut_anchor: bool,
}

/// A place and its type.
#[derive(Clone, Copy)]
struct At {
    place: Place,
    ty: TyId,
    /// Reached through a view or a stable value, so never written (OWN-12).
    shared: bool,
}

/// What a use of a place does (OWN-6, OWN-7, OWN-8).
#[derive(Clone, Copy)]
enum Use {
    /// A value position: a move when the type moves, else a copy.
    Value,
    /// `take p`.
    Take,
    View(Hold),
    Mut(Hold),
}

/// Per-slot scratch of one join or loop check, found by generation, never cleared.
#[derive(Clone, Copy)]
struct Acc {
    avail: Avail,
    /// An edge's site for a slot unavailable on it (its `Var.at`): a join keeps it.
    at: Option<NodeId>,
    count: u32,
    merge: u32,
    edge: u32,
    descriptor: Option<u32>,
    anchors: u32,
}

/// A block every one of whose predecessors is known only as they are emitted; it is
/// made at the first edge, so a join no edge reaches is never made.
#[derive(Default)]
struct Join {
    block: Option<BlockId>,
    descriptor: Option<u32>,
    anchors: u32,
    /// The row of the own value its edges carry, which is a temporary after it.
    drop: Option<TypeId>,
}

#[derive(Clone, Copy, PartialEq)]
enum FrameKind {
    Loop,
    While,
    Block,
}
struct Frame {
    label: Option<String>,
    kind: FrameKind,
    /// The loop header `continue` jumps to.
    header: Option<BlockId>,
    exit: usize,
    /// The value type its exits carry, once one fixes it.
    ty: Option<TyId>,
    /// Expected payload constraints survive until the first exit fixes its type.
    hint: Option<Want>,
    /// Whether any exit names it, reachable or not.
    left: bool,
    /// The undo log's length and the declaration count at its start.
    mark: usize,
    born: u32,
    /// Where its exit edges begin in `exits`.
    exits: usize,
    scopes: usize,
    /// The owners begun before it.
    owners: usize,
}

struct BodyInstance {
    definition: usize,
    arguments: PoolRange<TyId>,
    next: Option<usize>,
    row: (FuncId, SigId),
}

struct Cx<'c, 'a, 'e> {
    p: &'c Program<'a>,
    at: ItemRef,
    item: Item<'a>,
    fd: std::borrow::Cow<'c, FnDecl>,
    definition: usize,
    header: NodeId,
    params: Vec<u16>,
    t: Local<'c>,
    e: &'e mut Emitter,
    b: Body,
    func: Option<FuncId>,
    /// The signature a source body's entry was emitted against. A row first mentioned
    /// by an earlier body may already be sealed to its word carriage; installing the
    /// body puts this signature back, so the next [`Emitter::seal`] converts the body.
    emitted: Option<SigId>,
    ir_item: ItemId,
    cur: Option<BlockId>,
    open: Vec<Inst>,
    open_anchors: Vec<Option<Anchor>>,
    /// Emission stopped: a refusal or a gap was recorded.
    broken: bool,
    /// The expression being checked, innermost; where a halt is reported.
    node: NodeId,
    /// Where emission first stopped, or where a live expression of a valued type first
    /// produced no value. At the end of the body, a halt with no refusal and no gap
    /// recorded is itself a gap: a body is never accepted with its emission cut short.
    halted: Option<NodeId>,
    /// The slot table, indexed by binder id.
    vars: Vec<Option<Var>>,
    /// Availability changes, undone to a mark when a branch ends: (slot, old, old site).
    log: Vec<(u32, Avail, NodeId, Option<u32>, u32)>,
    /// Join inputs: the slots one incoming edge changed and their states.
    diffs: Vec<(u32, Avail, NodeId, Option<u32>, u32)>,
    /// Edges to open frames' exits: (frame, start, len) in `diffs`.
    exits: Vec<(usize, u32, u32)>,
    edge_buf: Vec<(u32, u32)>,
    acc: Vec<Acc>,
    touched: Vec<u32>,
    merges: u32,
    edges_seen: u32,
    born: u32,
    /// No path reaches this point (after `break`, `continue`, `return` or a Never call).
    dead: bool,
    /// Freeze and reservation records, and the steps of their places.
    freezes: Vec<Freeze>,
    steps: Vec<PStep>,
    /// Block depth, and the slots of the open blocks in declaration order.
    depth: u32,
    /// The type the last expression checked synthesised, before its expected type.
    synth_ty: TyId,
    /// The synthesised type of the tail of the block being closed; unit without one.
    tail_ty: TyId,
    /// The literal being placed by `T(..) in r` (MEM-2), until its construction
    /// takes it.
    placing: Option<Placing>,
    scope_slots: Vec<u32>,
    copies: Vec<CopySize>,
    allocations: Vec<AllocationLayout>,
    numeric_sites: Vec<scalar_abi::NumericSite>,
    nested: std::collections::BTreeMap<FuncId, GenericTemplate>,
    call_seq: u32,
    callable_signatures: HashMap<TyId, closures::CallableAbi>,
    callable_reps: Vec<Rep>,
    callable_regions: Vec<u32>,
    /// Invocation parameters available while a closure header is being read.
    closure_header_params: Vec<(u32, TyId)>,
    callable_type_depth: u32,
    map_entries: HashMap<(TyId, TyId), maps::MapEntry>,
    key_functions: HashMap<(TyId, key_glue::KeyOp), (TypeId, FuncId, Rep)>,
    key_args: Vec<Arg>,
    numeric_functions: HashMap<(usize, TyId, TyId), numeric_glue::NumericAbi>,
    #[cfg(test)]
    key_metadata_builds: usize,
    patterns: patterns::Patterns,
    /// Reused call workspaces, one for each active nesting level.
    call_scratch: Vec<calls::CallScratch>,
    /// Completed array elements still owned separately during construction.
    array_parts: Vec<ValueId>,
    instance_heads: HashMap<u64, usize>,
    instance_rows: Vec<BodyInstance>,
    instance_args: Vec<TyId>,
    binder_at: HashMap<(u32, u32), u32>,
    frames: Vec<Frame>,
    joins: Vec<Join>,
    /// Per node: 0 unknown, 1 synthesises a type, 2 needs an expected type.
    synth: Vec<u8>,
    waited: bool,
    /// The program's entry, which is always `waits` (LANG-8, PROG-3).
    entry: bool,
    /// The destination a declared result is written to: the entry block's first
    /// parameter and its place.
    dest: Option<(ValueId, TypeId)>,
    /// A clone body's `to` (MEM-17): its result is built in a local `dest` and moved
    /// here on `Ok`.
    clone_to: Option<ValueId>,
    /// How each type this body names is carried, found once per body.
    reps: HashMap<TyId, Result<Rep, &'static str>>,
    /// Type kinds under this body's fixed generic bounds, shared with ownership uses.
    kind_cache: HashMap<TyId, (Kind, std::cell::Cell<Option<bool>>)>,
    /// The owners of the body, in the order they began (FAIL-8).
    owners: Vec<cleanup::Owner>,
    owner_buf: Vec<cleanup::Owner>,
    retained_owners: Vec<usize>,
    reached_temps: Vec<temporaries::Reached>,
    entry_setup: Vec<Inst>,
    /// Counts changes of owner and scope state; a failure successor made at one
    /// state serves every failing instruction until it changes.
    epoch: u64,
    fail_cache: Option<((u64, usize, usize), BlockId)>,
    /// Emitting failure cleanup: a failing drop appends its cause and cleanup goes on
    /// (FAIL-10).
    failing: bool,
    /// The edge whose cleanup is being emitted (FAIL-9).
    edge: Option<cleanup::Edge>,
    /// The pending result of the exit edge [`Self::unwind`] cleans next (FAIL-9).
    pending: Option<(AddressBase, TypeId)>,
    out: CoreCheck,
    capture_frame: Option<closures::CaptureFrame>,
    descriptors: closures::Descriptors,
    anchors: anchors::Anchors,
    anchor_path: Vec<anchors::Component>,
}

/// The gap a body reports when its emission stopped with no refusal or gap to say why.
pub(crate) const SILENT_HALT: &str = "emission halted without a reported reason";

impl<'c, 'a, 'e> Cx<'c, 'a, 'e> {
    fn new(p: &'c Program<'a>, f: usize, e: &'e mut Emitter) -> Self {
        crate::cost_scope!("phase", "body setup");
        let fd = &p.fns[f];
        let item = p.item(fd.at);
        let info = &p.items[fd.at.module as usize][fd.at.item as usize];
        let binder_at = info
            .resolution
            .binders
            .iter()
            .map(|b| ((b.site.span.start, b.site.span.end), b.id))
            .collect();
        let ir_item = e.item(&fd.path);
        Cx {
            p,
            at: fd.at,
            item,
            fd: std::borrow::Cow::Borrowed(fd),
            definition: f,
            header: item.root,
            params: fd.generics.iter().map(|g| g.bounds).collect(),
            t: Local::with_decls(&p.types, &p.decls),
            e,
            b: Body::default(),
            func: None,
            emitted: None,
            ir_item,
            cur: None,
            open: vec![],
            open_anchors: vec![],
            broken: false,
            node: item.root,
            halted: None,
            vars: vec![None; info.resolution.binders.len()],
            log: vec![],
            diffs: vec![],
            exits: vec![],
            edge_buf: vec![],
            acc: vec![
                Acc {
                    avail: Avail::Live,
                    at: None,
                    count: 0,
                    merge: 0,
                    edge: 0,
                    descriptor: None,
                    anchors: 0,
                };
                info.resolution.binders.len()
            ],
            touched: vec![],
            merges: 0,
            edges_seen: 0,
            born: 0,
            dead: false,
            freezes: vec![],
            steps: vec![],
            depth: 0,
            synth_ty: UNIT,
            tail_ty: UNIT,
            placing: None,
            scope_slots: vec![],
            copies: vec![],
            allocations: vec![],
            numeric_sites: vec![],
            nested: Default::default(),
            call_seq: 0,
            callable_signatures: HashMap::default(),
            callable_reps: vec![],
            callable_regions: vec![],
            closure_header_params: vec![],
            callable_type_depth: 0,
            map_entries: HashMap::default(),
            key_functions: HashMap::default(),
            key_args: vec![],
            numeric_functions: HashMap::default(),
            #[cfg(test)]
            key_metadata_builds: 0,
            patterns: patterns::Patterns::default(),
            call_scratch: vec![],
            array_parts: vec![],
            instance_heads: HashMap::default(),
            instance_rows: vec![],
            instance_args: vec![],
            binder_at,
            frames: vec![],
            joins: vec![],
            synth: vec![0; (item.range.nodes.end - item.range.nodes.start) as usize],
            waited: false,
            entry: p.entry.as_ref() == Some(&fd.path),
            dest: None,
            clone_to: None,
            reps: HashMap::default(),
            kind_cache: HashMap::default(),
            owners: vec![],
            owner_buf: vec![],
            retained_owners: vec![],
            reached_temps: vec![],
            entry_setup: vec![],
            epoch: 0,
            fail_cache: None,
            failing: false,
            edge: None,
            pending: None,
            out: CoreCheck::default(),
            capture_frame: None,
            descriptors: closures::Descriptors::default(),
            anchors: anchors::Anchors::new(info.resolution.binders.len()),
            anchor_path: vec![],
        }
    }

    // ---- syntax
    fn kind(&self, n: NodeId) -> NodeKind {
        self.item.tree.node(n).kind
    }
    fn kids(&self, n: NodeId) -> &'a [NodeId] {
        let item = self.item;
        item.tree.children(n)
    }
    fn token_text(&self, n: NodeId) -> &'a str {
        let item = self.item;
        match item.tree.node(n).token {
            Some(t) => item.tree.token_text(item.text, t),
            None => "",
        }
    }
    fn token_kind(&self, n: NodeId) -> Option<TokenKind> {
        let t = self.item.tree.node(n).token?;
        Some(self.item.tree.tokens[t.0 as usize].kind)
    }
    fn binder(&self, n: NodeId) -> Option<u32> {
        let s = self.item.tree.node(n).origin.span;
        self.binder_at.get(&(s.start, s.end)).copied()
    }
    fn name(&self, ty: TyId) -> String {
        crate::decls::canonical(&self.p.decls, &self.t, ty)
    }

    // ---- outcomes
    fn refuse(&mut self, rule: &str, n: NodeId, message: impl Into<String>) {
        self.refuse_at(rule, self.span(n), message);
    }
    fn refuse_at(&mut self, rule: &str, span: Span, message: impl Into<String>) {
        self.refuse_from(rule, span, vec![], message);
    }
    /// A refusal with its causal sites: the earlier constructs it depends on (PROG-7).
    fn refuse_from(
        &mut self,
        rule: &str,
        span: Span,
        causal: Vec<hirsel_core_resolve::Site>,
        message: impl Into<String>,
    ) {
        crate::cost_scope!("phase", "diagnostics/gaps");
        let site = self.p.site_span(self.at, span);
        let mut r = refusal(rule, site, message);
        r.causal = causal;
        self.out.refusals.push(r);
        self.halt();
    }
    /// Node `n` of this body as a causal site.
    fn cause(&self, n: NodeId) -> hirsel_core_resolve::Site {
        self.p.site_span(self.at, self.span(n))
    }
    /// Binder `s`'s declaration as a causal site.
    fn declared_at(&self, s: u32) -> hirsel_core_resolve::Site {
        self.p.items[self.at.module as usize][self.at.item as usize]
            .resolution
            .binders[s as usize]
            .site
    }
    fn span(&self, n: NodeId) -> Span {
        self.item.tree.node(n).origin.span
    }
    /// The operator token of a unary, binary or compound-assignment node `n`.
    fn op_span(&self, n: NodeId) -> Span {
        match self.item.tree.node(n).token {
            Some(t) => self.item.token_span(t),
            None => self.span(n),
        }
    }
    /// The closing `}` (or `)`) of node `n`.
    fn close_brace(&self, n: NodeId) -> Span {
        let s = self.span(n);
        Span {
            start: s.end.saturating_sub(1),
            end: s.end,
        }
    }
    /// A construct not built yet; each one is reported once per body.
    fn gap(&mut self, n: NodeId, what: &'static str) {
        crate::cost_scope!("phase", "diagnostics/gaps");
        if !self.out.gaps.iter().any(|g| g.what == what) {
            let site = self.p.site(self.at, n);
            self.out.gaps.push(Gap::new(site, what));
        }
        self.halt();
    }
    fn halt(&mut self) {
        self.halted.get_or_insert(self.node);
        self.broken = true;
        self.cur = None;
    }
    /// `t` where `w` is expected: equal, or Never, or an already refused type.
    fn expected_part(&self, mut ty: TyId, want: Want) -> Option<TyId> {
        for _ in 0..want.payload {
            ty = self.result_parts(ty)?.1;
        }
        Some(ty)
    }
    /// A success projection that reaches a still-rigid parameter follows that
    /// parameter into its argument, without inventing its Result/Option shape.
    fn payload_argument(&self, mut result: TyId, argument: TyId, mut want: Want) -> Option<Want> {
        for depth in 0..want.payload {
            if result == argument && matches!(self.t.kind(result), TyKind::Param(_)) {
                want.payload -= depth;
                return Some(want);
            }
            result = self.result_parts(result)?.1;
        }
        None
    }
    fn coerce(&mut self, t: TyId, mut w: Want, n: NodeId) {
        let Some(t) = self.expected_part(t, w) else {
            return;
        };
        w.payload = 0;
        self.out.counts.types += 1;
        if t == w.ty {
            return;
        }
        let plain_view = self.plain_view(t, w.ty);
        let viewed = w.view
            && (matches!(self.t.kind(t), TyKind::View(x) if *x == w.ty)
                || crate::decls::stable_parameter(&self.p.decls, &self.t, t, w.ty));
        let scoped_callable = self.scoped_callable_view(t, w.ty);
        let callable = matches!((self.t.kind(t), self.t.kind(w.ty)),
            (TyKind::Fn(a, ar, aw, ak), TyKind::Fn(b, br, bw, bk))
                if a == b && ar == br && (ak == bk || matches!((ak,bk), (Callable::OwnSend, Callable::Own))) && (!aw || *bw))
            || scoped_callable
            || self.plain_callable_fit(t, w.ty);
        if !callable
            && matches!(
                (self.t.kind(t), self.t.kind(w.ty)),
                (TyKind::Fn(..), TyKind::Fn(..))
            )
        {
            self.refuse(
                "LANG-13",
                n,
                "the callable does not fit the written callable type",
            );
            return;
        }
        let sequence = matches!((self.t.kind(w.ty), self.t.kind(t)),
            (TyKind::Adt(d, a), TyKind::Adt(e, b))
                if a == b && is_core_prelude(&self.p.decls[d.index()].path, &["SeqSpan"])
                    && is_core_prelude(&self.p.decls[e.index()].path, &["IndexedSpan"]));
        // OWN-25: a stable value is read where a view of its type is expected.
        let stable = matches!(self.t.kind(w.ty), TyKind::View(x)
            if crate::decls::stable_parameter(&self.p.decls, &self.t, t, *x));
        if t != w.ty
            && !viewed
            && !plain_view
            && !sequence
            && !callable
            && !stable
            && t != NEVER
            && !mentions_error(&self.t, t)
            && !mentions_error(&self.t, w.ty)
        {
            let (a, b) = (self.name(w.ty), self.name(t));
            self.refuse("LANG-11", n, format!("expected `{a}`, found `{b}`"));
        }
    }

    // ---- emission
    /// `Never` has no values (core.md): an expression of it that has not already
    /// diverged reads a binder no call can bind, so nothing follows it (FORM #31a).
    fn diverge_if_never(&mut self, t: TyId, n: NodeId) {
        if t == NEVER && self.live() {
            self.end(Terminator::Unreachable, Some(n));
        }
    }
    fn live(&self) -> bool {
        self.cur.is_some()
    }
    fn anchor(&self, n: NodeId) -> Anchor {
        let s = self.item.tree.node(n).origin.span;
        Anchor {
            item: self.ir_item,
            start: s.start,
            end: s.end,
        }
    }
    fn value(&mut self, t: Ty) -> ValueId {
        self.b.values.push(t);
        ValueId(self.b.values.len() as u32 - 1)
    }
    fn inst(&mut self, i: Inst, n: NodeId) {
        crate::cost_scope!("phase", "CoreIR emission");
        if self.live() {
            self.open.push(i);
            self.open_anchors.push(Some(self.anchor(n)));
        }
    }
    fn konst(&mut self, t: Ty, bits: u64, n: NodeId) -> Option<ValueId> {
        if !self.live() {
            return None;
        }
        let result = self.value(t);
        self.inst(Inst::Const { result, bits }, n);
        Some(result)
    }
    /// How a value of `ty` is carried in this body.
    fn rep(&mut self, ty: TyId) -> Result<Rep, &'static str> {
        if let Some(&r) = self.reps.get(&ty) {
            return r;
        }
        let r = self.e.rep(self.p, &mut self.t, ty);
        self.reps.insert(ty, r);
        r
    }
    /// The CoreIR type of a value of `ty`; None for unit, Never and types not built.
    fn ir_ty(&mut self, ty: TyId) -> Option<Ty> {
        self.rep(ty).ok().and_then(Rep::ty)
    }
    /// An address. A resource row's place is the holder of its block pointer, except
    /// that a pointer value typed by the row, with steps after it, is the block
    /// (FORM): fields of a resource are reached only through [`Self::fields_of`].
    fn address(&mut self, base: AddressBase, steps: &[Step], ty: PlaceTy, n: NodeId) -> AddressId {
        crate::cost_scope!("phase", "CoreIR emission");
        let fail = if steps.iter().any(|s| matches!(s, Step::Element(_))) {
            self.fail_to(n)
        } else {
            None
        };
        let steps = PoolRange::push(&mut self.b.pools.steps, steps.iter().copied());
        self.b.addresses.push(AddressInfo { base, steps, ty });
        let address = AddressId(self.b.addresses.len() as u32 - 1);
        self.inst(Inst::Address { address, fail }, n);
        address
    }
    /// The place of row `id`'s fields for the value at `base` (a place of row `id`):
    /// the place itself, or for a resource the block its holder points at.
    fn fields_of(&mut self, base: AddressBase, id: TypeId, n: NodeId) -> AddressId {
        if !self.e.by_block(id) {
            return self.address(base, &[], PlaceTy::Declared(id), n);
        }
        let at = self.address(base, &[], PlaceTy::Value(Ty::Ptr), n);
        let block = self.value(Ty::Ptr);
        self.inst(
            Inst::Read {
                result: block,
                address: at,
                mode: ReadMode::Bits,
            },
            n,
        );
        self.address(AddressBase::Value(block), &[], PlaceTy::Declared(id), n)
    }
    /// A new slot of `ty`, begun here, and its address.
    fn new_slot(&mut self, ty: PlaceTy, n: NodeId) -> AddressId {
        self.out.counts.places += 1;
        self.b.slots.push(ty);
        let slot = SlotId(self.b.slots.len() as u32 - 1);
        self.inst(Inst::StorageLive { slot }, n);
        self.address(AddressBase::Slot(slot), &[], ty, n)
    }
    fn pointer(&mut self, address: AddressId, n: NodeId) -> ValueId {
        let result = self.value(Ty::Ptr);
        self.inst(Inst::Pointer { result, address }, n);
        result
    }
    /// A held value's native stride or portable logical size. Rigid parameters
    /// retain the kind of size until mechanical instance materialization.
    pub(super) fn layout_size(&mut self, ty: TyId, logical: bool, n: NodeId) -> Option<ValueId> {
        let (id, bits) = match self.rep(ty) {
            Ok(Rep::Scalar(Ty::Scalar(s))) => (None, s.size()),
            Ok(Rep::Scalar(Ty::Ptr)) => (None, hirsel_core_layout::POINTER),
            Ok(Rep::None) => (None, 0),
            Ok(Rep::Place(id)) => {
                let bits = if self.e.is_resource(id) {
                    hirsel_core_layout::POINTER
                } else if self.e.by_block(id) {
                    // A stable value's holder counts as a `Stable` field (MEM-24).
                    if logical {
                        16
                    } else {
                        hirsel_core_layout::POINTER
                    }
                } else if logical {
                    self.e.recipes[id.index()].logical_size.unwrap()
                } else {
                    self.e.size(id)
                };
                (Some(id), bits)
            }
            Err(what) => {
                self.gap(n, what);
                return None;
            }
        };
        let value = self.konst(Ty::Scalar(Scalar::U64), bits, n)?;
        if let Some(id) = id
            && self.e.generic_names.contains_key(&id)
        {
            self.copies.push(CopySize {
                value,
                ty: id,
                logical,
            });
        }
        Some(value)
    }

    /// Copy the value of row `id` that `from` points at into place `to` (a move or a
    /// copy of plain data: the source needs no cleanup afterwards).
    fn copy_to(&mut self, to: AddressId, from: ValueId, id: TypeId, n: NodeId) {
        if self.e.needs_drop(id) {
            self.consume(Some(from), n);
        }
        let from = self.address(AddressBase::Value(from), &[], PlaceTy::Declared(id), n);
        let size = self.e.size(id);
        let bytes = self.value(Ty::Scalar(Scalar::U64));
        if self.e.generic_names.contains_key(&id) {
            self.copies.push(CopySize {
                value: bytes,
                ty: id,
                logical: false,
            });
        }
        self.inst(
            Inst::Const {
                result: bytes,
                bits: size,
            },
            n,
        );
        self.inst(Inst::Copy { to, from, bytes }, n);
    }
    /// A fresh place holding a copy of the value `from` points at; its pointer.
    /// Install at `to` the view of the `elem` value at `pointer`: its own bytes, a
    /// copy nobody cleans (FORM #56).
    pub(super) fn install_view(
        &mut self,
        to: AddressId,
        elem: TyId,
        pointer: ValueId,
        n: NodeId,
    ) -> Option<()> {
        let r = match self.rep(elem) {
            Ok(r) => r,
            Err(what) => {
                self.gap(n, what);
                return None;
            }
        };
        let value = match r {
            Rep::None => return Some(()),
            Rep::Scalar(t) => {
                let at = self.address(AddressBase::Value(pointer), &[], PlaceTy::Value(t), n);
                self.read_bits(at, t, n)
            }
            Rep::Place(_) => pointer,
        };
        self.install(to, r, Some(value), n);
        Some(())
    }
    fn snapshot(&mut self, from: ValueId, id: TypeId, n: NodeId) -> ValueId {
        let to = self.new_slot(PlaceTy::Declared(id), n);
        self.copy_to(to, from, id, n);
        let value = self.pointer(to, n);
        if let Some(id) = self.descriptors.of(Some(from)) {
            self.descriptors.values.insert(value, id);
        }
        value
    }
    /// Return `v`, a value of the body's result type: a declared result is written to
    /// the destination first.
    fn ret(&mut self, v: Option<ValueId>, n: NodeId) {
        if !self.live() {
            return;
        }
        if self.return_descriptor(v) {
            return;
        }
        self.return_place_anchors();
        if !self.live() {
            return;
        }
        let v = match (self.dest, v) {
            (Some((dest, id)), Some(v)) => {
                let to = self.address(AddressBase::Value(dest), &[], PlaceTy::Declared(id), n);
                self.copy_to(to, v, id, n);
                if self.e.runs_code(id) && !self.is_view(self.fd.result) {
                    self.pending = Some((AddressBase::Address(to), id));
                }
                None
            }
            _ => v,
        };
        self.unwind(0, 0, n);
        if let Some(to) = self.clone_to {
            self.clone_return(to, n);
            return;
        }
        let r = PoolRange::push(&mut self.b.pools.values, v);
        self.end(Terminator::Return(r), Some(n));
    }
    /// A clone body's return (MEM-17): `Ok(copy)` moves the copy to `to` and returns
    /// `(0, 0)`; `Error(Capacity)` leaves `to` alone and returns its request.
    fn clone_return(&mut self, to: ValueId, n: NodeId) {
        let Some((dest, rid)) = self.dest else {
            return self.gap(n, "this clone body's result is not built");
        };
        let Some((_, this, Some(cap))) = self.result_parts(self.fd.result) else {
            return self.gap(n, "this clone body's result is not built");
        };
        let (Ok(Rep::Place(this)), Ok(Rep::Place(cap))) = (self.rep(this), self.rep(cap)) else {
            return self.gap(n, "this clone body's result is not built");
        };
        let whole = self.address(AddressBase::Value(dest), &[], PlaceTy::Declared(rid), n);
        let tag = self.value(Ty::Scalar(Scalar::U32));
        self.inst(
            Inst::Case {
                result: tag,
                address: whole,
            },
            n,
        );
        let error = self.new_block(&[]);
        self.pattern_compare(
            Some(tag),
            Ty::Scalar(Scalar::U32),
            0,
            CompareOp::Eq,
            Some(error),
            n,
        );
        let copy = self.address(
            AddressBase::Address(whole),
            &[Step::CaseField { case: 0, field: 0 }],
            PlaceTy::Declared(this),
            n,
        );
        let copy = self.pointer(copy, n);
        let target = self.address(AddressBase::Value(to), &[], PlaceTy::Declared(this), n);
        self.copy_to(target, copy, this, n);
        let Some(zero) = self.konst(Ty::Scalar(Scalar::U64), 0, n) else {
            return;
        };
        let r = PoolRange::push(&mut self.b.pools.values, [zero, zero]);
        self.end(Terminator::Return(r), Some(n));
        self.start(error);
        let refused = self.address(
            AddressBase::Address(whole),
            &[Step::CaseField { case: 1, field: 0 }],
            PlaceTy::Declared(cap),
            n,
        );
        let mut pair = [zero; 2];
        for (i, v) in pair.iter_mut().enumerate() {
            let at = self.address(
                AddressBase::Address(refused),
                &[Step::Field(i as u32)],
                PlaceTy::Value(Ty::Scalar(Scalar::U64)),
                n,
            );
            *v = self.value(Ty::Scalar(Scalar::U64));
            self.inst(
                Inst::Read {
                    result: *v,
                    address: at,
                    mode: ReadMode::Bits,
                },
                n,
            );
        }
        let r = PoolRange::push(&mut self.b.pools.values, pair);
        self.end(Terminator::Return(r), Some(n));
    }

    fn new_block(&mut self, tys: &[Ty]) -> BlockId {
        crate::cost_scope!("phase", "CoreIR emission");
        let params: Vec<ValueId> = tys.iter().map(|&t| self.value(t)).collect();
        let params = PoolRange::push(&mut self.b.pools.values, params);
        self.b.blocks.push(Block {
            params,
            insts: PoolRange::EMPTY,
            anchor: None,
        });
        // Replaced when the block ends; every block made is started and ended.
        self.b.terminators.push(Terminator::Propagate);
        BlockId(self.b.blocks.len() as u32 - 1)
    }
    fn start(&mut self, blk: BlockId) {
        if !self.broken {
            debug_assert!(self.cur.is_none());
            self.cur = Some(blk);
        }
    }
    fn end(&mut self, term: Terminator, n: Option<NodeId>) {
        crate::cost_scope!("phase", "CoreIR emission");
        let Some(c) = self.cur.take() else { return };
        let insts = PoolRange::push(&mut self.b.insts, self.open.drain(..));
        self.b.inst_anchors.append(&mut self.open_anchors);
        let anchor = n.map(|n| self.anchor(n));
        let blk = &mut self.b.blocks[c.index()];
        blk.insts = insts;
        blk.anchor = anchor;
        self.b.terminators[c.index()] = term;
    }
    /// Blocks end in visit order, which is not block order (a join is made before the
    /// arms that reach it end); a body's instructions are laid out in block order.
    fn block_order(&mut self) {
        order_instructions(&mut self.b);
    }
    fn edge(&mut self, target: BlockId, args: &[ValueId]) -> EdgeId {
        let args = PoolRange::push(&mut self.b.pools.values, args.iter().copied());
        self.b.edges.push(Edge { target, args });
        EdgeId(self.b.edges.len() as u32 - 1)
    }
    fn jump(&mut self, target: BlockId, args: &[ValueId], n: NodeId) {
        if self.live() {
            let e = self.edge(target, args);
            self.end(Terminator::Jump(e), Some(n));
        }
    }
    fn block_params(&self, blk: BlockId) -> Vec<ValueId> {
        let r = self.b.blocks[blk.index()].params;
        r.get(&self.b.pools.values).unwrap_or_default().to_vec()
    }
    fn new_join(&mut self) -> usize {
        self.joins.push(Join::default());
        self.joins.len() - 1
    }
    /// Leave the current block for join `j`, whose parameters carry a value of `ty`.
    fn goto(&mut self, j: usize, ty: TyId, v: Option<ValueId>, n: NodeId) {
        if !self.live() {
            return;
        }
        let blk = match self.joins[j].block {
            Some(b) => b,
            None => {
                let tys: Vec<Ty> = self.ir_ty(ty).into_iter().collect();
                let b = self.new_block(&tys);
                self.joins[j].block = Some(b);
                b
            }
        };
        if let Some(id) = self.drop_id(ty)
            && v.is_some()
        {
            self.consume(v, n);
            self.joins[j].drop = Some(id);
        }
        self.join_descriptor(j, v);
        self.joins[j].anchors = self.anchors.union(self.joins[j].anchors, self.anchors.last);
        let args: Vec<ValueId> = v.into_iter().collect();
        self.jump(blk, &args, n);
    }
    /// Continue at join `j`, if any edge reached it; its parameter is the joined value.
    fn land(&mut self, j: usize) -> Option<ValueId> {
        let blk = self.joins[j].block?;
        self.start(blk);
        let v = self.block_params(blk).first().copied();
        let anchors = self.joins[j].anchors;
        self.anchors.last = anchors;
        if anchors != 0 {
            self.anchors.expressions.insert(self.node, anchors);
            if let Some(v) = v {
                self.anchors.values.insert(v, anchors);
            }
        }
        if let (Some(value), Some(id)) = (v, self.joins[j].descriptor) {
            self.descriptors.values.insert(value, id);
        }
        if let (Some(id), Some(value), true) = (self.joins[j].drop, v, self.live()) {
            self.owners.push(cleanup::Owner::Temp(cleanup::Temp {
                value,
                source: self.node,
                id,
                avail: Avail::Live,
                flag: None,
                reached: 0,
            }));
            self.epoch += 1;
        }
        v
    }

    // ---- the definition
    fn run(&mut self, f: usize, body: NodeId) {
        let row = self.e.row(self.p, f);
        self.run_row(row, body);
    }
    fn run_row(&mut self, row: Result<(FuncId, SigId), &'static str>, body: NodeId) {
        self.node = body;
        match row {
            Ok((func, sig)) => {
                self.func = Some(func);
                self.emitted = Some(sig);
                let s = &self.e.module.signatures[sig.index()];
                // The destination first, then the parameters; an address arrives as a
                // pointer (Body: block 0's parameters).
                let dest = s.dest;
                let tys: Vec<Ty> = dest
                    .map(|_| Ty::Ptr)
                    .into_iter()
                    .chain(s.params.iter().map(|p| match p {
                        Param::Value(t) => *t,
                        Param::Address { .. } => Ty::Ptr,
                    }))
                    .collect();
                let entry = self.new_block(&tys);
                self.start(entry);
                if let Some(id) = dest {
                    self.dest = Some((self.block_params(entry)[0], id));
                }
            }
            Err(what) => {
                let site = self.fd.site;
                self.out.gaps.push(Gap::new(site, what));
                self.halt();
            }
        }
        let entry_params = if self.func.is_some() {
            self.block_params(BlockId(0))
        } else {
            vec![]
        };
        let mut k = usize::from(self.dest.is_some());
        if let Some(frame) = &mut self.capture_frame {
            frame.environment = entry_params.get(k).copied();
            k += 1;
        }
        let p: &'c Program<'a> = self.p;
        let binders = &p.items[self.at.module as usize][self.at.item as usize]
            .resolution
            .binders;
        let fd = self.fd.clone();
        for (parameter_index, pd) in fd.params.iter().enumerate() {
            let v = match self.ir_ty(pd.ty) {
                _ if pd.mode == Mode::Mut && k < entry_params.len() => {
                    k += 1;
                    Some(entry_params[k - 1])
                }
                Some(_) if k < entry_params.len() => {
                    k += 1;
                    Some(entry_params[k - 1])
                }
                _ => None,
            };
            if let Some(b) = binders.iter().find(|b| {
                b.name == pd.name
                    && b.kind == hirsel_core_resolve::BinderKind::Parameter
                    && pd.site.span.start <= b.site.span.start
                    && b.site.span.end <= pd.site.span.end
            }) {
                let origin = match pd.mode {
                    Mode::View | Mode::Bytes => Origin::ViewParam,
                    Mode::Mut => Origin::MutParam,
                    Mode::Take => Origin::TakeParam,
                };
                let binding = match (pd.mode, v) {
                    // A finalizer receives its resource's block pointer: `t` is a
                    // holder slot of it, so fields are reached as through any holder.
                    (Mode::Mut, Some(v))
                        if self.live()
                            && self
                                .fd
                                .drop_of
                                .is_some_and(|d| p.decls[d.index()].repr == Repr::Resource) =>
                    {
                        let holder = match self.rep(pd.ty) {
                            Ok(Rep::Place(id)) => {
                                let h = self.new_slot(PlaceTy::Declared(id), body);
                                let at = self.address(
                                    AddressBase::Address(h),
                                    &[],
                                    PlaceTy::Value(Ty::Ptr),
                                    body,
                                );
                                self.inst(
                                    Inst::Store {
                                        address: at,
                                        value: v,
                                    },
                                    body,
                                );
                                Some(h)
                            }
                            _ => None,
                        };
                        Binding::Slot(holder)
                    }
                    (Mode::Mut, Some(v)) if self.live() => {
                        let ty = match self.rep(pd.ty) {
                            Ok(Rep::None) => self
                                .e
                                .type_row(p, &mut self.t, pd.ty)
                                .map(PlaceTy::Declared),
                            Ok(r) => r.stored(),
                            Err(_) => None,
                        };
                        Binding::Slot(
                            ty.map(|ty| self.address(AddressBase::Value(v), &[], ty, body)),
                        )
                    }
                    (Mode::Take, _) => Binding::Slot(self.slot(pd.ty, v, body)),
                    _ => Binding::Value(v),
                };
                self.declare(b.id, pd.ty, binding, origin, body);
                self.parameter_anchors(b.id, pd.ty, pd.mode, parameter_index as u32);
                self.own(b.id, body);
            }
        }
        // A clone body builds its result in a local destination (`clone_return`).
        if self.fd.clone_of.is_some() && self.live() {
            self.clone_to = entry_params.get(k).copied();
            if let Ok(Rep::Place(id)) = self.rep(self.fd.result) {
                let slot = self.new_slot(PlaceTy::Declared(id), body);
                let at = self.pointer(slot, body);
                self.dest = Some((at, id));
            }
        }
        let result = self.fd.result;
        let (_, v) = self.expr(
            body,
            Some(Want {
                ty: result,
                view: false,
                payload: 0,
            }),
        );
        self.ret(v, body);
        // Only a body checked to its end shows that nothing it calls waits.
        if self.fd.waits && !self.waited && !self.entry && self.fd.drop_of.is_none() && !self.broken
        {
            let root = self.header;
            let w = self
                .kids(root)
                .iter()
                .copied()
                .find(|&c| self.kind(c) == NodeKind::Waits)
                .unwrap_or(root);
            self.refuse("LANG-8", w, "declared `waits` but calls nothing that waits");
        }
    }
    fn finish(mut self) -> CoreCheck {
        crate::cost_scope!("phase", "body finish");
        if let Some(n) = self.halted
            && self.out.refusals.is_empty()
            && self.out.gaps.is_empty()
            && let site = self.p.site(self.at, n)
            // The resolver's and the declarations' refusals of this item are reported.
            && !self.p.refusals.iter().any(|r| {
                (r.primary.module, r.primary.item) == (site.module, site.item)
            })
        {
            self.out.gaps.push(Gap::new(site, SILENT_HALT));
            self.broken = true;
        }
        let b = &self.b;
        let size = |n: usize, s: usize| (n * s) as u64;
        self.out.counts.bytes = size(b.values.len(), size_of::<Ty>())
            + size(b.slots.len(), size_of::<PlaceTy>())
            + size(b.addresses.len(), size_of::<AddressInfo>())
            + size(b.blocks.len(), size_of::<Block>())
            + size(b.terminators.len(), size_of::<Terminator>())
            + size(b.insts.len(), size_of::<Inst>())
            + size(b.inst_anchors.len(), size_of::<Option<Anchor>>())
            + size(b.calls.len(), size_of::<CallSite>())
            + size(b.edges.len(), size_of::<Edge>())
            + size(b.pools.values.len(), size_of::<ValueId>())
            + size(b.pools.args.len(), size_of::<Arg>());
        if !self.broken
            && let Some(f) = self.func
        {
            self.install_entry_setup();
            self.block_order();
            if self.fd.generics.is_empty() {
                scalar_abi::inline_keys(&self.e.module, &mut self.b);
            }
            self.e.module.functions[f.index()].body = Some(std::mem::take(&mut self.b));
            if let Some(sig) = self.emitted {
                self.e.module.functions[f.index()].signature = sig;
            }
            self.out.function = Some(f);
            if !self.fd.generics.is_empty() {
                self.e.generic_rows.insert(f);
                let mut parameters = vec![];
                for i in 0..self.fd.generics.len() {
                    let ty = self.t.mk(TyKind::Param(i as u32));
                    parameters.push(self.e.type_row(self.p, &mut self.t, ty));
                }
                let body = self.e.module.functions[f.index()].body.as_ref().unwrap();
                let mut signatures = std::collections::BTreeMap::new();
                let ids = std::iter::once(self.e.module.functions[f.index()].signature)
                    .chain(body.calls.iter().map(|c| c.signature))
                    .chain(body.insts.iter().filter_map(|i| {
                        if let Inst::FunctionRef { function, .. } = i {
                            Some(self.e.module.functions[function.index()].signature)
                        } else {
                            None
                        }
                    }));
                for id in ids {
                    if let Some(abi) = self.e.signature_abis.get(&id) {
                        signatures.entry(id).or_insert_with(|| abi.clone());
                    }
                }
                let template = GenericTemplate {
                    signatures,
                    parameters,
                    copies: std::mem::take(&mut self.copies),
                    allocations: std::mem::take(&mut self.allocations),
                    numeric_sites: std::mem::take(&mut self.numeric_sites),
                    nested: std::mem::take(&mut self.nested),
                    types: self.e.generic_names.clone(),
                    calls: self.e.generic_calls.clone(),
                };
                #[cfg(any(test, feature = "test-assembly"))]
                self.e
                    .templates
                    .insert(self.fd.path.clone(), (f, template.clone()));
                self.out.template = Some(template);
            }
        }
        self.out
    }

    // ---- expressions
    /// Check `n` against `want` when given; a construct that passes `want` inward checks
    /// it there and returns `want`'s type, so the outer check here passes.
    fn expr(&mut self, n: NodeId, want: Option<Want>) -> (TyId, Option<ValueId>) {
        self.out.counts.nodes += 1;
        let (mut t, mut v) = self.checked(n, want);
        if let Some(w) = want.filter(|w| w.payload == 0)
            && let Some((payload, value)) = self.payload_view_value(t, v, w.ty, n)
        {
            t = payload;
            v = value;
            self.anchor_payload(n, t, v);
        }
        if let Some(w) = want.filter(|w| w.payload == 0)
            && t != w.ty
            && self.plain_callable_fit(t, w.ty)
        {
            v = self.fit_plain_callable(v, w.ty, n);
            t = w.ty;
            self.synth_ty = t;
        }
        if let Some(w) = want.filter(|w| w.payload == 0)
            && self.scoped_callable_view(t, w.ty)
        {
            // The owning code/environment pair is already a scoped carrier. Its
            // owner, including a reached temporary, stays frozen while it is viewed.
            self.anchor_payload(n, w.ty, v);
            if let Some(from) = v {
                match self.rep(w.ty) {
                    Ok(Rep::Place(id)) => {
                        let borrowed = self.snapshot(from, id, n);
                        self.anchor_payload(n, w.ty, Some(borrowed));
                        v = Some(borrowed);
                    }
                    Err(what) => self.gap(n, what),
                    _ => unreachable!("a scoped callable has a carrier"),
                }
            }
            t = w.ty;
            self.synth_ty = t;
        }
        match want {
            Some(w) if t != NEVER && w.payload != 0 => {
                self.coerce(t, w, n);
                (t, v)
            }
            Some(w) if t != NEVER => {
                self.coerce(t, w, n);
                let v = if self.plain_view(t, w.ty) {
                    self.read_plain_view(w.ty, v, n)
                } else {
                    v
                };
                (w.ty, v)
            }
            _ => (t, v),
        }
    }

    /// `n` typed with `want` as its expected type but not coerced to it: the caller
    /// compares the synthesised type itself (LANG-21's right operand).
    fn expr_hinted(&mut self, n: NodeId, want: Option<Want>) -> (TyId, Option<ValueId>) {
        self.out.counts.nodes += 1;
        self.checked(n, want)
    }

    /// `n` checked and emitted: the single place an expression's value is produced. A
    /// live expression of a valued type that produced no value marks the body halted.
    fn checked(&mut self, n: NodeId, want: Option<Want>) -> (TyId, Option<ValueId>) {
        let outer = std::mem::replace(&mut self.node, n);
        let (t, v) = self.expr_inner(n, want);
        if t.0 > HOLE.0 && self.anchor_tracked(t) {
            self.finish_anchors(n, t, v);
        } else {
            self.anchors.last = 0;
        }
        self.node = outer;
        self.synth_ty = t;
        self.diverge_if_never(t, n);
        if v.is_none()
            && self.halted.is_none()
            && self.live()
            && matches!(self.rep(t), Ok(Rep::Scalar(_) | Rep::Place(_)))
        {
            self.halted = Some(n);
        }
        (t, v)
    }

    fn expr_inner(&mut self, n: NodeId, want: Option<Want>) -> (TyId, Option<ValueId>) {
        crate::cost_scope!("construct", format!("{:?}", self.kind(n)));
        crate::cost_scope!(
            "construct-state",
            format!(
                "{:?}/{}",
                self.kind(n),
                if self.broken { "gap" } else { "live" }
            )
        );
        let c = self.kids(n);
        let want = want.filter(|w| {
            w.payload == 0
                || matches!(
                    self.kind(n),
                    NodeKind::Group
                        | NodeKind::Path
                        | NodeKind::Block
                        | NodeKind::If
                        | NodeKind::Loop
                        | NodeKind::Labelled
                        | NodeKind::Call
                        | NodeKind::Try
                        | NodeKind::Match
                        | NodeKind::Region
                        | NodeKind::Bound
                )
        });
        match self.kind(n) {
            NodeKind::Group => self.expr(c[0], want),
            NodeKind::Literal => self.literal(n, want, false),
            NodeKind::Unit => (UNIT, None),
            NodeKind::Path => self.path(n, want),
            NodeKind::Block => self.block(n, want),
            NodeKind::If => self.if_(n, want),
            NodeKind::Loop => self.loop_(n, want, None),
            NodeKind::While => self.while_(n, None),
            NodeKind::Labelled => self.labelled(n, want),
            NodeKind::Return => self.return_(n),
            NodeKind::Break => self.break_(n),
            NodeKind::Continue => self.continue_(n),
            NodeKind::Unary | NodeKind::Binary
                if let Some(sc) = want.and_then(|w| match self.t.kind(w.ty) {
                    TyKind::Scalar(sc) if numeric(*sc) => Some(*sc),
                    _ => None,
                }) && !self.synthesises(n) =>
            {
                self.literal_operand(n, sc)
            }
            NodeKind::Unary => self.unary(n, want),
            NodeKind::Binary => self.binary(n, want),
            NodeKind::Call => self.call(n, want),
            NodeKind::Error => {
                // The parser refused it already.
                self.halt();
                (ERROR, None)
            }
            NodeKind::Array | NodeKind::RepeatArray => self.array(n, want),
            NodeKind::Tuple => self.tuple(n, want),
            NodeKind::Index | NodeKind::Projection => self.place_value(n, want),
            NodeKind::Try => self.try_(n, want),
            NodeKind::Match => self.match_(n, want),
            NodeKind::For | NodeKind::ForTake => self.for_(n, None),
            NodeKind::Region => self.region_(n, want),
            NodeKind::Bound => self.bound_(n, want),
            NodeKind::Placement => self.placement(n, want),
            NodeKind::Closure | NodeKind::ScopedClosure => self.closure(n, want),
            _ => self.gap_ty(n, "expression form is not built"),
        }
    }
    /// A place in a value position: moved when its type moves, else copied (OWN-6).
    fn place_value(&mut self, n: NodeId, want: Option<Want>) -> (TyId, Option<ValueId>) {
        match self.place(n) {
            Ok(Some(at)) => {
                if self.var(at.place.slot).origin == Origin::MutParam
                    && matches!(self.t.kind(at.ty), TyKind::Fn(_, _, _, Callable::Scoped))
                    && want.is_some_and(|w| w.payload == 0 && w.ty == at.ty)
                    && self
                        .freezes
                        .iter()
                        .any(|f| f.hold == Hold::Assign && f.place.slot == at.place.slot)
                {
                    self.use_place(at, Use::View(Hold::View(Extent::Temp)), n);
                    let value = self
                        .place_ptr(at, n)
                        .and_then(|value| match self.rep(at.ty) {
                            Ok(Rep::Place(id)) => Some(self.snapshot(value, id, n)),
                            Err(what) => {
                                self.gap(n, what);
                                None
                            }
                            _ => unreachable!("a scoped callable has a carrier"),
                        });
                    return (at.ty, value);
                }
                if let Some(w) = want.filter(|w| w.payload == 0)
                    && self.scoped_callable_view(at.ty, w.ty)
                {
                    self.use_place(at, Use::View(Hold::View(Extent::Temp)), n);
                    return (w.ty, self.place_ptr(at, n));
                }
                if let Some(w) = want.filter(|w| w.payload == 0)
                    && let Some(payload) = self.payload_view_at(at, w.ty)
                {
                    let copies = self.copies(payload.ty) && !self.is_view(w.ty);
                    self.use_place(
                        payload,
                        if copies {
                            Use::Value
                        } else {
                            Use::View(Hold::View(Extent::Temp))
                        },
                        n,
                    );
                    let value = if copies
                        || matches!(self.rep(payload.ty), Ok(Rep::Scalar(_) | Rep::None))
                    {
                        self.emit_place(payload, n)
                    } else {
                        self.place_ptr(payload, n)
                    };
                    let ty = if copies || self.is_view(payload.ty) {
                        payload.ty
                    } else {
                        self.t.mk(TyKind::View(payload.ty))
                    };
                    return (ty, value);
                }
                if at.place.len > 0
                    && self.is_view(at.ty)
                    && kind(
                        &self.p.decls,
                        &self.t,
                        self.var(at.place.slot).ty,
                        &self.params,
                    ) != Kind::Scoped
                {
                    self.use_place(at, Use::View(Hold::View(Extent::Temp)), n);
                    let value = if matches!(self.rep(at.ty), Ok(Rep::Place(_))) {
                        self.place_ptr(at, n)
                    } else {
                        self.emit_place(at, n)
                    };
                    return (at.ty, value);
                }
                if self.borrowed_index(at) {
                    self.use_place(at, Use::View(Hold::View(Extent::Temp)), n);
                    let ty = self.t.mk(TyKind::View(at.ty));
                    return (ty, self.place_ptr(at, n));
                }
                self.use_place(at, Use::Value, n);
                let v = self.emit_place(at, n);
                if at.place.len == 0 && !self.copies(at.ty) {
                    self.temp(at.ty, v);
                }
                (at.ty, v)
            }
            Ok(None) if self.kind(n) == NodeKind::Index => self.temporary_index(n),
            Ok(None) if self.kind(n) == NodeKind::Projection => self.temporary_field(n),
            Ok(None) => self.gap_ty(n, "field and index places are not built"),
            Err(()) => (ERROR, None),
        }
    }
    fn gap_ty(&mut self, n: NodeId, what: &'static str) -> (TyId, Option<ValueId>) {
        self.gap(n, what);
        (ERROR, None)
    }

    /// Whether `n` has a type without an expected one (LANG-11): everything but a
    /// literal operand (LANG-21). Memoised per node, so
    /// operator chains stay linear.
    fn synthesises(&mut self, n: NodeId) -> bool {
        self.out.counts.nodes += 1;
        match self.synth[(n.0 - self.item.range.nodes.start) as usize] {
            1 => return true,
            2 => return false,
            _ => {}
        }
        let c = self.kids(n);
        let r = match self.kind(n) {
            NodeKind::Literal => !self.unsuffixed_number(n),
            NodeKind::Group => self.synthesises(c[0]),
            // LANG-21: a literal operand is an unsuffixed number, or a unary `-` or `~`,
            // a binary operator or parentheses over literal operands only.
            NodeKind::Unary => !matches!(self.token_text(n), "-" | "~") || self.synthesises(c[0]),
            NodeKind::Binary => self.synthesises(c[0]) || self.synthesises(c[1]),
            _ => true,
        };
        self.synth[(n.0 - self.item.range.nodes.start) as usize] = if r { 1 } else { 2 };
        r
    }
}

/// Whether `ty` is or contains the error type: a part that already produced a refusal
/// or gap, so a comparison involving it is not refused again.
fn mentions_error(t: &impl TyRead, ty: TyId) -> bool {
    match t.kind(ty) {
        TyKind::Error => true,
        TyKind::Tuple(xs) | TyKind::Adt(_, xs) => xs.iter().any(|&x| mentions_error(t, x)),
        TyKind::Array(x, _) | TyKind::View(x) => mentions_error(t, *x),
        TyKind::Fn(ps, r, _, _) => {
            mentions_error(t, *r) || ps.iter().any(|&(_, x)| mentions_error(t, x))
        }
        TyKind::Anchored(x, root) => mentions_error(t, *x) || mentions_error(t, *root),
        TyKind::Never
        | TyKind::Unit
        | TyKind::Scalar(_)
        | TyKind::Param(_)
        | TyKind::Hole
        | TyKind::Region(_) => false,
    }
}

#[cfg(test)]
mod row_cache_tests {
    use super::*;

    #[test]
    fn only_mentioned_function_rows_occupy_the_cache() {
        let mut source = (0..64)
            .map(|i| format!("fn unused{i}() -> u64 {{ 0 }}\n"))
            .collect::<String>();
        source.push_str("fn only() -> u64 { 42 }\n");
        let (index, modules) = crate::tests::program_inputs(&source);
        let p = Program::declare(&index, &modules);
        assert!(p.refusals.is_empty(), "{:?}", p.refusals);
        let f = p.fns.iter().position(|f| f.path.0.name == "only").unwrap();
        assert!(f >= 64);
        let mut e = Emitter::new();
        let row = e.row(&p, f).unwrap();
        assert_eq!(e.rows.len(), 1, "unmentioned definitions took cache slots");
        assert_eq!(e.module.functions.len(), 1);
        assert_eq!(e.row(&p, f).unwrap(), row);
        assert_eq!(e.rows.len(), 1);
    }
}
