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
    DeclBody, FLOAT, FnDecl, INTEGER, KEY, Kind, ORDERED, Repr, Role, copyable, has_bounds,
    is_core_prelude, kind,
};
use crate::own::{Act, Avail, Extent, Freeze, Hold, Origin, PStep, Place, breaks, join, overlap};
use crate::types::*;
use crate::{Def, Gap, ItemRef, Program, refusal};
use hirsel_core_ids::DefPath;
use hirsel_core_ir::{
    AddressBase, AddressId, AddressInfo, AllocKind, AllocPlace, Anchor, Arg, ArithMode, ArithOp,
    BinaryOp, Block, BlockId, Body, CallId, CallSite, Callee, CompareOp, ConvertMode, Edge, EdgeId,
    FuncId, Function, Inst, InstanceId, InstanceKey, ItemId, LayoutQuery, Loan, Module, Param,
    PlaceTy, PoolRange, ReadMode, SeqWord, SigId, Signature, SimdOp, SlotId, Step, Terminator, Ty,
    TypeId, UnaryOp, ValueId, Vector,
};
use hirsel_core_layout::{
    Carried, FieldTy, Handle, Layout, Recipe, RecipeRow, Repr as LRepr, Scalar, TypeRef,
};
use hirsel_core_resolve::{CausalRole, Cause, NameScope, Refusal, RefusalDetail, Target};
use hirsel_core_syntax::{Item, NodeId, NodeKind, Span, TokenKind, byte_value, character_value};
use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};
use simd::SimdCall;
mod admission;
use admission::held_size;
mod anchors;
mod builtins;
mod bytes;
mod order;
use order::order_instructions;
mod callable_abi;
mod calls;
mod cleanup;
mod clone_glue;
mod closures;
mod code_describe;
mod codec_glue;
mod columns;
mod compact;
mod compare;
mod constants;
mod control;
mod depth_steps;
mod exits;
use exits::{Frame, FrameKind};
pub(crate) mod core_ops;
pub(crate) mod flow;
mod frame_lists;
mod frame_strings;
mod growth;
mod joins;
mod key_glue;
mod lists;
mod maps;
mod numeric_glue;
mod ops;
pub(crate) mod ordered_codec;
mod patterns;
mod places;
mod prefetch;
mod row_closures;
pub(crate) use row_closures::RowClosures;
mod scalar_abi;
mod simd;
mod slots;
mod spans;
mod stable;
mod stack_frame;
mod templates;
mod temporaries;
mod types;
mod writers;
pub use key_glue::{GLUE_OPERATIONS, is_glue};
use templates::{AllocationLayout, CallOperation, CopySize, GenericCall, NameRecipe};
pub use templates::{
    BUILTIN_CODEC_MAKERS, GenericTemplate, NamePart, import_instance, materialize, materialize_glue,
};
mod values;
use types::*;
pub(crate) use types::{
    float_binary_bits, float_negate_bits, float_suffix, int_magnitude, int_suffix, parse_float_bits,
};
pub(crate) use values::quoted_bytes;

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
    pub flow: flow::Facts,
    /// The types this body's contextual constructors resolved to.
    pub constructor_types: std::collections::BTreeSet<DefPath>,
    /// A contextual constructor resolved by searching its whole candidate set
    /// (no expected type), so every candidate of that name took part.
    pub constructor_candidates: bool,
    /// The computed constants this body reads (LANG-20): their values enter its
    /// instances' record keys and its groups' constant slots, never its body.
    pub computed: std::collections::BTreeSet<DefPath>,
}

/// Source conventions survive physical sealing. Generated fixed-ABI glue has the
/// same source and physical convention, recorded unchanged here.
#[derive(Default)]
pub(super) struct SourceSignatures {
    functions: Vec<Option<SigId>>,
    signatures: HashMap<(Signature, scalar_abi::SignatureAbi), SigId>,
    abis: std::collections::BTreeMap<SigId, scalar_abi::SignatureAbi>,
}
impl SourceSignatures {
    fn get(&self, function: FuncId) -> Option<SigId> {
        self.functions.get(function.index()).copied().flatten()
    }
    fn remember(&mut self, function: FuncId, signature: SigId) {
        self.functions
            .resize(self.functions.len().max(function.index() + 1), None);
        self.functions[function.index()] = Some(signature);
    }
    fn intern(
        &mut self,
        module: &mut Module,
        sig: Signature,
        abi: scalar_abi::SignatureAbi,
    ) -> SigId {
        let key = (sig, abi);
        if let Some(&id) = self.signatures.get(&key) {
            return id;
        }
        let id = SigId(module.signatures.len() as u32);
        module.signatures.push(key.0.clone());
        if key.1.result || key.1.params.iter().any(|x| *x) {
            self.abis.insert(id, key.1.clone());
        }
        self.signatures.insert(key, id);
        id
    }
}

/// The module being emitted: one row per instance, made on first mention, so a call
/// can name a function whose body is checked later.
#[derive(Default)]
pub struct Emitter {
    pub module: Module,
    funcs: HashMap<InstanceKey, FuncId>,
    callable_adapters: HashMap<(FuncId, SigId), (FuncId, Option<GenericTemplate>)>,
    #[cfg(test)]
    pub(crate) callable_signature_builds: usize,
    source_signatures: SourceSignatures,
    /// Rows of generic templates (nested ones included): [`Emitter::seal`] leaves them
    /// to the instances that fill them.
    generic_rows: std::collections::BTreeSet<FuncId>,
    items: HashMap<DefPath, ItemId>,
    /// Per non-generic definition of the program, its function row once mentioned.
    rows: HashMap<usize, FuncId>,
    /// The row interner shared by source shapes and materialized templates.
    types: crate::layout::ShapeNames,
    /// Successful row lookups for types in the program's immutable type arena.
    program_type_rows: HashMap<TyId, TypeId>,
    /// The recipes of `module.types`, recomputed when rows were added.
    recipes: Vec<RecipeRow>,
    recipe_scratch: hirsel_core_layout::RecipeScratch,
    generic_names: std::collections::BTreeMap<TypeId, NameRecipe>,
    generic_codecs: std::collections::BTreeMap<TypeId, templates::CodecRecipe>,
    generic_key_codecs: std::collections::BTreeMap<TypeId, (DefPath, codec_glue::Rows)>,
    generic_calls: std::collections::BTreeMap<FuncId, GenericCall>,
    /// Per row, whether cleaning it may run code (memoised [`Self::runs_code`]).
    runs: Vec<[Option<bool>; 2]>,
    /// `module.statics` rows by their bytes.
    statics: HashMap<Vec<u8>, hirsel_core_ir::StaticId>,
    constant_statics: HashMap<crate::ConstValue, hirsel_core_ir::StaticId>,
    /// A computed constant's slot (PROG-10), by constant.
    computed_statics: HashMap<u32, hirsel_core_ir::StaticId>,
    /// Some layout or recipe was refused and its rows taken back.
    refused: bool,
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
            &self.source_signatures.abis,
            &self.generic_rows,
            &self.generic_names,
        );
    }
    fn sig(&mut self, s: Signature) -> SigId {
        self.sig_abi(s, scalar_abi::SignatureAbi::default())
    }
    fn sig_abi(&mut self, s: Signature, abi: scalar_abi::SignatureAbi) -> SigId {
        self.source_signatures.intern(&mut self.module, s, abi)
    }
    /// A source body is emitted in this convention; `seal` alone converts it.
    pub fn source_signature(&self, function: FuncId) -> SigId {
        self.source_signatures
            .get(function)
            .expect("a source-emission function has its source signature")
    }
    fn install_source_signature(&mut self, function: FuncId, signature: SigId) {
        self.source_signatures.remember(function, signature);
        self.module.functions[function.index()].signature = signature;
    }
    /// The read-only static row holding `bytes`, shared by equal literals.
    fn static_bytes(&mut self, bytes: &[u8]) -> hirsel_core_ir::StaticId {
        if let Some(&id) = self.statics.get(bytes) {
            return id;
        }
        let id = hirsel_core_ir::StaticId(self.module.statics.len() as u32);
        self.module.statics.push(hirsel_core_ir::Static::Bytes {
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
    fn row(
        &mut self,
        p: &Program,
        t: &mut impl TyRead,
        f: usize,
    ) -> Result<(FuncId, SigId), &'static str> {
        if let Some(&function) = self.rows.get(&f) {
            return Ok((function, self.source_signature(function)));
        }
        let (sig, abi) = self.signature_inst(p, t, f, &[])?;
        let key = InstanceKey {
            definition: p.fns[f].path.clone(),
            arguments: vec![],
            dictionaries: vec![],
        };
        let func = self.add_function_abi(key, sig, abi);
        let row = (func, self.source_signature(func));
        self.rows.insert(f, func);
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
            return self.row(p, t, f);
        }
        let fd = &p.fns[f];
        // A region argument names no layout, so it is erased: every region
        // instantiation shares one instance, its argument `()`, and the signature keeps
        // the callee's own generic root (MEM-6).
        let erased;
        let rooted;
        let (arguments, signed) = if fd.generics.iter().any(|g| g.region) {
            erased = arguments
                .iter()
                .zip(&fd.generics)
                .map(|(&arg, g)| if g.region { crate::types::UNIT } else { arg })
                .collect::<Vec<_>>();
            rooted = arguments
                .iter()
                .zip(&fd.generics)
                .enumerate()
                .map(|(i, (&arg, g))| {
                    if g.region {
                        t.mk(generic_arg(fd, i))
                    } else {
                        arg
                    }
                })
                .collect::<Vec<_>>();
            (&erased[..], &rooted[..])
        } else {
            (arguments, arguments)
        };
        // An anchor bounds lifetime without changing a layout. Instance identities
        // name the published row, exactly as the materializer looks it up.
        let rows: Vec<_> = arguments
            .iter()
            .map(|&ty| {
                self.type_row(p, t, ty)
                    .ok_or("values of this type are not built")
            })
            .collect::<Result<_, &'static str>>()?;
        let mut dictionaries = vec![];
        for ((generic, &row), &ty) in fd.generics.iter().zip(&rows).zip(arguments) {
            if generic.bounds & crate::decls::CODEC != 0 {
                // `of` through the instance reaches the argument's maker by its row.
                self.tuple_codec(p, t, ty, row);
                if has_param(t, ty) {
                    self.structural_codec(p, t, ty, row);
                }
            }
            for bit in 0..u16::BITS {
                if generic.bounds & (1 << bit) != 0 {
                    let ty = &self.module.types[row.index()].name;
                    dictionaries.push(format!("{}[{ty}]", crate::decls::bound_names(1 << bit)));
                }
            }
        }
        let key = InstanceKey {
            definition: fd.path.clone(),
            arguments: rows,
            dictionaries,
        };
        if let Some(&function) = self.funcs.get(&key) {
            return Ok((function, self.source_signature(function)));
        }
        let (signature, abi) = self.signature_inst(p, t, f, signed)?;
        let function = self.add_function_abi(key, signature, abi);
        if arguments.iter().any(|&ty| has_param(t, ty)) {
            let mut dictionaries = vec![];
            for (generic, &ty) in fd.generics.iter().zip(arguments) {
                for bit in 0..u16::BITS {
                    if generic.bounds & (1 << bit) != 0 {
                        dictionaries.push(NameRecipe::dictionary(
                            &crate::decls::bound_names(1 << bit),
                            &NameRecipe::of(p, t, ty),
                        ));
                    }
                }
            }
            self.generic_calls.insert(
                function,
                GenericCall {
                    dictionaries,
                    operation: None,
                },
            );
        }
        Ok((function, self.source_signature(function)))
    }

    /// Gives the row of `ty`, a List, Map, Option or Result that holds a parameter, std's maker at its arguments as its codec hook, when an instance takes the
    /// row for a `codec`-bound parameter: `of` there reaches the maker once substitution
    /// instantiates the hook with the row. A closed row has its hook from layout
    /// (`layout::Shapes::structural_codec`), since a closed row's hook is part of it in the
    /// closure hash; a generic row gets one only where it is asked for, as hooking every
    /// generic row builds makers whose bodies lay out further generic rows without end.
    fn structural_codec(&mut self, p: &Program, t: &mut impl TyRead, ty: TyId, row: TypeId) {
        if self.module.types[row.index()].codec.is_some() {
            return;
        }
        let TyKind::Adt(d, args) = t.kind(ty).clone() else {
            return;
        };
        let Some((maker, args)) = p.structural_codecs().maker(&*t, d, &args) else {
            return;
        };
        if let Ok((func, _)) = self.instance(p, t, maker as usize, &args) {
            std::sync::Arc::make_mut(&mut self.module.types[row.index()]).codec =
                Some(hirsel_core_layout::FuncRef(func.0));
        }
    }

    /// The function row of a non-generic definition.
    pub fn function(&mut self, path: &DefPath, sig: Signature) -> FuncId {
        let key = InstanceKey {
            definition: path.clone(),
            arguments: vec![],
            dictionaries: vec![],
        };
        self.add_function(key, sig)
    }
    fn add_function(&mut self, key: InstanceKey, sig: Signature) -> FuncId {
        self.add_function_abi(key, sig, scalar_abi::SignatureAbi::default())
    }
    /// `op`'s glue at closed row `ty`: a signature-only row, an instance of its own
    /// whose body is derived where it is materialised. The row is registered here,
    /// so a later request for that instance, closed or the placeholder of a type
    /// generic only in its regions (whose name, carrying no region, is already
    /// closed), is that row and not a second one (one row per instance; FORM #117).
    fn glue(&mut self, ty: TypeId, op: key_glue::KeyOp) -> Result<FuncId, &'static str> {
        key_glue::derivable(&self.module, ty, op)?;
        Ok(self.glue_function(op.instance(ty), |m| op.sealed(m, ty).1))
    }
    /// The signature-only row of generated glue at `key`, made on first mention. Glue's
    /// constructor emits its fixed convention directly: it has no source carriage for
    /// seal to convert, so `sealed` is its source signature too, interned with every other.
    fn glue_function(
        &mut self,
        key: InstanceKey,
        sealed: impl FnOnce(&Module) -> Signature,
    ) -> FuncId {
        if let Some(&f) = self.funcs.get(&key) {
            return f;
        }
        let signature = self.sig(sealed(&self.module));
        let instance = InstanceId(self.module.instances.len() as u32);
        self.module.instances.push(key.clone());
        let f = FuncId(self.module.functions.len() as u32);
        self.module.functions.push(Function {
            instance,
            signature,
            body: None,
            versioned: false,
            cold: false,
        });
        self.source_signatures.remember(f, signature);
        self.funcs.insert(key, f);
        f
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
        let versioned = sig.has_vectors();
        let signature = self.sig_abi(sig, abi);
        let instance = InstanceId(self.module.instances.len() as u32);
        self.module.instances.push(key.clone());
        let f = FuncId(self.module.functions.len() as u32);
        self.module.functions.push(Function {
            instance,
            signature,
            body: None,
            versioned,
            cold: false,
        });
        self.source_signatures.remember(f, signature);
        self.funcs.insert(key, f);
        f
    }
}

impl Emitter {
    /// The row of closed type `ty` in `module.types`, made with the rows it holds
    /// inline; None when some part has no field type yet.
    fn type_row(&mut self, p: &Program, t: &mut impl TyRead, ty: TyId) -> Option<TypeId> {
        crate::cost_scope!("query", "type row");
        let program_type = (ty.0 as usize) < p.types.len();
        if program_type && let Some(&row) = self.program_type_rows.get(&ty) {
            return Some(row);
        }
        let row = TypeId(self.closed_row(p, t, ty)?.0);
        self.remember_generic_types(p, t, ty);
        if program_type {
            self.program_type_rows.insert(ty, row);
        }
        Some(row)
    }
    /// The place a `mut` parameter of type `ty` lends (#88): one of scalar
    /// representation (`mut n: u64`, `mut r: Region`) is a value place, as a slot's;
    /// any other is its type's row. None when that row is not built.
    fn mut_place(
        &mut self,
        p: &Program,
        t: &mut impl TyRead,
        ty: TyId,
        rep: Rep,
    ) -> Option<PlaceTy> {
        match rep {
            Rep::Scalar(s) => Some(PlaceTy::Value(s)),
            _ => self.type_row(p, t, ty).map(PlaceTy::Declared),
        }
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
            codecs: p.structural_codecs(),
            tuple_codecs: vec![],
            key_codecs: vec![],
            kinds: Default::default(),
        };
        let r = f(&mut s);
        if r.is_none() {
            self.refused = true;
            s.truncate(mark);
            self.program_type_rows.retain(|_, row| row.index() < mark);
        }
        let hooks = std::mem::take(&mut s.hooks);
        let tuples = std::mem::take(&mut s.tuple_codecs);
        let keys = std::mem::take(&mut s.key_codecs);
        for (row, hook, f, arguments) in hooks {
            // A generic row's finalizer or clone body is its template's instance at the
            // row's arguments, materialized as any other generic callee.
            let func = if arguments.is_empty() {
                if hook == crate::layout::Hook::Codec {
                    // A maker is an ordinary function: its row has its own signature.
                    match self.row(p, t, f as usize) {
                        Ok((func, _)) => func,
                        Err(_) => {
                            std::sync::Arc::make_mut(&mut self.module.types[row.index()]).codec =
                                None;
                            continue;
                        }
                    }
                } else {
                    self.hook_row(p, f as usize)
                }
            } else {
                match self.instance(p, t, f as usize, &arguments) {
                    Ok((func, _)) => func,
                    // A maker is reached only by a call of `of`, which requires the
                    // bound; a row whose maker is not built has none.
                    Err(_) if hook == crate::layout::Hook::Codec => {
                        std::sync::Arc::make_mut(&mut self.module.types[row.index()]).codec = None;
                        continue;
                    }
                    Err(_) => return None,
                }
            };
            let func = Some(hirsel_core_layout::FuncRef(func.0));
            let shape = std::sync::Arc::make_mut(&mut self.module.types[row.index()]);
            match hook {
                crate::layout::Hook::Finalizer => shape.finalizer = func,
                crate::layout::Hook::Clone => shape.clone = func,
                crate::layout::Hook::Codec => shape.codec = func,
            }
        }
        for (row, ty) in tuples {
            self.tuple_codec(p, t, ty, TypeId(row.0));
        }
        for (row, ty) in keys {
            self.ordered_codec(p, t, ty, TypeId(row.0));
        }
        r
    }
    /// The function row of the program's finalizer or clone body `f`, at its hook
    /// signature (FAIL-11, MEM-17, FORM).
    fn hook_row(&mut self, p: &Program, f: usize) -> FuncId {
        if let Some(func) = self.rows.get(&f) {
            return *func;
        }
        let sig = if p.fns[f].clone_of.is_some() {
            clone_body_signature()
        } else {
            finalizer_signature()
        };
        let func = self.function(&p.fns[f].path, sig);
        self.rows.insert(f, func);
        func
    }
    /// The recipe of row `id`, or None when the table's layout is refused, in which case
    /// the rows from `mark` on are taken back so the table stays computable.
    fn recipe(&mut self, p: &Program, id: TypeId, mark: usize) -> Option<&Recipe> {
        crate::cost_scope!("phase", "layout/drop recipes");
        if self.recipes.len() != self.module.types.len() {
            let shapes = &self.module.types;
            let names = &self.types;
            let mut layouts = p.recipe_layouts.lock().unwrap();
            match self
                .recipe_scratch
                .extend_with(shapes, &mut self.recipes, |row, make| {
                    layouts.resolve(shapes, names, row, make)
                }) {
                Ok(()) => {}
                Err(_) => {
                    self.refused = true;
                    self.module.types.truncate(mark);
                    self.types.retain(|_, r| (r.0 as usize) < mark);
                    self.program_type_rows.retain(|_, row| row.index() < mark);
                    self.generic_names.retain(|id, _| id.index() < mark);
                    self.runs.truncate(mark);
                    return None;
                }
            }
        }
        self.recipes.get(id.index()).map(|r| &**r)
    }
    /// How a value of `ty` is carried: a scalar, or a place of a fixed layout. A value
    /// whose cleanup may run a finalizer (a resource, or one held inside) is not built.
    fn rep(&mut self, p: &Program, t: &mut impl TyRead, ty: TyId) -> Result<Rep, &'static str> {
        crate::cost_scope!("phase", "layout/drop recipes");
        if let Some(scalar) = scalar_kind(ty) {
            return Ok(Rep::Scalar(Ty::Scalar(scalar)));
        }
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
        match self.recipe(p, id, mark) {
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
    /// region value, which never moves once published (MEM-5).
    fn by_block(&self, id: TypeId) -> bool {
        matches!(
            self.module.types[id.index()].repr,
            LRepr::Resource | LRepr::Region
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
    /// The byte size of a value of row `id` where it is held, whose recipe exists.
    fn size(&self, id: TypeId) -> u64 {
        held_size(
            self.module.types[id.index()].repr,
            &self.recipes[id.index()],
            false,
        )
        .expect("only fixed layouts are carried")
    }
    /// The size and alignment of row `id`'s fields laid out together (a resource's block).
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
        let mut t = Local::with_program(p);
        self.signature_inst(p, &mut t, f, &[]).map(|(sig, _)| sig)
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
        // Each source parameter's index in `params`, when it is passed by address.
        let mut at: Vec<Option<u32>> = vec![];
        for q in &fd.params {
            let ty = subst(t, q.ty, arguments);
            let rep = self.rep(p, t, ty)?;
            at.push(match (q.mode, rep) {
                (Mode::Mut, _) | (_, Rep::Place(_)) => Some(params.len() as u32),
                _ => None,
            });
            if q.mode == Mode::Mut {
                let place = self
                    .mut_place(p, t, ty, rep)
                    .ok_or("values of this type are not built")?;
                params.push(Param::Address {
                    place,
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
                        place: PlaceTy::Declared(id),
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
        // A result through `dest` anchored on parameters (OWN-20) borrows their places.
        let mut anchors: Vec<u32> = match dest {
            Some(_) => fd.anchors.iter().filter_map(|&k| at[k as usize]).collect(),
            None => vec![],
        };
        anchors.sort_unstable();
        anchors.dedup();
        Ok((
            Signature {
                params,
                results,
                dest,
                anchors,
                never,
            },
            abi,
        ))
    }
}

/// A clone body's: `(ptr from, ptr to)`, failing with `Capacity` (MEM-17).
fn clone_body_signature() -> Signature {
    Signature::new(
        vec![Param::Value(Ty::Ptr), Param::Value(Ty::Ptr)],
        vec![],
        None,
    )
}

/// The loan a by-address parameter of this source mode carries (crate docs of
/// hirsel-core-ir, "Loans"). `bytes` parameters are `u64` values, never places.
pub(super) fn loan(mode: Mode) -> Loan {
    match mode {
        Mode::Mut => Loan::Mut,
        Mode::Take => Loan::Take,
        Mode::View | Mode::Bytes | Mode::Steps => Loan::View,
    }
}

fn finalizer_signature() -> Signature {
    Signature::new(vec![Param::Value(Ty::Ptr)], vec![], None)
}

/// A field whose cleanup runs code by itself; the rows it holds go to `row`.
fn field_runs_code(t: &FieldTy, row: &mut impl FnMut(TypeId)) -> bool {
    match t {
        FieldTy::Inline(r) | FieldTy::Box(r) => {
            row(TypeId(r.0));
            false
        }
        FieldTy::Seq(e) | FieldTy::Columns(e) | FieldTy::Array(e, _) => field_runs_code(e, row),
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
        TyKind::Vector(v) => Ok(Some(Ty::Vector(*v))),
        TyKind::Fn(_, _, _, Callable::Plain) => Ok(Some(Ty::Ptr)),
        TyKind::Unit | TyKind::Never | TyKind::Error => Ok(None),
        _ => Err("values of this type are not built"),
    }
}

fn int_of(ty: TyId) -> Option<(bool, u32)> {
    Ty::Scalar(scalar_kind(ty)?).int()
}

fn is_float(ty: TyId) -> bool {
    matches!(scalar_kind(ty), Some(Scalar::F32 | Scalar::F64))
}

/// The [`hirsel_core_ir::CLOSED`] index of a core function on LANG-20's closed
/// list: reaching it refuses a constant's initialiser.
pub(crate) fn closed(path: &DefPath) -> Option<u8> {
    if !path.package_is("core", "1") {
        return None;
    }
    hirsel_core_ir::CLOSED
        .iter()
        .position(|entry| path.is_core(entry))
        .map(|i| i as u8)
}

impl Program<'_> {
    /// Check function `f`'s body and emit it into `e` (LANG-8: one visit, reading only
    /// this body, signatures and summaries).
    pub fn check_lower(&self, f: usize, e: &mut Emitter) -> CoreCheck {
        crate::cost_scope!("body", self.fns[f].path.to_string());
        let fd = &self.fns[f];
        let Some(body) = fd.body else {
            return CoreCheck::default();
        };
        crate::cost_scope!("phase", "body typing");
        let mut cx = Cx::new(self, f, e);
        cx.run(f, body);
        let mut out = cx.finish();
        if self.entry.as_ref() == Some(&fd.path) {
            self.check_entry(fd, &mut out.refusals);
            if !out.refusals.is_empty() {
                out.gaps.clear();
            }
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
        const GRANTS: [&str; 15] = [
            "Args", "Dir", "Net", "Clock", "Random", "Stdout", "Stderr", "Stdin", "Env", "Process",
            "Tasks", "Secret", "Foreign", "Exec", "Cred",
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
    /// Lexical activation that owns this slot, preserved through scoped captures.
    frame_level: u32,
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
    /// For a `mut` pattern binding, the scrutinee place it names (OWN-15).
    lent: Option<Place>,
}

/// A place and its type.
#[derive(Clone, Copy)]
struct At {
    place: Place,
    ty: TyId,
    /// Reached through a view or a region value, so never written (OWN-12).
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
    destination: Option<AddressId>,
}

struct BodyInstance {
    definition: usize,
    arguments: PoolRange<TyId>,
    hash: u64,
    row: (FuncId, SigId),
}

#[derive(Default)]
pub(crate) struct ClosedTypeFacts {
    kind: std::sync::OnceLock<Option<Kind>>,
    scoped_copies: std::sync::OnceLock<bool>,
}

impl ClosedTypeFacts {
    #[inline]
    fn kind(&self, p: &Program<'_>, ty: TyId) -> Option<Kind> {
        *self
            .kind
            .get_or_init(|| (!has_param(&p.types, ty)).then(|| kind(&p.decls, &p.types, ty, &[])))
    }
}

#[derive(Clone, Copy)]
enum GeneratedKind {
    Closure,
    TaskEntry,
    FlowTransform,
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
    versioned: bool,
    steps_in_source: bool,
    func: Option<FuncId>,
    /// The StaticFn words `code.describe` produced in this body: each one's
    /// described function, its identity static and that static's length, so a
    /// `code.identity` or `code.callable` of one reads no descriptor.
    static_fns: HashMap<ValueId, (FuncId, hirsel_core_ir::StaticId, Option<u64>)>,
    descriptor_recipes: std::collections::BTreeMap<FuncId, crate::identity::DescriptorRecipe>,
    flow_literals: HashSet<ValueId>,
    shared_flow_holds: &'c [std::sync::OnceLock<(u8, u64)>],
    local_flow_holds: Vec<Option<(u8, u64)>>,
    /// The growth path is cold (growth.rs).
    growth: growth::Growth,
    /// The signature a source body's entry was emitted against. A row first mentioned
    /// by an earlier body may already be sealed to its word carriage; installing the
    /// body puts this signature back, so the next [`Emitter::seal`] converts the body.
    emitted: Option<SigId>,
    ir_item: ItemId,
    /// The body is generated (a callable adapter): its code is decided by its
    /// identity alone, so it carries no source anchors of the body that made it.
    generated: bool,
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
    /// A place already evaluated for its node: while testing whether an ascription
    /// denotes it directly, or whether a `copy` operand is plain. The next `place` of
    /// that node reads it without repeating its indices.
    resolved_operand: Option<(NodeId, At)>,
    /// LANG-18: while a `?=>` path mapper is applied, its one argument is the
    /// error payload already in hand: the mapping node, its type and value.
    mapped_error: Option<(NodeId, TyId, Option<ValueId>)>,
    /// Block depth, and the slots of the open blocks in declaration order.
    depth: u32,
    /// The type the last expression checked synthesised, before its expected type.
    synth_ty: TyId,
    /// The synthesised type of the tail of the block being closed; unit without one.
    tail_ty: TyId,
    scope_slots: Vec<u32>,
    /// MEM-4: the call nodes where a writer call's Result may stand, found on the
    /// first writer call of the item.
    writer_sites: Option<HashSet<NodeId>>,
    step_scopes: Vec<(u32, u32)>,
    copies: Vec<CopySize>,
    allocations: Vec<AllocationLayout>,
    has_work_code: bool,
    numeric_sites: Vec<scalar_abi::NumericSite>,
    nested: std::collections::BTreeMap<FuncId, GenericTemplate>,
    generated_ordinals: [HashMap<NodeId, usize>; 3],
    call_seq: u32,
    /// The argument being checked is `list.set_lanes`'s, the one `mut` loan a
    /// column takes (MEM-24).
    column_mut: bool,
    /// The cut rows of each `Columns` drain owner (`Owner::Columns`).
    column_drains: Vec<columns::ColumnDrain>,
    /// The call whose copyable view arguments are passed by address rather than
    /// snapshotted, and the arguments so passed.
    direct_call: Option<(u32, Option<SlotId>)>,
    direct_args: Vec<(usize, TypeId, NodeId)>,
    /// The open calls whose `mut` argument loans a scalar argument's reads cross (OWN-9).
    scalar_calls: Vec<u32>,
    callable_signatures: HashMap<TyId, callable_abi::CallableAbi>,
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
    diagnostic_callee: Option<calls::DiagnosticCallee>,
    /// Completed array elements still owned separately during construction.
    array_parts: Vec<ValueId>,
    instance_rows: hashbrown::HashTable<BodyInstance>,
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
    return_place: Option<AddressId>,
    returned_local: Option<(u32, AddressId)>,
    /// The uninitialized destination offered to this particular expression.
    destination: Option<(NodeId, AddressId, bool)>,
    /// A clone body's `to` (MEM-17): its result is built in a local `dest` and moved
    /// here on `Ok`.
    clone_to: Option<ValueId>,
    /// How each type this body names is carried, found once per body.
    reps: HashMap<TyId, Result<Rep, &'static str>>,
    /// Type kinds under this body's fixed generic bounds, shared with ownership uses.
    kind_cache: HashMap<TyId, (Kind, std::cell::Cell<Option<bool>>)>,
    closed_type_facts: &'c [ClosedTypeFacts],
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
    /// Emitting failure cleanup: a failing drop appends its cause and cleanup goes on (FAIL-10).
    failing: bool,
    /// The edge whose cleanup is being emitted (FAIL-9).
    edge: Option<cleanup::Edge>,
    /// The pending result of the exit edge [`Self::unwind`] cleans next (FAIL-9).
    pending: Option<(AddressBase, TypeId)>,
    out: CoreCheck,
    capture_frame: Option<closures::CaptureFrame>,
    enclosing_frame: Option<&'e closures::EnclosingFrame<'e>>,
    /// Resolution, an enclosing closure or a callee's header already has a refusal.
    inherited_refusal: bool,
    has_callback_parameters: bool,
    frame_level: u32,
    descriptors: closures::Descriptors,
    anchors: anchors::Anchors,
    anchor_path: Vec<anchors::Component>,
    /// Binders of lists that never escape (frame_lists.rs), found at the first
    /// `let mut` of a new list.
    frame_binders: Option<HashMap<u32, bool>>,
    /// The frame bytes of each frame-backed list, by its slot.
    frame_slots: HashMap<SlotId, frame_lists::FrameList>,
    /// Literal copies in an immutable let whose binding is only viewed.
    frame_string_copies: Vec<(NodeId, u64)>,
}

/// The gap a body reports when its emission stopped with no refusal or gap to say why.
pub(crate) const SILENT_HALT: &str = "emission halted without a reported reason";

impl<'c, 'a, 'e> Cx<'c, 'a, 'e> {
    fn new(p: &'c Program<'a>, f: usize, e: &'e mut Emitter) -> Self {
        crate::cost_scope!("phase", "body setup");
        let fd = &p.fns[f];
        let item = p.item(fd.at);
        let info = &p.items[fd.at.module as usize][fd.at.item as usize];
        // A syntax refusal of this item poisons the statements it lost (PROG-8).
        let refused = !info.resolution.diagnostics.is_empty() || item.range.has_errors;
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
            t: Local::with_program(p),
            e,
            b: Body::default(),
            versioned: false,
            steps_in_source: !fd.path.package_is("core", "1"),
            func: None,
            static_fns: HashMap::default(),
            descriptor_recipes: Default::default(),
            flow_literals: HashSet::default(),
            shared_flow_holds: p.flow_holds.get_or_init(|| {
                (0..p.types.len())
                    .map(|_| std::sync::OnceLock::new())
                    .collect()
            }),
            local_flow_holds: Vec::new(),
            growth: growth::Growth::default(),
            emitted: None,
            ir_item,
            generated: false,
            cur: None,
            open: vec![],
            open_anchors: vec![],
            // Keep checking independent constructs without emitting through
            // unresolved types, patterns or captures.
            broken: refused,
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
            resolved_operand: None,
            mapped_error: None,
            depth: 0,
            synth_ty: UNIT,
            tail_ty: UNIT,
            scope_slots: vec![],
            writer_sites: None,
            step_scopes: vec![],
            copies: vec![],
            allocations: vec![],
            has_work_code: false,
            numeric_sites: vec![],
            nested: Default::default(),
            generated_ordinals: Default::default(),
            call_seq: 0,
            column_mut: false,
            column_drains: Vec::new(),
            direct_call: None,
            direct_args: vec![],
            scalar_calls: vec![],
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
            diagnostic_callee: None,
            array_parts: vec![],
            instance_rows: hashbrown::HashTable::new(),
            instance_args: vec![],
            binder_at,
            frames: vec![],
            joins: vec![],
            synth: vec![0; (item.range.nodes.end - item.range.nodes.start) as usize],
            waited: false,
            entry: fd.is_test || p.entry.as_ref() == Some(&fd.path),
            dest: None,
            return_place: None,
            returned_local: None,
            destination: None,
            clone_to: None,
            reps: HashMap::default(),
            kind_cache: HashMap::default(),
            closed_type_facts: p.closed_type_facts.get_or_init(|| {
                (0..p.types.len())
                    .map(|_| ClosedTypeFacts::default())
                    .collect()
            }),
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
            enclosing_frame: None,
            inherited_refusal: refused,
            has_callback_parameters: false,
            frame_level: 0,
            descriptors: closures::Descriptors::default(),
            anchors: anchors::Anchors::new(info.resolution.binders.len()),
            anchor_path: vec![],
            frame_binders: None,
            frame_slots: HashMap::default(),
            frame_string_copies: Vec::new(),
        }
    }

    fn generated_name(&mut self, kind: GeneratedKind, site: NodeId) -> String {
        // First visits follow the body's lowering order.
        // A syntax node is only a key for recognizing a repeat visit.
        let ordinals = &mut self.generated_ordinals[kind as usize];
        let next = ordinals.len();
        let ordinal = ordinals.entry(site).or_insert(next);
        let suffix = match kind {
            GeneratedKind::Closure => "$closure",
            GeneratedKind::TaskEntry => "$task_entry",
            GeneratedKind::FlowTransform => "$flow_transform",
        };
        format!("{}{suffix}{ordinal}", self.fd.path.0.name)
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
    /// `ty` as a refusal shows it, each region root by its source name (PROG-7).
    fn shown(&self, ty: TyId) -> String {
        self.shown_in(ty, &self.fd)
    }

    fn shown_in(&self, ty: TyId, function: &FnDecl) -> String {
        let symbols = &self.p.index.symbols;
        let binders = &self.p.items[self.at.module as usize][self.at.item as usize]
            .resolution
            .binders;
        let root = |r: &Root, s: &mut String| {
            let name = match *r {
                Root::Parameter(i) => function.params.get(i as usize).map(|q| q.name),
                Root::Generic(i) => function.generics.get(i as usize).map(|g| g.name),
                Root::Local(b) => binders.get(b as usize).map(|b| b.name),
                // A callable type's parameters are unnamed: its root is a position.
                Root::CallableParameter { index, .. } => {
                    s.push_str(&format!("in callable parameter {}", index + 1));
                    return;
                }
                Root::Enclosing => None,
            };
            s.push_str("in ");
            s.push_str(name.map_or("r", |n| symbols.text(n)));
        };
        crate::decls::shown(&self.p.decls, &self.t, ty, &root)
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
        causal: Vec<Cause>,
        message: impl Into<String>,
    ) {
        let mut r = refusal(rule, self.p.site_span(self.at, span), message);
        r.causal = causal;
        self.push_refusal(r);
    }
    /// A refusal with its compared facts: what the program has at `n` and what the
    /// rule requires there (PROG-7).
    fn refuse_compared(
        &mut self,
        rule: &str,
        n: NodeId,
        declared: impl Into<String>,
        required: impl Into<String>,
        message: impl Into<String>,
    ) {
        self.refuse_compared_at(rule, self.span(n), declared, required, message);
    }
    fn refuse_compared_at(
        &mut self,
        rule: &str,
        span: Span,
        declared: impl Into<String>,
        required: impl Into<String>,
        message: impl Into<String>,
    ) {
        let site = self.p.site_span(self.at, span);
        self.push_refusal(refusal(rule, site, message).with_comparison(declared, required));
    }
    /// LANG-11 for a call or construction with the wrong number of arguments.
    fn refuse_arity_at(&mut self, span: Span, written: usize, want: usize, message: String) {
        let count = |k| crate::decls::counted(k, "argument");
        self.refuse_compared_at("LANG-11", span, count(written), count(want), message);
    }
    /// LANG-11 for an argument that determines a type parameter differently from an
    /// earlier argument: the argument's type against the parameter type as the earlier
    /// arguments determined it.
    fn refuse_twice(&mut self, e: NodeId, found: TyId, before: TyId, message: &str) {
        let (found, before) = (self.shown(found), self.shown(before));
        self.refuse_compared("LANG-11", e, found, before, message);
    }
    /// LANG-12 for a type written with the wrong number of type arguments.
    fn refuse_type_arguments(&mut self, n: NodeId, written: usize, want: usize, message: &str) {
        let count = |k| crate::decls::counted(k, "type argument");
        self.refuse_compared("LANG-12", n, count(written), count(want), message);
    }
    /// LANG-3 for a literal that its type does not take: the literal as written, with
    /// the `-` it is directly under, against the type.
    fn refuse_literal(&mut self, n: NodeId, negate: bool, ty: String, message: String) {
        self.refuse_literal_as("LANG-3", n, negate, ty, message);
    }
    fn refuse_literal_as(
        &mut self,
        rule: &str,
        n: NodeId,
        negate: bool,
        ty: String,
        message: String,
    ) {
        let written = format!("{}{}", if negate { "-" } else { "" }, self.token_text(n));
        self.refuse_compared(rule, n, written, ty, message);
    }
    /// An argument's written mode mark as an OWN-7 fact: `mut`, `view`, `take`, `bytes`,
    /// or `unmarked`.
    fn written_mode(&self, a: NodeId) -> &'static str {
        match self.kind(a) {
            NodeKind::BytesArgument => "bytes",
            NodeKind::StepsArgument => "steps",
            NodeKind::Unary => match self.token_text(a) {
                "mut" => "mut",
                "view" => "view",
                "take" => "take",
                _ => "unmarked",
            },
            _ => "unmarked",
        }
    }
    /// LANG-9 for a construction argument against its field: the argument as written
    /// (`label: ` or positional) against the field's form, labelled `name:` when
    /// `field` names it, positional otherwise.
    fn refuse_label(&mut self, a: NodeId, field: Option<&str>, message: String) {
        let written = if self.kind(a) == NodeKind::LabelledArgument {
            format!("{}:", self.token_text(a))
        } else {
            "positional".to_owned()
        };
        let required = field.map_or("positional".to_owned(), |f| format!("{f}:"));
        self.refuse_compared("LANG-9", a, written, required, message);
    }
    fn push_refusal(&mut self, mut r: Refusal) {
        crate::cost_scope!("phase", "diagnostics/gaps");
        if r.rule == "LANG-11"
            && let Some(callee) = self.diagnostic_callee
        {
            r.message.push_str(". Expected signature: ");
            r.message.push_str(&self.shown_callee(callee));
        }
        // PROG-8: a construct the resolver refused is not refused again by a
        // later check of the same site.
        let resolved = &self.p.items[self.at.module as usize][self.at.item as usize].resolution;
        let restated = resolved.diagnostics.iter().any(|d| {
            d.rule == r.rule
                && (d.primary.module, d.primary.item, d.primary.span.start)
                    == (r.primary.module, r.primary.item, r.primary.span.start)
        });
        if !restated {
            self.out.refusals.push(r);
        }
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
        // MEM-6: a published `T in a` is usable as `T in r` when `a` outlives `r`.
        match self.region_fit(t, w.ty) {
            Some(true) => return,
            Some(false) => {
                let (a, b) = (self.shown(w.ty), self.shown(t));
                let message =
                    format!("expected `{a}`, found `{b}`, whose region does not outlive it");
                self.refuse_compared("MEM-6", n, b, a, message);
                return;
            }
            None => {}
        }
        let plain_view = self.plain_view(t, w.ty);
        let viewed = w.view
            && (matches!(self.t.kind(t), TyKind::View(x) if *x == w.ty)
                || crate::decls::stable_parameter(&self.p.decls, &self.t, t, w.ty));
        let scoped_callable = self.scoped_callable_view(t, w.ty);
        let callable = matches!((self.t.kind(t), self.t.kind(w.ty)),
            (TyKind::Fn(a, ar, aw, ak), TyKind::Fn(b, br, bw, bk))
                if a == b && ar == br && (ak == bk || matches!((ak,bk), (Callable::OwnSend, Callable::Own) | (Callable::ScopedView, Callable::Scoped))) && (!aw || *bw))
            || scoped_callable
            || self.plain_callable_fit(t, w.ty);
        if !callable
            && matches!(
                (self.t.kind(t), self.t.kind(w.ty)),
                (TyKind::Fn(..), TyKind::Fn(..))
            )
        {
            let (a, b) = (self.shown(t), self.shown(w.ty));
            let message = "the callable does not fit the written callable type";
            self.refuse_compared("LANG-13", n, a, b, message);
            return;
        }
        let sequence = matches!((self.t.kind(w.ty), self.t.kind(t)),
            (TyKind::Adt(d, a), TyKind::Adt(e, b))
                if a == b && is_core_prelude(&self.p.decls[d.index()].path, &["SeqSpan"])
                    && is_core_prelude(&self.p.decls[e.index()].path, &["IndexedSpan"]));
        // OWN-25: a region value is read where a view of its type is expected.
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
            let (a, b) = (self.shown(w.ty), self.shown(t));
            let message = format!("expected `{a}`, found `{b}`");
            let previous = self.diagnostic_callee;
            if self.kind(n) == NodeKind::Call
                && let Some(callee) = self.named_callee(self.kids(n)[0])
            {
                self.diagnostic_callee = Some(calls::DiagnosticCallee::Named(callee));
            }
            self.refuse_compared("LANG-11", n, b, a, message);
            self.diagnostic_callee = previous;
        }
    }

    /// MEM-6: whether `from` differs from `to` only in region roots, in an anchored
    /// type's root, a type's region arguments, or inside `Option` and tuples; then
    /// whether each of `from`'s roots there outlives `to`'s. `None` when the types
    /// differ otherwise, or a differing root is not a region.
    fn region_fit(&self, from: TyId, to: TyId) -> Option<bool> {
        if from == to {
            return Some(true);
        }
        match (self.t.kind(from), self.t.kind(to)) {
            (TyKind::Region(a), TyKind::Region(b)) => {
                let (a, b) = (*a, *b);
                Some(
                    self.region_root(a)?
                        && self.region_root(b)?
                        && self.outlives_root(a, b, from, to),
                )
            }
            (TyKind::Anchored(x, a), TyKind::Anchored(y, b)) => {
                let (x, a, y, b) = (*x, *a, *y, *b);
                Some(self.region_fit(x, y)? & self.region_fit(a, b)?)
            }
            (TyKind::Tuple(xs), TyKind::Tuple(ys)) if xs.len() == ys.len() => {
                let mut ok = true;
                for (&x, &y) in xs.iter().zip(ys) {
                    ok &= self.region_fit(x, y)?;
                }
                Some(ok)
            }
            (TyKind::Adt(d, xs), TyKind::Adt(e, ys)) if d == e && xs.len() == ys.len() => {
                let option = is_core_prelude(&self.p.decls[d.index()].path, &["Option"]);
                let mut ok = true;
                for (&x, &y) in xs.iter().zip(ys) {
                    if x == y {
                        continue;
                    }
                    let region = matches!(self.t.kind(x), TyKind::Region(_))
                        && matches!(self.t.kind(y), TyKind::Region(_));
                    if !(option || region) {
                        return None;
                    }
                    ok &= self.region_fit(x, y)?;
                }
                Some(ok)
            }
            _ => None,
        }
    }
    /// Whether `root` names a region: a region generic, a `Region` parameter or a
    /// local bound to a `Region` (MEM-6). Other roots anchor to places (OWN-20).
    fn region_root(&self, root: Root) -> Option<bool> {
        let region = match root {
            Root::Generic(i) => self.fd.generics.get(i as usize).is_some_and(|g| g.region),
            Root::Parameter(i) => self
                .fd
                .params
                .get(i as usize)
                .is_some_and(|p| self.region_handle(p.ty)),
            Root::Local(slot) => self
                .vars
                .get(slot as usize)
                .copied()
                .flatten()
                .is_some_and(|v| self.region_handle(v.ty)),
            Root::Enclosing | Root::CallableParameter { .. } => false,
        };
        region.then_some(true)
    }
    /// MEM-6 outlives, from the lexical nesting and the signature only: `a` is `b`;
    /// a signature region outlives every region the body opens; a region opened in
    /// an enclosing block outlives one opened inside it; `where` edges, transitively.
    fn outlives_root(&self, a: Root, b: Root, a_ty: TyId, b_ty: TyId) -> bool {
        if a == b {
            return true;
        }
        let signature = |r: Root| matches!(r, Root::Generic(_) | Root::Parameter(_));
        match (a, b) {
            (Root::Local(x), Root::Local(y)) => {
                let (Some(x), Some(y)) = (self.vars[x as usize], self.vars[y as usize]) else {
                    return false;
                };
                // Both are in scope here, so the shallower one's block encloses the other's.
                (x.frame_level, x.depth) < (y.frame_level, y.depth)
            }
            (a, Root::Local(_)) => signature(a),
            _ if signature(a) && signature(b) => {
                let mut seen = vec![a_ty];
                let mut i = 0;
                while i < seen.len() {
                    if seen[i] == b_ty {
                        return true;
                    }
                    for &(p, q) in &self.fd.outlives {
                        if p == seen[i] && !seen.contains(&q) {
                            seen.push(q);
                        }
                    }
                    i += 1;
                }
                false
            }
            _ => false,
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
    fn anchor(&self, n: NodeId) -> Option<Anchor> {
        let s = self.item.tree.node(n).origin.span;
        (!self.generated).then_some(Anchor {
            item: self.ir_item,
            start: s.start,
            end: s.end,
        })
    }
    fn value(&mut self, t: Ty) -> ValueId {
        self.b.values.push(t);
        ValueId(self.b.values.len() as u32 - 1)
    }
    #[inline(always)]
    fn inst(&mut self, i: Inst, n: NodeId) {
        crate::cost_scope!("phase", "CoreIR emission");
        if self.live() {
            if let Inst::Call { call } = i
                && self.growth.cold
            {
                self.b.calls[call.index()].cold = true;
            }
            self.open.push(i);
            self.open_anchors.push(self.anchor(n));
        }
    }
    pub(super) fn callback_step_to(&mut self, n: NodeId, fail: Option<BlockId>) {
        self.inst(Inst::Step { fail }, n);
    }
    fn konst(&mut self, t: Ty, bits: u64, n: NodeId) -> Option<ValueId> {
        if !self.live() {
            return None;
        }
        let result = self.value(t);
        if bits == 1 && t == Ty::Scalar(Scalar::U64) {
            self.growth.recent_one = Some(result);
        }
        self.inst(Inst::Const { result, bits }, n);
        Some(result)
    }
    /// How a value of `ty` is carried in this body.
    fn rep(&mut self, ty: TyId) -> Result<Rep, &'static str> {
        if let Some(scalar) = scalar_kind(ty) {
            return Ok(Rep::Scalar(Ty::Scalar(scalar)));
        }
        *self
            .reps
            .entry(ty)
            .or_insert_with(|| self.e.rep(self.p, &mut self.t, ty))
    }
    /// How a value of `ty` is carried, or the gap that says why it is not built: an
    /// emit step never ends on a representation without naming it (FORM #120).
    fn rep_at(&mut self, ty: TyId, n: NodeId) -> Option<Rep> {
        self.rep(ty).map_err(|what| self.gap(n, what)).ok()
    }
    /// The row of a value of `ty` held in a place, or the gap `what` (FORM #120).
    fn place_row(&mut self, ty: TyId, what: &'static str, n: NodeId) -> Option<TypeId> {
        match self.rep_at(ty, n)? {
            Rep::Place(id) => Some(id),
            _ => {
                self.gap(n, what);
                None
            }
        }
    }
    /// `found`, or the gap `what` when it is absent: an emit step that needs it names
    /// why it stops (FORM #120).
    fn named<T>(&mut self, found: Option<T>, what: &'static str, n: NodeId) -> Option<T> {
        if found.is_none() {
            self.gap(n, what);
        }
        found
    }
    /// The prelude declaration `name` a built-in is made of, or the gap naming its
    /// absence (FORM #120).
    fn prelude(&mut self, name: &str, n: NodeId) -> Option<crate::types::DeclId> {
        let found = self.p.core_decl(name);
        self.named(found, "a built-in without the core prelude is not built", n)
    }
    /// The place a stored value of `ty` takes, or the gap `what`: a zero-sized value
    /// takes none, so a step that needs one names it (FORM #120).
    fn stored_at(&mut self, ty: TyId, what: &'static str, n: NodeId) -> Option<PlaceTy> {
        let stored = self.rep_at(ty, n)?.stored();
        if stored.is_none() {
            self.gap(n, what);
        }
        stored
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
        self.note_pointer(result, address);
        result
    }
    /// A held value's native stride or portable logical size. Rigid parameters
    /// retain the kind of size until mechanical instance materialization.
    pub(super) fn layout_word(&mut self, query: LayoutQuery, n: NodeId) -> ValueId {
        let result = self.value(Ty::Scalar(Scalar::U64));
        self.inst(Inst::LayoutConst { result, query }, n);
        result
    }
    pub(super) fn layout_size(&mut self, ty: TyId, logical: bool, n: NodeId) -> Option<ValueId> {
        let (id, bits) = match self.rep(ty) {
            Ok(Rep::Scalar(Ty::Scalar(s))) => (None, s.size()),
            Ok(Rep::Scalar(Ty::Ptr)) => (None, hirsel_core_layout::POINTER),
            Ok(Rep::Scalar(Ty::Vector(v))) => (None, v.size()),
            Ok(Rep::None) => (None, 0),
            Ok(Rep::Place(id)) => {
                if !logical {
                    return Some(self.native_size(id, n));
                }
                let bits = held_size(
                    self.e.module.types[id.index()].repr,
                    &self.e.recipes[id.index()],
                    logical,
                )
                .expect("only fixed layouts are carried");
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

    fn native_size(&mut self, id: TypeId, n: NodeId) -> ValueId {
        self.layout_word(LayoutQuery::HeldSize(id), n)
    }

    /// Copy the value of row `id` that `from` points at into place `to` (a move or a
    /// copy of plain data: the source needs no cleanup afterwards).
    fn copy_to(&mut self, to: AddressId, from: ValueId, id: TypeId, n: NodeId) {
        if self.e.needs_drop(id) {
            self.consume(Some(from), n);
        }
        self.copy_bytes_to(to, from, id, n);
    }

    /// Copy carrier bytes while leaving their owner in place.
    fn copy_bytes_to(&mut self, to: AddressId, from: ValueId, id: TypeId, n: NodeId) {
        if self.pointer_address(from) == Some(to) {
            return;
        }
        let from = self.address(AddressBase::Value(from), &[], PlaceTy::Declared(id), n);
        let bytes = self.native_size(id, n);
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
        match r {
            Rep::Scalar(t) => {
                let at = self.address(AddressBase::Value(pointer), &[], PlaceTy::Value(t), n);
                let value = self.read_bits(at, t, n);
                self.inst(Inst::Store { address: to, value }, n);
            }
            Rep::Place(id) if self.live() => self.copy_bytes_to(to, pointer, id, n),
            Rep::None | Rep::Place(_) => {}
        }
        Some(())
    }
    pub(super) fn snapshot(&mut self, from: ValueId, id: TypeId, n: NodeId) -> ValueId {
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
                let to = self.return_place.unwrap_or_else(|| {
                    self.address(AddressBase::Value(dest), &[], PlaceTy::Declared(id), n)
                });
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
    /// A clone body's return (MEM-17): the copy moves to `to`. A refusal in the body
    /// raised `Capacity` and never reaches here.
    fn clone_return(&mut self, to: ValueId, n: NodeId) {
        let Some((dest, rid)) = self.dest else {
            return self.gap(n, "this clone body's result is not built");
        };
        let copy = self.address(AddressBase::Value(dest), &[], PlaceTy::Declared(rid), n);
        let copy = self.pointer(copy, n);
        let target = self.address(AddressBase::Value(to), &[], PlaceTy::Declared(rid), n);
        self.copy_to(target, copy, rid, n);
        let r = PoolRange::push(&mut self.b.pools.values, []);
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
            decision: false,
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
        self.end_block(term, n, false);
    }
    /// End the block with a source decision at `n` (`Block::decision`).
    pub(super) fn decide(&mut self, term: Terminator, n: NodeId) {
        self.end_block(term, Some(n), true);
    }
    fn end_block(&mut self, term: Terminator, n: Option<NodeId>, decision: bool) {
        crate::cost_scope!("phase", "CoreIR emission");
        let Some(c) = self.cur.take() else { return };
        let insts = PoolRange::new(self.b.insts.len() as u32, self.open.len() as u32);
        self.b.insts.append(&mut self.open);
        self.b.inst_anchors.append(&mut self.open_anchors);
        let anchor = n.and_then(|n| self.anchor(n));
        let blk = &mut self.b.blocks[c.index()];
        blk.insts = insts;
        blk.decision = decision && anchor.is_some();
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
        self.b.edges.push(Edge {
            target,
            args,
            cold: false,
        });
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
    // ---- the definition
    fn run(&mut self, f: usize, body: NodeId) {
        let row = self.e.row(self.p, &mut self.t, f);
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
                if let Some(i) = closed(&self.fd.path) {
                    self.runtime_row(hirsel_core_ir::RtOp::Closed(i), &[], body);
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
        let mut written_parameters = vec![];
        for (parameter_index, pd) in fd.params.iter().enumerate() {
            let carried = self.ir_ty(pd.ty).is_some() || pd.mode == Mode::Mut;
            let v = (carried && k < entry_params.len()).then(|| {
                k += 1;
                entry_params[k - 1]
            });
            let mut callable_ty = pd.ty;
            while let TyKind::Anchored(inner, _) | TyKind::View(inner) = self.t.kind(callable_ty) {
                callable_ty = *inner;
            }
            if pd.mode != Mode::Mut && matches!(self.t.kind(callable_ty), TyKind::Fn(..)) {
                self.b.callable_parameters.extend(v);
            }
            if let Some(b) = binders.iter().find(|b| {
                b.name == pd.name
                    && b.kind == hirsel_core_resolve::BinderKind::Parameter
                    && pd.site.span.start <= b.site.span.start
                    && b.site.span.end <= pd.site.span.end
            }) {
                let origin = match pd.mode {
                    Mode::View | Mode::Bytes | Mode::Steps => Origin::ViewParam,
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
                self.has_callback_parameters |= self.contains_callback(pd.ty);
                self.parameter_anchors(b.id, pd.ty, pd.mode, parameter_index as u32);
                self.own(b.id, body);
                written_parameters.push((parameter_index, b.id, pd.ty, pd.mode));
            }
        }
        // A formal whose written type names anchors (`take b: Body in input`) has
        // that anchor set (OWN-19), which may name a later parameter.
        for (index, slot, ty, mode) in written_parameters {
            self.written_parameter_anchors(index, slot, ty, mode);
        }
        // A refused signature type leaves nothing to bind: check the body, build none.
        if mentions_error(&self.t, self.fd.callable) {
            self.inherited_refusal = true;
            self.halt();
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
        // FAIL-17: an activation of source code is refused at its entry, after
        // its parameters are bound and before any of its code runs.
        if self.steps_in_source && self.live() {
            let fail = self.fail_to(body);
            self.inst(Inst::StackCheck { fail }, body);
        }
        if self.fd.drop_of.is_some() || self.fd.clone_of.is_some() {
            self.source_step(body);
        }
        if let Some((dest, id)) = self.dest
            && !self.is_view(self.fd.result)
            // A rigid numeric result has only a template destination. Every
            // closed signature returns it as a scalar.
            && !matches!(*self.t.kind(self.fd.result), TyKind::Param(i)
                if self.params[i as usize] & (INTEGER | FLOAT) != 0)
        {
            self.return_place =
                Some(self.address(AddressBase::Value(dest), &[], PlaceTy::Declared(id), body));
        }
        let v = self.return_operand(body);
        self.ret(v, body);
        self.frame_ceiling();
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
            let message = "declared `waits` but calls nothing that waits";
            self.refuse_compared("LANG-8", w, "waits", "not waits", message);
        }
    }
    fn finish(mut self) -> CoreCheck {
        crate::cost_scope!("phase", "body finish");
        self.out.flow.waits = self.fd.waits;
        // The resolver's and the declarations' refusals of this item are reported.
        let item = self.p.site(self.at, self.header);
        let refused = self.inherited_refusal
            || !self.out.refusals.is_empty()
            || self
                .p
                .refusals
                .iter()
                .any(|r| (r.primary.module, r.primary.item) == (item.module, item.item));
        // PROG-8: a refused definition is never built, so none of its constructs is a
        // gap in the build; a gap there only restates the refusal.
        if refused {
            self.out.gaps.clear();
        }
        if let Some(n) = self.halted
            && !refused
            && self.out.gaps.is_empty()
            && let site = self.p.site(self.at, n)
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
                scalar_abi::finish_closed(&self.e.module, &mut self.b, self.has_work_code);
            }
            let function = &mut self.e.module.functions[f.index()];
            function.versioned |= self.versioned;
            function.cold = self.fd.cold;
            function.body = Some(std::mem::take(&mut self.b));
            if let Some(sig) = self.emitted {
                self.e.install_source_signature(f, sig);
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
                let ids = std::iter::once(self.e.source_signature(f))
                    .chain(body.calls.iter().map(|c| c.signature))
                    .chain(body.insts.iter().filter_map(|i| match i {
                        Inst::FunctionRef { function, .. }
                        | Inst::DescriptorRef { function, .. } => {
                            Some(self.e.source_signature(*function))
                        }
                        _ => None,
                    }));
                for id in ids {
                    if let Some(abi) = self.e.source_signatures.abis.get(&id) {
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
                    descriptor_adapter: None,
                    descriptors: std::mem::take(&mut self.descriptor_recipes),
                    types: self.e.generic_names.clone(),
                    calls: self.e.generic_calls.clone(),
                    codecs: self.e.generic_codecs.clone(),
                    key_codecs: self.e.generic_key_codecs.clone(),
                };
                self.out.template = Some(template);
            }
        }
        self.out
    }

    // ---- expressions
    fn can_construct(&self, n: NodeId) -> bool {
        match self.kind(n) {
            NodeKind::Call | NodeKind::Tuple | NodeKind::Array | NodeKind::RepeatArray => true,
            NodeKind::Group | NodeKind::Ascription => self.can_construct(self.kids(n)[0]),
            NodeKind::Block => self
                .block_tail(n)
                .is_some_and(|tail| self.can_construct(tail)),
            NodeKind::If => self.kids(n)[1..].iter().any(|&arm| self.can_construct(arm)),
            NodeKind::Match => self.kids(n)[1..]
                .iter()
                .any(|&arm| self.can_construct(*self.kids(arm).last().unwrap())),
            NodeKind::Path => matches!(
                self.p.target(self.at, n),
                Some(Target::ContextualConstructor { .. })
            ),
            _ => false,
        }
    }

    /// A partial aggregate in the result place must not overlap another return
    /// from one of its fields. A nested function has its own result place.
    fn may_return(&self, n: NodeId) -> bool {
        match self.kind(n) {
            NodeKind::Return | NodeKind::Try => true,
            NodeKind::Closure | NodeKind::ScopedClosure => false,
            _ => self.kids(n).iter().any(|&child| self.may_return(child)),
        }
    }

    fn returned_local(&self, body: NodeId) -> Option<u32> {
        let tail = self.block_tail(body)?;
        let local = self.returned_binding(tail)?;
        let stmts = self.kids(body);
        let start = stmts.iter().position(|&stmt| {
            matches!(self.kind(stmt), NodeKind::Let | NodeKind::LetMut)
                && self.binder(self.kids(stmt)[0]) == Some(local)
        })?;
        stmts[start..]
            .iter()
            .all(|&stmt| self.returns_only(stmt, local))
            .then_some(local)
    }

    fn block_tail(&self, n: NodeId) -> Option<NodeId> {
        let &tail = self.kids(n).last()?;
        (self.kind(tail) == NodeKind::ExpressionStatement).then(|| self.kids(tail)[0])
    }

    fn returned_binding(&self, n: NodeId) -> Option<u32> {
        match self.kind(n) {
            NodeKind::Return | NodeKind::Group => self.returned_binding(*self.kids(n).first()?),
            NodeKind::Unary if self.token_text(n) == "take" => {
                self.returned_binding(self.kids(n)[0])
            }
            NodeKind::Block => self.returned_binding(self.block_tail(n)?),
            NodeKind::If if self.kids(n).len() == 3 => {
                let local = self.returned_binding(self.kids(n)[1])?;
                (self.returned_binding(self.kids(n)[2]) == Some(local)).then_some(local)
            }
            NodeKind::Match => {
                let mut arms = self.kids(n)[1..].iter();
                let arm_result = |&arm| self.returned_binding(*self.kids(arm).last()?);
                let local = arm_result(arms.next()?)?;
                arms.all(|arm| arm_result(arm) == Some(local))
                    .then_some(local)
            }
            NodeKind::Path => match self.p.resolved(self.at, n)? {
                use_ if !use_.projections.is_empty() => None,
                use_ => match use_.target {
                    Target::Binder(id) => Some(id),
                    _ => None,
                },
            },
            _ => None,
        }
    }

    fn returns_only(&self, n: NodeId, local: u32) -> bool {
        match self.kind(n) {
            NodeKind::Return => {
                self.returned_binding(n) == Some(local)
                    && self
                        .kids(n)
                        .iter()
                        .all(|&child| self.returns_only(child, local))
            }
            NodeKind::Try => false,
            NodeKind::Closure | NodeKind::ScopedClosure => true,
            _ => self
                .kids(n)
                .iter()
                .all(|&child| self.returns_only(child, local)),
        }
    }

    fn return_operand(&mut self, n: NodeId) -> Option<ValueId> {
        self.expr_into(n, expect(self.fd.result), self.return_place)
            .1
    }

    fn expression_destination(&self, n: NodeId) -> Option<AddressId> {
        self.destination
            .filter(|&(node, _, _)| node == n)
            .map(|(_, to, _)| to)
    }

    fn fresh_destination_root(&self, n: NodeId) -> Option<SlotId> {
        let (node, mut address, fresh) = self.destination?;
        if node != n || !fresh {
            return None;
        }
        loop {
            address = match self.b.addresses[address.index()].base {
                AddressBase::Slot(slot) => return Some(slot),
                AddressBase::Address(base) => base,
                AddressBase::Value(value) => self.pointer_address(value)?,
            };
        }
    }

    // The enclosing recipe may describe the field as a carrier or holder.
    // Its constructor uses the field value's own declared recipe.
    fn field_destination(
        &mut self,
        base: AddressId,
        steps: &[Step],
        ty: TypeId,
        n: NodeId,
    ) -> AddressId {
        let ty = PlaceTy::Declared(ty);
        let projected = self.address(AddressBase::Address(base), steps, ty, n);
        let pointer = self.pointer(projected, n);
        self.address(AddressBase::Value(pointer), &[], ty, n)
    }

    fn result_slot(&mut self, ty: PlaceTy, n: NodeId) -> AddressId {
        self.expression_destination(n)
            .filter(|to| self.b.addresses[to.index()].ty == ty)
            .unwrap_or_else(|| self.new_slot(ty, n))
    }

    fn expr_into(
        &mut self,
        n: NodeId,
        want: Option<Want>,
        to: Option<AddressId>,
    ) -> (TyId, Option<ValueId>) {
        self.expr_into_field(n, want, to, false)
    }

    fn expr_into_field(
        &mut self,
        n: NodeId,
        want: Option<Want>,
        to: Option<AddressId>,
        fresh: bool,
    ) -> (TyId, Option<ValueId>) {
        let outer = std::mem::replace(&mut self.destination, to.map(|to| (n, to, fresh)));
        let result = self.expr(n, want);
        self.destination = outer;
        result
    }

    fn result_expr(&mut self, n: NodeId, want: Option<Want>) -> (TyId, Option<ValueId>) {
        self.expr_into(n, want, self.expression_destination(self.node))
    }

    /// A partial expectation guides the expression's parts. The expression
    /// and its joins keep the type they find for the enclosing call to unify.
    fn fixed_want(&self, want: Want) -> bool {
        want.payload == 0 && !has_hole(&self.t, want.ty)
    }

    /// Check `n` against `want` when given; a construct that passes `want` inward checks
    /// it there and returns `want`'s type, so the outer check here passes.
    fn expr(&mut self, n: NodeId, want: Option<Want>) -> (TyId, Option<ValueId>) {
        self.out.counts.nodes += 1;
        let want = want.filter(|w| w.ty != HOLE);
        let (mut t, mut v) = self.checked(n, want);
        let want = want.filter(|w| w.payload != 0 || self.fixed_want(*w));
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
            self.fit_scoped_descriptor(t, v, n);
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
                if self.plain_view(t, w.ty) {
                    return (w.ty, self.read_plain_view(w.ty, v, n));
                }
                // A view kept for a view parameter (OWN-7): a join carrying it owns nothing.
                let kept = w.view && matches!(self.t.kind(t), TyKind::View(x) if *x == w.ty);
                (if kept { t } else { w.ty }, v)
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
        if let Some((node, ty, value)) = self.mapped_error
            && node == n
        {
            // The mapper's argument is the error payload; it is owned here until
            // the mapper takes it.
            self.mapped_error = None;
            self.temp(ty, value);
            return (ty, value);
        }
        let outer = std::mem::replace(&mut self.node, n);
        let (t, v) = self.expr_inner(n, want);
        if !self.versioned && t > HOLE && matches!(self.t.kind(t), TyKind::Vector(_)) {
            self.versioned = true;
        }
        self.flow_expression(n, t);
        if matches!(self.kind(n), NodeKind::Closure | NodeKind::ScopedClosure)
            && let Some(v) = v
        {
            self.flow_literals.insert(v);
        }
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
            NodeKind::Group => self.result_expr(c[0], want),
            NodeKind::Ascription => {
                let ty = self.ty(c[1]);
                if ty == ERROR {
                    return (ERROR, None);
                }
                let to = self.destination.filter(|&(node, to, _)| {
                    node == n
                        && matches!(self.rep(ty), Ok(Rep::Place(id))
                            if self.b.addresses[to.index()].ty == PlaceTy::Declared(id))
                });
                let (address, fresh) = to.map_or((None, false), |(_, to, fresh)| (Some(to), fresh));
                self.expr_into_field(c[0], expect(ty), address, fresh)
            }
            NodeKind::Literal => self.literal(n, want, false),
            NodeKind::BytesOf => self.bytes_of(n, c[0]),
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
            NodeKind::Closure | NodeKind::ScopedClosure => self.closure(n, want),
            _ => self.gap_ty(n, "expression form is not built"),
        }
    }
    /// A place in a value position: moved when its type moves, else copied (OWN-6).
    fn place_value(&mut self, n: NodeId, want: Option<Want>) -> (TyId, Option<ValueId>) {
        match self.place(n) {
            Ok(Some(at)) => {
                if self.var(at.place.slot).origin == Origin::MutParam
                    && matches!(
                        self.t.kind(at.ty),
                        TyKind::Fn(_, _, _, Callable::Scoped | Callable::ScopedView)
                    )
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
                    let value = self.place_ptr(at, n);
                    self.fit_scoped_descriptor(at.ty, value, n);
                    return (w.ty, value);
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

#[cfg(test)]
mod row_cache_tests;
