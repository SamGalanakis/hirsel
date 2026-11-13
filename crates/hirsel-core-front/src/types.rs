//! Types of the check: one interner per program for declarations and signatures, and
//! a body-local overlay for the types a body makes, so bodies check independently.
//! A type parameter is `Param(i)`, the i-th generic of the definition being read; it
//! is rigid (OWN-23) and means nothing outside that definition.

use hirsel_core_ids::published::{Map, Publisher, Table};
use hirsel_core_layout::Scalar;
use rustc_hash::FxHashMap as HashMap;

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

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct Types {
    base: Option<std::sync::Arc<Types>>,
    kinds: Table<TyKind>,
    #[serde(skip)]
    map: HashMap<TyKind, TyId>,
    published_map: Map<TyKind, TyId>,
}
impl<'de> serde::Deserialize<'de> for Types {
    fn deserialize<D: serde::Deserializer<'de>>(decoder: D) -> Result<Self, D::Error> {
        #[derive(serde::Deserialize)]
        struct Wire {
            base: Option<std::sync::Arc<Types>>,
            kinds: Table<TyKind>,
            published_map: Map<TyKind, TyId>,
        }
        let wire: Wire = serde::Deserialize::deserialize(decoder)?;
        let offset = wire.base.as_ref().map_or(0, |b| b.len()) + wire.kinds.base_len();
        let map = wire
            .kinds
            .own_iter()
            .enumerate()
            .map(|(i, k)| (k.clone(), TyId((offset + i) as u32)))
            .collect();
        Ok(Self {
            base: wire.base,
            kinds: wire.kinds,
            map,
            published_map: wire.published_map,
        })
    }
}
impl Types {
    pub fn publish(&self, writer: &mut impl Publisher) -> std::io::Result<Self> {
        let rows: Table<TyKind> = (0..self.len())
            .map(|i| self.kind(TyId(i as u32)).clone())
            .collect::<Vec<_>>()
            .into();
        let map: Map<TyKind, TyId> = rows
            .iter()
            .enumerate()
            .map(|(i, k)| (k.clone(), TyId(i as u32)))
            .collect();
        Ok(Self {
            base: None,
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
        Self {
            base: Some(base),
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
        self.kinds.push(k.clone());
        self.map.insert(k, id);
        id
    }
    #[inline(always)]
    pub fn kind(&self, t: TyId) -> &TyKind {
        let base_len = self.base.as_ref().map_or(0, |b| b.len());
        if (t.0 as usize) < base_len {
            self.base.as_ref().unwrap().kind(t)
        } else {
            &self.kinds[t.0 as usize - base_len]
        }
    }
    #[inline(always)]
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
            map: HashMap::default(),
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
        if let Some(&id) = self.map.get(&k) {
            return id;
        }
        let id = TyId((self.base.len() + self.kinds.len()) as u32);
        self.kinds.push(k.clone());
        self.map.insert(k, id);
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
    fn layout_name(&self, decls: &crate::Rows<crate::decls::TypeDecl>, ty: TyId) -> String
    where
        Self: Sized,
    {
        crate::decls::uncached_layout_name(decls, self, ty)
    }
}
impl TyRead for Types {
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
    fn layout_name(&self, decls: &crate::Rows<crate::decls::TypeDecl>, ty: TyId) -> String {
        let Some(cache) = self.layout_names.as_ref().filter(|cache| {
            std::ptr::eq(cache.base, self.base) && std::ptr::eq(cache.decls, decls)
        }) else {
            return crate::decls::uncached_layout_name(decls, self, ty);
        };
        if let Some(name) = cache.values.borrow().get(&ty) {
            return name.clone();
        }
        let name = crate::decls::uncached_layout_name(decls, self, ty);
        cache.values.borrow_mut().insert(ty, name.clone());
        name
    }
}

/// Replace `Param(i)` by `args[i]`.
pub fn subst(t: &mut impl TyRead, ty: TyId, args: &[TyId]) -> TyId {
    crate::cost_scope!("phase", "type substitution");
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
        TyKind::Fn(ps, r, w, k) => {
            let ps = ps.iter().map(|&(m, p)| (m, subst(t, p, args))).collect();
            let r = subst(t, r, args);
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

/// Whether `ty` names a type parameter anywhere.
pub fn has_hole(t: &impl TyRead, ty: TyId) -> bool {
    crate::cost_scope!("query", "has hole");
    match t.kind(ty) {
        TyKind::Hole => true,
        TyKind::Tuple(xs) | TyKind::Adt(_, xs) => xs.iter().any(|&x| has_hole(t, x)),
        TyKind::Anchored(x, root) => has_hole(t, *x) || has_hole(t, *root),
        TyKind::View(x) | TyKind::Array(x, _) => has_hole(t, *x),
        TyKind::Fn(ps, r, _, _) => ps.iter().any(|&(_, p)| has_hole(t, p)) || has_hole(t, *r),
        _ => false,
    }
}
pub fn has_param(t: &impl TyRead, ty: TyId) -> bool {
    crate::cost_scope!("query", "has parameter");
    match t.kind(ty) {
        TyKind::Param(_) | TyKind::Region(Root::Generic(_)) => true,
        TyKind::Anchored(x, r) => has_param(t, *x) || has_param(t, *r),
        TyKind::Tuple(xs) | TyKind::Adt(_, xs) => xs.iter().any(|&x| has_param(t, x)),
        TyKind::View(x) | TyKind::Array(x, _) => has_param(t, *x),
        TyKind::Fn(ps, r, _, _) => ps.iter().any(|&(_, p)| has_param(t, p)) || has_param(t, *r),
        _ => false,
    }
}

#[cfg(test)]
mod interner_tests {
    use super::*;

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
    }
}
