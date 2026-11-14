//! Types of the check: one interner per program for declarations and signatures, and
//! a body-local overlay for the types a body makes, so bodies check independently.
//! A type parameter is `Param(i)`, the i-th generic of the definition being read; it
//! is rigid (OWN-23) and means nothing outside that definition.

use hirsel_core_ids::published::{Map, Publisher, Table};
use hirsel_core_layout::Scalar;
use rustc_hash::FxHashMap as HashMap;
use std::hash::BuildHasher;

#[derive(
    Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd, serde::Serialize, serde::Deserialize,
)]
pub struct TyId(pub u32);

/// Index of a type declaration in [`crate::Program::decls`].
#[derive(
    Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd, serde::Serialize, serde::Deserialize,
)]
pub struct DeclId(pub u32);
impl DeclId {
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// A parameter mode (OWN-7).
#[derive(
    Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum Mode {
    View,
    Mut,
    Take,
    Bytes,
}

impl Mode {
    /// The mode as source writes it, a compared fact of a mode refusal (PROG-7).
    pub fn keyword(self) -> &'static str {
        match self {
            Mode::View => "view",
            Mode::Mut => "mut",
            Mode::Take => "take",
            Mode::Bytes => "bytes",
        }
    }
}

/// A root in a published signature or a body. Identities are local to the definition.
#[derive(
    Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum Root {
    Generic(u32),
    Parameter(u32),
    Local(u32),
    /// The enclosing stable declaration's region (MEM-6).
    Enclosing,
    /// A callable's invocation parameter, distinguished from lexical function
    /// parameters. Depth counts intervening callable signatures.
    CallableParameter {
        depth: u32,
        index: u32,
    },
}

/// Callable representation and the owning environment's send promise (LANG-13).
#[derive(
    Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum Callable {
    Plain,
    Own,
    OwnSend,
    Scoped,
}

#[derive(
    Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum TyKind {
    /// A type that already produced a refusal; it satisfies every check silently.
    Error,
    Never,
    Unit,
    Scalar(Scalar),
    Tuple(Vec<TyId>),
    /// An inline array with a literal length (SEM-9).
    Array(TyId, u64),
    /// A region argument, which is never an ordinary value type.
    Region(Root),
    /// A stable or scoped type with its explicit root (MEM-6, OWN-20).
    Anchored(TyId, TyId),
    /// A nominal type: a declaration and its arguments.
    Adt(DeclId, Vec<TyId>),
    /// `View[T]` (OWN-8).
    View(TyId),
    /// The i-th type parameter of the definition being read.
    Param(u32),
    /// A callable signature; parameter and result types are not environment fields.
    Fn(Vec<(Mode, TyId)>, TyId, bool, Callable),
    /// A part of a call's expected type not fixed yet; it constrains nothing (LANG-11).
    Hole,
}

/// Syntactic properties of an immutable interned type, including its arguments.
#[derive(Clone, Copy, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct TypeFlags(u8);
impl TypeFlags {
    const PARAM: u8 = 1;
    const HOLE: u8 = 2;
    fn of(t: &impl TyRead, kind: &TyKind) -> Self {
        let flags = match kind {
            TyKind::Param(_) | TyKind::Region(Root::Generic(_)) => Self::PARAM,
            TyKind::Hole => Self::HOLE,
            TyKind::Tuple(xs) | TyKind::Adt(_, xs) => xs.iter().fold(0, |f, &x| f | t.flags(x).0),
            TyKind::Anchored(x, r) => t.flags(*x).0 | t.flags(*r).0,
            TyKind::View(x) | TyKind::Array(x, _) => t.flags(*x).0,
            TyKind::Fn(ps, r, _, _) => ps.iter().fold(t.flags(*r).0, |f, &(_, x)| f | t.flags(x).0),
            _ => 0,
        };
        Self(flags)
    }
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct TypeEntry {
    kind: TyKind,
    flags: TypeFlags,
}
impl TypeEntry {
    fn new(t: &impl TyRead, kind: TyKind) -> Self {
        let flags = TypeFlags::of(t, &kind);
        Self { kind, flags }
    }
}

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct Types {
    base: Option<std::sync::Arc<Types>>,
    #[serde(skip)]
    base_len: usize,
    kinds: Table<TypeEntry>,
    #[serde(skip)]
    map: HashMap<TyKind, TyId>,
    published_map: Map<TyKind, TyId>,
}
impl<'de> serde::Deserialize<'de> for Types {
    fn deserialize<D: serde::Deserializer<'de>>(decoder: D) -> Result<Self, D::Error> {
        #[derive(serde::Deserialize)]
        struct Wire {
            base: Option<std::sync::Arc<Types>>,
            kinds: Table<TypeEntry>,
            published_map: Map<TyKind, TyId>,
        }
        let wire: Wire = serde::Deserialize::deserialize(decoder)?;
        let base_len = wire.base.as_ref().map_or(0, |b| b.len());
        let offset = base_len + wire.kinds.base_len();
        let map = wire
            .kinds
            .own_iter()
            .enumerate()
            .map(|(i, k)| (k.kind.clone(), TyId((offset + i) as u32)))
            .collect();
        Ok(Self {
            base: wire.base,
            base_len,
            kinds: wire.kinds,
            map,
            published_map: wire.published_map,
        })
    }
}
impl Types {
    pub fn publish(&self, writer: &mut impl Publisher) -> std::io::Result<Self> {
        let rows: Table<TypeEntry> = (0..self.len())
            .map(|i| self.entry(TyId(i as u32)).clone())
            .collect::<Vec<_>>()
            .into();
        let map: Map<TyKind, TyId> = rows
            .iter()
            .enumerate()
            .map(|(i, k)| (k.kind.clone(), TyId(i as u32)))
            .collect();
        Ok(Self {
            base: None,
            base_len: 0,
            kinds: writer.table(&rows)?,
            map: HashMap::default(),
            published_map: writer.map(&map)?,
        })
    }
    pub fn new() -> Self {
        let mut t = Self::default();
        for k in [TyKind::Error, TyKind::Never, TyKind::Unit] {
            t.intern(k);
        }
        for s in Scalar::ALL {
            t.intern(TyKind::Scalar(s));
        }
        t.intern(TyKind::Hole);
        t
    }
    pub fn overlay(base: std::sync::Arc<Types>) -> Self {
        let base_len = base.len();
        Self {
            base: Some(base),
            base_len,
            kinds: Table::default(),
            map: HashMap::default(),
            published_map: Map::default(),
        }
    }
    fn find(&self, k: &TyKind) -> Option<TyId> {
        self.map
            .get(k)
            .copied()
            .or_else(|| self.base.as_ref().and_then(|b| b.find(k)))
            .or_else(|| self.published_map.get(k).copied())
    }
    pub fn intern(&mut self, k: TyKind) -> TyId {
        crate::cost_scope!("phase", "type interning/hashing");
        if let Some(id) = self.find(&k) {
            return id;
        }
        let id = TyId(self.len() as u32);
        self.kinds.push(TypeEntry::new(self, k.clone()));
        self.map.insert(k, id);
        id
    }
    #[inline(always)]
    fn entry(&self, t: TyId) -> &TypeEntry {
        if (t.0 as usize) < self.base_len {
            self.base.as_ref().unwrap().entry(t)
        } else {
            &self.kinds[t.0 as usize - self.base_len]
        }
    }
    #[inline(always)]
    pub fn kind(&self, t: TyId) -> &TyKind {
        &self.entry(t).kind
    }
    #[inline(always)]
    pub fn len(&self) -> usize {
        self.base_len + self.kinds.len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

pub const ERROR: TyId = TyId(0);
pub const NEVER: TyId = TyId(1);
pub const UNIT: TyId = TyId(2);
pub fn scalar(s: Scalar) -> TyId {
    TyId(3 + Scalar::ALL.iter().position(|&x| x == s).unwrap() as u32)
}
pub const BOOL: TyId = TyId(3);
pub const HOLE: TyId = TyId(3 + Scalar::ALL.len() as u32);

/// The program's types plus the types one body makes.
pub struct Local<'a> {
    pub base: &'a Types,
    kinds: Vec<TypeEntry>,
    map: hashbrown::HashTable<TyId>,
    layout_names: Option<LayoutNames<'a>>,
}
struct LayoutNames<'a> {
    base: &'a Types,
    decls: &'a crate::Rows<crate::decls::TypeDecl>,
    values: std::cell::RefCell<HashMap<TyId, String>>,
}
impl<'a> Local<'a> {
    pub fn new(base: &'a Types) -> Self {
        Self {
            base,
            kinds: vec![],
            map: hashbrown::HashTable::new(),
            layout_names: None,
        }
    }
    pub(crate) fn with_decls(
        base: &'a Types,
        decls: &'a crate::Rows<crate::decls::TypeDecl>,
    ) -> Self {
        Self {
            layout_names: Some(LayoutNames {
                base,
                decls,
                values: Default::default(),
            }),
            ..Self::new(base)
        }
    }
    pub fn intern(&mut self, k: TyKind) -> TyId {
        crate::cost_scope!("phase", "type interning/hashing");
        if let Some(id) = self.base.find(&k) {
            return id;
        }
        let base = self.base.len();
        let hash = rustc_hash::FxBuildHasher.hash_one(&k);
        if let Some(&id) = self
            .map
            .find(hash, |id| self.kinds[id.0 as usize - base].kind == k)
        {
            return id;
        }
        let id = TyId((base + self.kinds.len()) as u32);
        self.kinds.push(TypeEntry::new(self, k));
        let kinds = &self.kinds;
        self.map.insert_unique(hash, id, |id| {
            rustc_hash::FxBuildHasher.hash_one(&kinds[id.0 as usize - base].kind)
        });
        id
    }
    #[inline(always)]
    pub fn len(&self) -> usize {
        self.base.len() + self.kinds.len()
    }
    pub fn is_empty(&self) -> bool {
        false
    }
}

/// Read access to types, shared by the declaration pass and body checks.
pub trait TyRead {
    fn kind(&self, t: TyId) -> &TyKind;
    fn mk(&mut self, k: TyKind) -> TyId;
    fn flags(&self, ty: TyId) -> TypeFlags
    where
        Self: Sized,
    {
        TypeFlags::of(self, self.kind(ty))
    }
    fn layout_name(&self, decls: &crate::Rows<crate::decls::TypeDecl>, ty: TyId) -> String
    where
        Self: Sized,
    {
        crate::decls::uncached_layout_name(decls, self, ty)
    }
    fn with_layout_name<R>(
        &self,
        decls: &crate::Rows<crate::decls::TypeDecl>,
        ty: TyId,
        read: impl FnOnce(std::borrow::Cow<'_, str>) -> R,
    ) -> R
    where
        Self: Sized,
    {
        read(std::borrow::Cow::Owned(self.layout_name(decls, ty)))
    }
}
impl TyRead for Types {
    #[inline(always)]
    fn flags(&self, ty: TyId) -> TypeFlags {
        self.entry(ty).flags
    }
    #[inline(always)]
    fn kind(&self, t: TyId) -> &TyKind {
        Types::kind(self, t)
    }
    fn mk(&mut self, k: TyKind) -> TyId {
        self.intern(k)
    }
}
impl TyRead for Local<'_> {
    #[inline(always)]
    fn flags(&self, ty: TyId) -> TypeFlags {
        let i = ty.0 as usize;
        if i < self.base.len() {
            self.base.flags(ty)
        } else {
            self.kinds[i - self.base.len()].flags
        }
    }
    #[inline(always)]
    fn kind(&self, t: TyId) -> &TyKind {
        let i = t.0 as usize;
        if i < self.base.len() {
            self.base.kind(t)
        } else {
            &self.kinds[i - self.base.len()].kind
        }
    }
    fn mk(&mut self, k: TyKind) -> TyId {
        self.intern(k)
    }
    fn layout_name(&self, decls: &crate::Rows<crate::decls::TypeDecl>, ty: TyId) -> String {
        self.with_layout_name(decls, ty, |name| name.into_owned())
    }
    fn with_layout_name<R>(
        &self,
        decls: &crate::Rows<crate::decls::TypeDecl>,
        ty: TyId,
        read: impl FnOnce(std::borrow::Cow<'_, str>) -> R,
    ) -> R {
        use std::borrow::Cow;
        let Some(cache) = self.layout_names.as_ref().filter(|cache| {
            std::ptr::eq(cache.base, self.base) && std::ptr::eq(cache.decls, decls)
        }) else {
            return read(Cow::Owned(crate::decls::uncached_layout_name(
                decls, self, ty,
            )));
        };
        let values = cache.values.borrow();
        if let Some(name) = values.get(&ty) {
            return read(Cow::Borrowed(name));
        }
        drop(values);
        let name = crate::decls::uncached_layout_name(decls, self, ty);
        cache.values.borrow_mut().insert(ty, name.clone());
        read(Cow::Owned(name))
    }
}

/// Replace `Param(i)` by `args[i]`.
pub fn subst(t: &mut impl TyRead, ty: TyId, args: &[TyId]) -> TyId {
    crate::cost_scope!("phase", "type substitution");
    if args.is_empty() {
        return ty;
    }
    subst_at(t, ty, args, 0)
}

fn subst_at(t: &mut impl TyRead, ty: TyId, args: &[TyId], depth: u32) -> TyId {
    match t.kind(ty).clone() {
        TyKind::Param(i) | TyKind::Region(Root::Generic(i)) => {
            let value = args.get(i as usize).copied().unwrap_or(ERROR);
            if depth == 0 {
                value
            } else {
                callable_roots(t, value, 0, RootSubstitution::Shift(depth))
            }
        }
        TyKind::Anchored(x, root) => {
            let x = subst_at(t, x, args, depth);
            let root = subst_at(t, root, args, depth);
            t.mk(TyKind::Anchored(x, root))
        }
        TyKind::Tuple(xs) => {
            let xs = xs.iter().map(|&x| subst_at(t, x, args, depth)).collect();
            t.mk(TyKind::Tuple(xs))
        }
        TyKind::Adt(d, xs) => {
            let xs = xs.iter().map(|&x| subst_at(t, x, args, depth)).collect();
            t.mk(TyKind::Adt(d, xs))
        }
        TyKind::Array(x, n) => {
            let x = subst_at(t, x, args, depth);
            t.mk(TyKind::Array(x, n))
        }
        TyKind::View(x) => {
            let x = subst_at(t, x, args, depth);
            t.mk(TyKind::View(x))
        }
        TyKind::Fn(ps, r, w, k) => {
            let ps = ps
                .iter()
                .map(|&(m, p)| (m, subst_at(t, p, args, depth + 1)))
                .collect();
            let r = subst_at(t, r, args, depth + 1);
            t.mk(TyKind::Fn(ps, r, w, k))
        }
        _ => ty,
    }
}

/// Replace the root `from` by the root type `to` (a call's Region parameter by the
/// region its argument names, MEM-6).
pub fn reroot(t: &mut impl TyRead, ty: TyId, from: Root, to: TyId) -> TyId {
    match t.kind(ty).clone() {
        TyKind::Region(r) if r == from => to,
        TyKind::Anchored(x, root) => {
            let x = reroot(t, x, from, to);
            let root = reroot(t, root, from, to);
            t.mk(TyKind::Anchored(x, root))
        }
        TyKind::Tuple(xs) => {
            let xs = xs.iter().map(|&x| reroot(t, x, from, to)).collect();
            t.mk(TyKind::Tuple(xs))
        }
        TyKind::Adt(d, xs) => {
            let xs = xs.iter().map(|&x| reroot(t, x, from, to)).collect();
            t.mk(TyKind::Adt(d, xs))
        }
        TyKind::Array(x, n) => {
            let x = reroot(t, x, from, to);
            t.mk(TyKind::Array(x, n))
        }
        TyKind::View(x) => {
            let x = reroot(t, x, from, to);
            t.mk(TyKind::View(x))
        }
        TyKind::Fn(ps, r, w, k) => {
            let ps = ps
                .iter()
                .map(|&(m, p)| (m, reroot(t, p, from, to)))
                .collect();
            let r = reroot(t, r, from, to);
            t.mk(TyKind::Fn(ps, r, w, k))
        }
        _ => ty,
    }
}

/// Close a function/closure declaration's formal roots over its callable binder.
pub fn bind_callable_roots(t: &mut impl TyRead, ty: TyId) -> TyId {
    let TyKind::Fn(mut ps, r, w, k) = t.kind(ty).clone() else {
        unreachable!()
    };
    for (_, p) in &mut ps {
        *p = callable_roots(t, *p, 0, RootSubstitution::Bind);
    }
    let r = callable_roots(t, r, 0, RootSubstitution::Bind);
    t.mk(TyKind::Fn(ps, r, w, k))
}

/// Open only the invoked callable binder. Lexical roots and roots bound by a
/// nested returned callable remain untouched.
pub fn open_callable_roots(t: &mut impl TyRead, ty: TyId, roots: &[Option<TyId>]) -> TyId {
    if roots.iter().all(Option::is_none) {
        return ty;
    }
    callable_roots(t, ty, 0, RootSubstitution::Open(roots))
}

#[derive(Clone, Copy)]
enum RootSubstitution<'a> {
    Bind,
    Open(&'a [Option<TyId>]),
    Shift(u32),
}

fn callable_roots(
    t: &mut impl TyRead,
    ty: TyId,
    depth: u32,
    operation: RootSubstitution<'_>,
) -> TyId {
    match t.kind(ty).clone() {
        TyKind::Region(Root::Parameter(index)) if matches!(operation, RootSubstitution::Bind) => {
            t.mk(TyKind::Region(Root::CallableParameter { depth, index }))
        }
        TyKind::Region(Root::CallableParameter {
            depth: bound,
            index,
        }) => match operation {
            RootSubstitution::Open(roots) if bound == depth => {
                let value = roots.get(index as usize).copied().flatten().unwrap_or(ty);
                if depth == 0 {
                    value
                } else {
                    callable_roots(t, value, 0, RootSubstitution::Shift(depth))
                }
            }
            RootSubstitution::Shift(amount) if bound >= depth => {
                t.mk(TyKind::Region(Root::CallableParameter {
                    depth: bound + amount,
                    index,
                }))
            }
            _ => ty,
        },
        TyKind::Anchored(x, r) => {
            let x = callable_roots(t, x, depth, operation);
            let r = callable_roots(t, r, depth, operation);
            t.mk(TyKind::Anchored(x, r))
        }
        TyKind::Tuple(mut xs) => {
            for x in &mut xs {
                *x = callable_roots(t, *x, depth, operation);
            }
            t.mk(TyKind::Tuple(xs))
        }
        TyKind::Adt(d, mut xs) => {
            for x in &mut xs {
                *x = callable_roots(t, *x, depth, operation);
            }
            t.mk(TyKind::Adt(d, xs))
        }
        TyKind::Array(x, n) => {
            let x = callable_roots(t, x, depth, operation);
            t.mk(TyKind::Array(x, n))
        }
        TyKind::View(x) => {
            let x = callable_roots(t, x, depth, operation);
            t.mk(TyKind::View(x))
        }
        TyKind::Fn(mut ps, r, w, k) => {
            for (_, p) in &mut ps {
                *p = callable_roots(t, *p, depth + 1, operation);
            }
            let r = callable_roots(t, r, depth + 1, operation);
            t.mk(TyKind::Fn(ps, r, w, k))
        }
        _ => ty,
    }
}

/// Whether `ty` names a type parameter anywhere.
pub fn has_hole(t: &impl TyRead, ty: TyId) -> bool {
    crate::cost_scope!("query", "has hole");
    t.flags(ty).0 & TypeFlags::HOLE != 0
}
pub fn has_param(t: &impl TyRead, ty: TyId) -> bool {
    crate::cost_scope!("query", "has parameter");
    t.flags(ty).0 & TypeFlags::PARAM != 0
}

#[cfg(test)]
mod interner_tests {
    use super::*;

    #[test]
    fn type_flags_follow_arguments_roots_and_local_arenas() {
        let mut t = Types::new();
        let param = t.intern(TyKind::Param(0));
        let generic = t.intern(TyKind::Region(Root::Generic(0)));
        let lexical = t.intern(TyKind::Region(Root::Parameter(0)));
        let anchored = t.intern(TyKind::Anchored(UNIT, generic));
        let concrete = t.intern(TyKind::Anchored(UNIT, lexical));
        let pair = t.intern(TyKind::Tuple(vec![param, HOLE]));
        let nominal = t.intern(TyKind::Adt(DeclId(0), vec![pair]));
        let callable = t.intern(TyKind::Fn(
            vec![(Mode::View, nominal)],
            UNIT,
            false,
            Callable::Scoped,
        ));
        for ty in [param, generic, anchored, pair, nominal, callable] {
            assert!(has_param(&t, ty));
        }
        for ty in [UNIT, lexical, concrete, HOLE] {
            assert!(!has_param(&t, ty));
        }
        for ty in [HOLE, pair, nominal, callable] {
            assert!(has_hole(&t, ty));
        }
        for ty in [UNIT, param, generic, lexical, anchored, concrete] {
            assert!(!has_hole(&t, ty));
        }
        let mut a = Local::new(&t);
        let mut b = Local::new(&t);
        let x = a.intern(TyKind::Array(pair, 2));
        let y = b.intern(TyKind::Array(UNIT, 2));
        assert_eq!(x, y);
        assert!(has_param(&a, x) && has_hole(&a, x));
        assert!(!has_param(&b, y) && !has_hole(&b, y));
        let decoded: Types = serde_json::from_value(serde_json::to_value(&t).unwrap()).unwrap();
        for i in 0..t.len() {
            let ty = TyId(i as u32);
            assert_eq!(has_param(&t, ty), has_param(&decoded, ty));
            assert_eq!(has_hole(&t, ty), has_hole(&decoded, ty));
        }
    }

    #[test]
    fn querying_type_flags_does_not_walk_the_type_graph() {
        struct Counted<'a> {
            types: &'a Types,
            reads: std::cell::Cell<usize>,
        }
        impl TyRead for Counted<'_> {
            fn kind(&self, ty: TyId) -> &TyKind {
                self.reads.set(self.reads.get() + 1);
                self.types.kind(ty)
            }
            fn mk(&mut self, _: TyKind) -> TyId {
                unreachable!()
            }
            fn flags(&self, ty: TyId) -> TypeFlags {
                self.types.flags(ty)
            }
        }
        let mut t = Types::new();
        let mut ty = t.intern(TyKind::Param(0));
        for _ in 0..64 {
            ty = t.intern(TyKind::View(ty));
        }
        let counted = Counted {
            types: &t,
            reads: Default::default(),
        };
        assert!(has_param(&counted, ty));
        assert!(!has_hole(&counted, ty));
        assert_eq!(
            counted.reads.get(),
            0,
            "a predicate re-walked the type graph"
        );
    }

    #[test]
    fn local_type_index_survives_growth_and_compares_full_keys() {
        let base = Types::new();
        let mut local = Local::new(&base);
        let mut ids = vec![];
        for n in 0..256 {
            ids.push(local.intern(TyKind::Tuple(vec![UNIT; n])));
        }
        for (n, &id) in ids.iter().enumerate().rev() {
            assert_eq!(local.intern(TyKind::Tuple(vec![UNIT; n])), id);
            assert!(matches!(local.kind(id), TyKind::Tuple(xs) if xs.len() == n));
        }
        assert_eq!(local.len(), base.len() + 256);
        assert_eq!(local.intern(TyKind::Unit), UNIT);
        assert_eq!(local.len(), base.len() + 256);
        let arguments = vec![UNIT; 1024];
        let address = arguments.as_ptr();
        let id = local.intern(TyKind::Tuple(arguments));
        let TyKind::Tuple(stored) = local.kind(id) else {
            panic!("tuple")
        };
        assert_eq!(stored.as_ptr(), address, "interner cloned its vector key");
    }

    #[test]
    fn layout_names_are_local_to_the_arena_and_declarations() {
        let (index, modules) = crate::tests::program_inputs("type Local { n: u64 }\n");
        let p = crate::Program::declare(&index, &modules);
        let d = p
            .decls
            .iter()
            .position(|d| d.path.0.name == "Local")
            .unwrap();
        let mut renamed = p.decls.clone();
        renamed[d].path.0.name = "Other".into();
        let mut first = Local::with_decls(&p.types, &p.decls);
        let nominal = first.intern(TyKind::Adt(DeclId(d as u32), vec![]));
        let expected = p.decls[d].path.to_string();
        assert_eq!(first.layout_name(&p.decls, nominal), expected);
        first.with_layout_name(&p.decls, nominal, |name| {
            assert!(matches!(name, std::borrow::Cow::Borrowed(_)));
            assert_eq!(name, expected);
        });
        first.with_layout_name(&renamed, nominal, |name| {
            assert!(matches!(name, std::borrow::Cow::Owned(_)));
            assert_eq!(name, renamed[d].path.to_string());
        });
        assert_eq!(first.layout_name(&p.decls, nominal), expected);
        assert_eq!(
            first.layout_name(&renamed, nominal),
            renamed[d].path.to_string()
        );
        assert_eq!(first.layout_name(&p.decls, nominal), expected);
        let a = first.intern(TyKind::Array(nominal, 2));
        let mut second = Local::with_decls(&p.types, &p.decls);
        assert_eq!(
            second.intern(TyKind::Adt(DeclId(d as u32), vec![])),
            nominal
        );
        let b = second.intern(TyKind::Array(nominal, 3));
        assert_eq!(a, b);
        assert_eq!(
            first.layout_name(&p.decls, a),
            format!("Inline[{expected}, 2]")
        );
        assert_eq!(
            second.layout_name(&p.decls, b),
            format!("Inline[{expected}, 3]")
        );
        let anchor = first.intern(TyKind::Region(Root::Local(4)));
        let anchored = first.intern(TyKind::Anchored(a, anchor));
        assert_eq!(
            first.layout_name(&p.decls, anchored),
            first.layout_name(&p.decls, a)
        );
        let view = first.intern(TyKind::View(anchored));
        assert_eq!(
            first.layout_name(&p.decls, view),
            format!("View[Inline[{expected}, 2]]")
        );
    }

    #[test]
    fn overlay_ids_do_not_grow_suffixes_or_escape_local_arenas() {
        let mut base = Types::new();
        let tuple = TyKind::Tuple(vec![scalar(Scalar::U64)]);
        let id = base.intern(tuple.clone());
        let base = std::sync::Arc::new(base);
        let mut program = Types::overlay(base.clone());
        let start = program.len();
        for _ in 0..3 {
            assert_eq!(program.intern(tuple.clone()), id);
            assert_eq!(program.len(), start);
            assert_eq!(program.kind(id), &tuple);
        }
        let pair = TyKind::Tuple(vec![id, id]);
        let pair_id = program.intern(pair.clone());
        assert_eq!(pair_id, TyId(start as u32));
        assert_eq!(program.intern(pair), pair_id);
        assert_eq!(program.len(), start + 1);
        let mut first = Local::new(&program);
        for _ in 0..3 {
            assert_eq!(first.intern(tuple.clone()), id);
            assert_eq!(first.len(), program.len());
        }
        let two = TyKind::Array(pair_id, 2);
        let first_id = first.intern(two.clone());
        assert_eq!(first_id, TyId(program.len() as u32));
        assert_eq!(first.intern(two.clone()), first_id);
        assert_eq!(first.len(), program.len() + 1);
        let mut second = Local::new(&program);
        assert_eq!(second.intern(tuple), id);
        let three = TyKind::Array(pair_id, 3);
        let second_id = second.intern(three.clone());
        assert_eq!(first_id, second_id);
        assert_eq!(first.kind(first_id), &two);
        assert_eq!(second.kind(second_id), &three);
        assert_eq!(program.find(&two), None);
        assert_eq!(base.len(), start);
        let wire = serde_json::to_value(&program).unwrap();
        assert_eq!(
            wire.as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["base", "kinds", "published_map"]
        );
        let decoded: Types = serde_json::from_value(wire).unwrap();
        assert_eq!(decoded.len(), program.len());
        for i in 0..program.len() {
            assert_eq!(decoded.kind(TyId(i as u32)), program.kind(TyId(i as u32)));
        }
        let mut nested = Types::overlay(std::sync::Arc::new(decoded));
        let before = nested.len();
        let added = nested.intern(TyKind::Array(pair_id, 7));
        assert_eq!(added, TyId(before as u32));
        assert_eq!(nested.len(), before + 1);
        assert_eq!(nested.kind(pair_id), program.kind(pair_id));
        assert_eq!(nested.kind(added), &TyKind::Array(pair_id, 7));
    }
}

#[cfg(test)]
mod callable_roots_tests {
    use super::*;
    #[test]
    fn opening_a_callable_preserves_a_nested_binder() {
        let mut t = Types::new();
        let lexical = t.intern(TyKind::Region(Root::Parameter(0)));
        let inner = t.intern(TyKind::Region(Root::CallableParameter {
            depth: 0,
            index: 0,
        }));
        let a = t.intern(TyKind::Anchored(scalar(Scalar::U64), lexical));
        let b = t.intern(TyKind::Anchored(scalar(Scalar::U64), inner));
        let pair = t.intern(TyKind::Tuple(vec![a, b]));
        let nested = t.intern(TyKind::Fn(vec![], pair, false, Callable::Plain));
        let outer = t.intern(TyKind::Fn(vec![], nested, false, Callable::Plain));
        let closed = bind_callable_roots(&mut t, outer);
        let TyKind::Fn(_, nested, _, _) = t.kind(closed) else {
            unreachable!()
        };
        let nested = *nested;
        let actual = t.intern(TyKind::Region(Root::Local(99)));
        let opened = open_callable_roots(&mut t, nested, &[Some(actual)]);
        let TyKind::Fn(_, pair, _, _) = t.kind(opened) else {
            unreachable!()
        };
        let TyKind::Tuple(xs) = t.kind(*pair) else {
            unreachable!()
        };
        assert_eq!(
            t.kind(xs[0]),
            &TyKind::Anchored(scalar(Scalar::U64), actual)
        );
        assert_eq!(t.kind(xs[1]), &TyKind::Anchored(scalar(Scalar::U64), inner));
    }
}
