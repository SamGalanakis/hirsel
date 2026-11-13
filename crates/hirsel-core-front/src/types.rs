//! Types of the check: one interner per program for declarations and signatures, and
//! a body-local overlay for the types a body makes, so bodies check independently.
//! A type parameter is `Param(i)`, the i-th generic of the definition being read; it
//! is rigid (OWN-23) and means nothing outside that definition.

use hirsel_core_layout::Scalar;
use std::collections::HashMap;

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
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Mode {
    View,
    Mut,
    Take,
}

/// A root in a published signature or a body. Identities are local to the definition.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Root {
    Generic(u32),
    Parameter(u32),
    Local(u32),
    /// The enclosing stable declaration's region (MEM-6).
    Enclosing,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize)]
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
    /// A plain callable type `fn(..) -> R` (LANG-13).
    Fn(Vec<(Mode, TyId)>, TyId, bool),
    /// A part of a call's expected type not fixed yet; it constrains nothing (LANG-11).
    Hole,
}

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct Types {
    base: Option<std::sync::Arc<Types>>,
    kinds: Vec<TyKind>,
    #[serde(skip)]
    map: HashMap<TyKind, TyId>,
}
impl<'de> serde::Deserialize<'de> for Types {
    fn deserialize<D: serde::Deserializer<'de>>(decoder: D) -> Result<Self, D::Error> {
        #[derive(serde::Deserialize)]
        struct Wire {
            base: Option<std::sync::Arc<Types>>,
            kinds: Vec<TyKind>,
        }
        let wire: Wire = serde::Deserialize::deserialize(decoder)?;
        let offset = wire.base.as_ref().map_or(0, |b| b.len());
        let map = wire
            .kinds
            .iter()
            .enumerate()
            .map(|(i, k)| (k.clone(), TyId((offset + i) as u32)))
            .collect();
        Ok(Self {
            base: wire.base,
            kinds: wire.kinds,
            map,
        })
    }
}
impl Types {
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
        Self {
            base: Some(base),
            kinds: vec![],
            map: HashMap::new(),
        }
    }
    fn find(&self, k: &TyKind) -> Option<TyId> {
        self.base
            .as_ref()
            .and_then(|b| b.find(k))
            .or_else(|| self.map.get(k).copied())
    }
    pub fn intern(&mut self, k: TyKind) -> TyId {
        if let Some(id) = self.find(&k) {
            return id;
        }
        let id = TyId(self.len() as u32);
        self.kinds.push(k.clone());
        self.map.insert(k, id);
        id
    }
    pub fn kind(&self, t: TyId) -> &TyKind {
        let base_len = self.base.as_ref().map_or(0, |b| b.len());
        if (t.0 as usize) < base_len {
            self.base.as_ref().unwrap().kind(t)
        } else {
            &self.kinds[t.0 as usize - base_len]
        }
    }
    pub fn len(&self) -> usize {
        self.base.as_ref().map_or(0, |b| b.len()) + self.kinds.len()
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
    kinds: Vec<TyKind>,
    map: HashMap<TyKind, TyId>,
}
impl<'a> Local<'a> {
    pub fn new(base: &'a Types) -> Self {
        Self {
            base,
            kinds: vec![],
            map: HashMap::new(),
        }
    }
    pub fn intern(&mut self, k: TyKind) -> TyId {
        if let Some(id) = self.base.find(&k) {
            return id;
        }
        if let Some(&id) = self.map.get(&k) {
            return id;
        }
        let id = TyId((self.base.len() + self.kinds.len()) as u32);
        self.kinds.push(k.clone());
        self.map.insert(k, id);
        id
    }
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
}
impl TyRead for Types {
    fn kind(&self, t: TyId) -> &TyKind {
        Types::kind(self, t)
    }
    fn mk(&mut self, k: TyKind) -> TyId {
        self.intern(k)
    }
}
impl TyRead for Local<'_> {
    fn kind(&self, t: TyId) -> &TyKind {
        let i = t.0 as usize;
        if i < self.base.len() {
            self.base.kind(t)
        } else {
            &self.kinds[i - self.base.len()]
        }
    }
    fn mk(&mut self, k: TyKind) -> TyId {
        self.intern(k)
    }
}

/// Replace `Param(i)` by `args[i]`.
pub fn subst(t: &mut impl TyRead, ty: TyId, args: &[TyId]) -> TyId {
    if args.is_empty() {
        return ty;
    }
    match t.kind(ty).clone() {
        TyKind::Param(i) | TyKind::Region(Root::Generic(i)) => {
            args.get(i as usize).copied().unwrap_or(ERROR)
        }
        TyKind::Anchored(x, root) => {
            let x = subst(t, x, args);
            let root = subst(t, root, args);
            t.mk(TyKind::Anchored(x, root))
        }
        TyKind::Tuple(xs) => {
            let xs = xs.iter().map(|&x| subst(t, x, args)).collect();
            t.mk(TyKind::Tuple(xs))
        }
        TyKind::Adt(d, xs) => {
            let xs = xs.iter().map(|&x| subst(t, x, args)).collect();
            t.mk(TyKind::Adt(d, xs))
        }
        TyKind::Array(x, n) => {
            let x = subst(t, x, args);
            t.mk(TyKind::Array(x, n))
        }
        TyKind::View(x) => {
            let x = subst(t, x, args);
            t.mk(TyKind::View(x))
        }
        TyKind::Fn(ps, r, w) => {
            let ps = ps.iter().map(|&(m, p)| (m, subst(t, p, args))).collect();
            let r = subst(t, r, args);
            t.mk(TyKind::Fn(ps, r, w))
        }
        _ => ty,
    }
}

/// Whether `ty` names a type parameter anywhere.
pub fn has_hole(t: &impl TyRead, ty: TyId) -> bool {
    match t.kind(ty) {
        TyKind::Hole => true,
        TyKind::Tuple(xs) | TyKind::Adt(_, xs) => xs.iter().any(|&x| has_hole(t, x)),
        TyKind::Anchored(x, _) | TyKind::View(x) | TyKind::Array(x, _) => has_hole(t, *x),
        TyKind::Fn(ps, r, _) => ps.iter().any(|&(_, p)| has_hole(t, p)) || has_hole(t, *r),
        _ => false,
    }
}
pub fn has_param(t: &impl TyRead, ty: TyId) -> bool {
    match t.kind(ty) {
        TyKind::Param(_) | TyKind::Region(Root::Generic(_)) => true,
        TyKind::Anchored(x, r) => has_param(t, *x) || has_param(t, *r),
        TyKind::Tuple(xs) | TyKind::Adt(_, xs) => xs.iter().any(|&x| has_param(t, x)),
        TyKind::View(x) | TyKind::Array(x, _) => has_param(t, *x),
        TyKind::Fn(ps, r, _) => ps.iter().any(|&(_, p)| has_param(t, p)) || has_param(t, *r),
        _ => false,
    }
}
