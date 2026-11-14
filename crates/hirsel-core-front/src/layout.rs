//! Shapes for `hirsel_core_layout::compute`. A declaration with no parameters gets its
//! recipe once, with the closed types it holds inline; a body module's type table
//! (`Module::types`) gets one row per closed type its signatures and values use,
//! instances of generic declarations included (FORM v1b: `Option` and `Result` are
//! ordinary variant rows, one per instantiation).

use crate::Program;
use crate::decls::{DeclBody, Repr, Role};
use crate::types::*;
use hirsel_core_layout::{
    Body, Carried, Case, Field, FieldTy, FuncRef, Handle, LayoutError, Repr as LRepr, Scalar,
    Shape, Ty, TypeRef, compute,
};
use rustc_hash::FxHashMap as HashMap;

/// A shape table being extended: rows are keyed by canonical text, which is also the
/// row's name, so one closed type has one row whichever body names it.
pub(crate) struct Shapes<'p, T: TyRead> {
    pub decls: &'p crate::Rows<crate::decls::TypeDecl>,
    pub types: &'p mut T,
    pub shapes: &'p mut Vec<Shape>,
    pub of: &'p mut HashMap<String, TypeRef>,
    /// A body module's table: a finalizer or clone body names a function row this
    /// table cannot name yet, so it is left to [`Self::hooks`].
    pub module: bool,
    /// The rows whose hook `.1` is the program's function `.2` at the type's arguments
    /// `.3` (its instance when the type is generic), for the caller to name.
    pub hooks: Vec<(TypeRef, Hook, u32, Vec<TyId>)>,
}

/// A row's core-declared function: its finalizer (`Shape::finalizer`) or its clone
/// body (`Shape::clone`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Hook {
    Finalizer,
    Clone,
}

impl<T: TyRead> Shapes<'_, T> {
    /// The shape of a closed type held inline, or None when the layout crate has no
    /// field type for some part of it (recorded as a gap by the caller).
    pub fn shape(&mut self, ty: TyId) -> Option<TypeRef> {
        crate::cost_scope!("query", "shape lookup");
        let name = match crate::decls::with_layout_name(self.decls, &*self.types, ty, |name| {
            self.of
                .get(name.as_ref())
                .copied()
                .ok_or_else(|| name.into_owned())
        }) {
            Ok(row) => return Some(row),
            Err(name) => name,
        };
        // A view is held as its type's own bytes (FORM #56); an anchor only bounds where
        // a value may go.
        if let TyKind::Anchored(x, _) | TyKind::View(x) = *self.types.kind(ty) {
            return self.shape(x);
        }
        // Reserve the index first: a field reaching this type through an indirection
        // (`List[Tree]` in `Tree`) names it before its body is known.
        let r = TypeRef(self.shapes.len() as u32);
        self.shapes.push(Shape {
            name: name.clone(),
            repr: LRepr::Own,
            carried: carried(self.decls, &*self.types, ty).unwrap_or(Carried::Address),
            body: Body::Record(vec![]),
            finalizer: None,
            clone: None,
            grant: 0,
        });
        self.of.insert(name, r);
        let (repr, body, finalizer) = match self.types.kind(ty).clone() {
            TyKind::Scalar(s) => (
                LRepr::Own,
                Body::Record(vec![Field {
                    name: "0".into(),
                    ty: FieldTy::Scalar(s),
                }]),
                None,
            ),
            // A rigid parameter is an opaque owned place until materialization.
            // A non-plain placeholder keeps every potential owner/drop edge alive;
            // the concrete shape replaces it before any execution.
            TyKind::Param(_) if self.module => (
                LRepr::Own,
                record(vec![(
                    "opaque",
                    FieldTy::Seq(Box::new(FieldTy::Scalar(Scalar::U8))),
                )]),
                None,
            ),
            // `Never` has size 0 (core.md, built-in types), as unit does.
            TyKind::Unit | TyKind::Never => (LRepr::Own, Body::Record(vec![]), None),
            TyKind::Fn(_, _, _, callable) => (
                if callable == Callable::Scoped {
                    LRepr::Scoped
                } else {
                    LRepr::Own
                },
                record(vec![("callable", self.field(ty)?)]),
                None,
            ),
            TyKind::Array(x, n) => (
                LRepr::Own,
                Body::Record(vec![Field {
                    name: "0".into(),
                    ty: FieldTy::Array(Box::new(self.field(x)?), n),
                }]),
                None,
            ),
            TyKind::Tuple(xs) => {
                let mut fields = vec![];
                for (i, x) in xs.into_iter().enumerate() {
                    fields.push(Field {
                        name: i.to_string(),
                        ty: self.field(x)?,
                    });
                }
                (
                    if crate::decls::kind(self.decls, self.types, ty, &[]) == crate::Kind::Scoped {
                        LRepr::Scoped
                    } else {
                        LRepr::Own
                    },
                    Body::Record(fields),
                    None,
                )
            }
            TyKind::Adt(d, args) => {
                let decls = self.decls;
                let decl = &decls[d.index()];
                // Core storage hooks also apply to built-in carriers, whose body
                // below is supplied by the form rather than source fields.
                self.shapes[r.index()].clone = match decl.clone_body {
                    Some(f) if self.module => {
                        self.hooks.push((r, Hook::Clone, f, args.clone()));
                        Some(FuncRef(u32::MAX))
                    }
                    Some(_) => Some(FuncRef(d.0)),
                    None => None,
                };
                match decl.role {
                    // The core collections' carriers (message.core: `own "String"`).
                    Some(Role::String) => {
                        let bytes = FieldTy::Seq(Box::new(FieldTy::Scalar(Scalar::U8)));
                        return self.fill(r, LRepr::Own, record(vec![("bytes", bytes)]), None);
                    }
                    Some(Role::List) => {
                        let items = FieldTy::Seq(Box::new(self.field(args[0])?));
                        return self.fill(r, LRepr::Own, record(vec![("items", items)]), None);
                    }
                    Some(Role::Map) => {
                        let entries = self.map_field(&args)?;
                        return self.fill(r, LRepr::Own, record(vec![("entries", entries)]), None);
                    }
                    // A log owns one boxed List carrier.  The source Log[E] declaration
                    // has E as its only argument; construct the corresponding List[E]
                    // row here so the box shape is the same row used by list kernels.
                    Some(Role::Log) => {
                        let list = self
                            .decls
                            .iter()
                            .position(|d| d.role == Some(Role::List))
                            .map(|d| crate::types::DeclId(d as u32))?;
                        let list = self.types.mk(TyKind::Adt(list, vec![args[0]]));
                        let node = FieldTy::Box(self.shape(list)?);
                        return self.fill(r, LRepr::Own, record(vec![("node", node)]), None);
                    }
                    // Compact text or a span held as a value: a stable reference to
                    // compact data in its region (core.md), plain. The locator names
                    // what it locates (FORM #63), so key, equality and order read it.
                    Some(
                        Role::Text
                        | Role::FixedSpan
                        | Role::SeqSpan
                        | Role::IndexedSpan
                        | Role::SeqIndex,
                    ) => {
                        let at = self.field(ty)?;
                        return self.fill(r, LRepr::Own, record(vec![("at", at)]), None);
                    }
                    // A span builder held as a value: its region blob and its region
                    // (MEM-3); the blob lives in the region, so cleaning does nothing.
                    // A stream builder (MEM-4) is its blob, the root's offset, the bytes
                    // used, its region and the blob's payer entry (for `finish_trim`);
                    // a child is its destination's identity and its offset. Plain words.
                    // A writer held as a value: the pointer to its frame in the build
                    // (MEM-4), scoped and never cleaned.
                    Some(Role::Writer | Role::Dest) => {
                        let body = record(vec![("frame", FieldTy::Borrow)]);
                        return self.fill(r, LRepr::Scoped, body, None);
                    }
                    Some(role @ (Role::SpanBuilder | Role::Builder | Role::Child)) => {
                        let k = if role == Role::Builder { 5 } else { 2 };
                        let words = FieldTy::Array(Box::new(FieldTy::Scalar(Scalar::U64)), k);
                        return self.fill(r, LRepr::Own, record(vec![("words", words)]), None);
                    }
                    // A box or counted reference held as a value: its one pointer field,
                    // cleaned by the recipe (freed, or released, MEM-15/16).
                    Some(role @ (Role::Box | Role::Rc | Role::Arc)) => {
                        let to = self.shape(args[0])?;
                        let field = match role {
                            Role::Box => FieldTy::Box(to),
                            Role::Rc => FieldTy::Rc(to),
                            _ => FieldTy::Arc(to),
                        };
                        let body = record(vec![("node", field)]);
                        return self.fill(r, LRepr::Own, body, None);
                    }
                    // A provider handle's carrier: its one handle field, cleaned by the
                    // runtime (closing a channel side, abandoning a task, MEM-24).
                    Some(
                        role @ (Role::Sender
                        | Role::Receiver
                        | Role::Task
                        | Role::HostFile
                        | Role::Causes),
                    ) => {
                        let handle = match role {
                            Role::Sender => Handle::Sender,
                            Role::Receiver => Handle::Receiver,
                            Role::Task => Handle::Task,
                            Role::HostFile => Handle::HostFile,
                            _ => Handle::Causes,
                        };
                        let body = record(vec![("handle", FieldTy::Handle(handle))]);
                        return self.fill(r, LRepr::Own, body, None);
                    }
                    // A capability, Args, Unique or Site: its one u64 word,
                    // never cleaned (the provider owns it).
                    _ if handle(decl) || matches!(decl.role, Some(Role::Unique | Role::Site)) => {
                        let body = record(vec![("handle", FieldTy::Scalar(Scalar::U64))]);
                        return self.fill(r, LRepr::Own, body, None);
                    }
                    Some(_) => return None,
                    None => {}
                }
                if decl.summary.cycle {
                    return None;
                }
                // A compact stable value is held like `Text`: its locator of the
                // compact row (FORM #65), which every compact context names instead.
                if decl.repr == Repr::Stable && decl.compact.is_some() {
                    let at = FieldTy::Locator(Box::new(FieldTy::Inline(self.compact(ty)?)));
                    return self.fill(r, LRepr::Own, record(vec![("at", at)]), None);
                }
                let repr = match decl.repr {
                    // Option and Result follow their arguments' kind (OWN-3): over a
                    // view they are scoped, and hold the view as a borrow.
                    Repr::Own
                        if decl.structural
                            && args.iter().any(|&a| {
                                crate::decls::kind(self.decls, &*self.types, a, &[])
                                    == crate::decls::Kind::Scoped
                            }) =>
                    {
                        LRepr::Scoped
                    }
                    Repr::Own => LRepr::Own,
                    Repr::Resource => LRepr::Resource,
                    Repr::Scoped => LRepr::Scoped,
                    Repr::Stable => LRepr::Stable,
                };
                let body = self.body(decl, &args, false)?;
                // A body module names the function row once the emitter has one.
                let mut hook = |kind, f: Option<u32>| match f {
                    Some(f) if self.module => {
                        self.hooks.push((r, kind, f, args.clone()));
                        Some(FuncRef(u32::MAX))
                    }
                    Some(_) => Some(FuncRef(d.0)),
                    None => None,
                };
                let finalizer = hook(Hook::Finalizer, decl.finalizer);
                self.shapes[r.index()].grant = decl.grant;
                (repr, body, finalizer)
            }
            _ => return None,
        };
        self.fill(r, repr, body, finalizer)
    }
    /// The compact row of the compact stable type `ty` (FORM #65), named
    /// `compact <canonical>`: its fields encoded in the blob (MEM-9).
    fn compact(&mut self, ty: TyId) -> Option<TypeRef> {
        let name = format!(
            "compact {}",
            crate::decls::layout_name(self.decls, &*self.types, ty)
        );
        if let Some(&r) = self.of.get(&name) {
            return Some(r);
        }
        let TyKind::Adt(d, args) = self.types.kind(ty).clone() else {
            return None;
        };
        let decls = self.decls;
        let decl = &decls[d.index()];
        // A structural `Option` inside compact data (only of a link) is as wide as any
        // container: its one link word takes the container's width.
        let width = match decl.compact {
            Some(w) => w,
            None if decl.structural => hirsel_core_layout::Width::U64,
            None => return None,
        };
        let r = TypeRef(self.shapes.len() as u32);
        self.shapes.push(Shape {
            name: name.clone(),
            repr: LRepr::Compact(width),
            carried: Carried::Address,
            body: Body::Record(vec![]),
            finalizer: None,
            clone: None,
            grant: 0,
        });
        self.of.insert(name, r);
        let body = self.body(decl, &args, true)?;
        self.shapes[r.index()].body = body;
        Some(r)
    }
    /// The row a compact context names for `ty`: a compact stable type's compact row.
    fn compact_or_shape(&mut self, ty: TyId) -> Option<TypeRef> {
        match *self.types.kind(ty) {
            TyKind::Adt(d, _)
                if self.decls[d.index()].repr == Repr::Stable
                    && self.decls[d.index()].compact.is_some() =>
            {
                self.compact(ty)
            }
            TyKind::Anchored(x, _) | TyKind::View(x) => self.compact_or_shape(x),
            _ => self.shape(ty),
        }
    }
    /// A declaration's fields at `args`, each as a compact context when `compact`.
    fn body(
        &mut self,
        decl: &crate::decls::TypeDecl,
        args: &[TyId],
        compact: bool,
    ) -> Option<Body> {
        let map = |fs: &[crate::decls::FieldDecl], s: &mut Self| {
            let mut out = vec![];
            for f in fs {
                let fty = subst(s.types, f.ty, args);
                out.push(Field {
                    name: f.name.clone(),
                    ty: s.field_in(fty, compact)?,
                });
            }
            Some(out)
        };
        Some(match &decl.body {
            DeclBody::Record(fs) => Body::Record(map(fs, self)?),
            DeclBody::Variant(cs) => {
                let mut cases = vec![];
                for c in cs {
                    cases.push(Case {
                        name: c.name.clone(),
                        fields: map(&c.fields, self)?,
                    });
                }
                Body::Variant(cases)
            }
            DeclBody::Alias(_) => return None,
        })
    }
    fn fill(
        &mut self,
        r: TypeRef,
        repr: LRepr,
        body: Body,
        finalizer: Option<FuncRef>,
    ) -> Option<TypeRef> {
        let shape = &mut self.shapes[r.index()];
        shape.repr = repr;
        shape.body = body;
        shape.finalizer = finalizer;
        word_carriage(self.shapes, r.index());
        Some(r)
    }
    /// Forget the rows from `mark` on: a type whose parts do not all map leaves no
    /// partial rows behind.
    pub fn truncate(&mut self, mark: usize) {
        self.shapes.truncate(mark);
        self.of.retain(|_, r| (r.0 as usize) < mark);
        self.hooks.retain(|(r, ..)| (r.0 as usize) < mark);
    }

    /// The table block of a `SeqIndex` of `elem` (MEM-12, MEM-24): the sequence's
    /// locator at 0, the count at 16, then `count` u64 offsets of each element from
    /// the span's first element byte, at 24 (outside the row, inside the block,
    /// which is charged round8(24 + 8 count)). Hash, equality and order read the
    /// row: the located sequence, and the count it determines.
    fn seq_index_block(&mut self, ty: TyId, elem: TyId) -> Option<TypeRef> {
        let name = format!(
            "{} block",
            crate::decls::canonical(self.decls, &*self.types, ty)
        );
        if let Some(&r) = self.of.get(&name) {
            return Some(r);
        }
        let seq = FieldTy::Locator(Box::new(FieldTy::SeqSpan(Box::new(
            self.field_in(elem, true)?,
        ))));
        let r = TypeRef(self.shapes.len() as u32);
        self.shapes.push(Shape {
            name: name.clone(),
            repr: LRepr::Stable,
            carried: Carried::Address,
            body: record(vec![("seq", seq), ("count", FieldTy::Scalar(Scalar::U64))]),
            finalizer: None,
            clone: None,
            grant: 0,
        });
        self.of.insert(name, r);
        Some(r)
    }
    fn map_field(&mut self, args: &[TyId]) -> Option<FieldTy> {
        let (k, v) = (self.field(args[0])?, self.field(args[1])?);
        Some(FieldTy::Map(Box::new(k), Box::new(v)))
    }
    pub(crate) fn field(&mut self, ty: TyId) -> Option<FieldTy> {
        self.field_in(ty, false)
    }

    fn field_in(&mut self, ty: TyId, compact: bool) -> Option<FieldTy> {
        Some(match self.types.kind(ty).clone() {
            TyKind::Scalar(s) => FieldTy::Scalar(s),
            TyKind::Anchored(x, _) => self.field_in(x, compact)?,
            TyKind::Adt(d, args) => match self.decls[d.index()].role {
                Some(Role::Box) => FieldTy::Box(self.shape(args[0])?),
                Some(Role::Rc) => FieldTy::Rc(self.shape(args[0])?),
                Some(Role::Arc) => FieldTy::Arc(self.shape(args[0])?),
                Some(Role::List) => FieldTy::Seq(Box::new(self.field(args[0])?)),
                Some(Role::String) => FieldTy::Seq(Box::new(FieldTy::Scalar(Scalar::U8))),
                Some(Role::Map) => FieldTy::Inline(self.shape(ty)?),
                Some(Role::Log | Role::StaticFn) => FieldTy::Scalar(Scalar::U64),
                _ if handle(&self.decls[d.index()]) => FieldTy::Scalar(Scalar::U64),
                Some(Role::Task) => FieldTy::Handle(Handle::Task),
                Some(Role::Sender) => FieldTy::Handle(Handle::Sender),
                Some(Role::Receiver) => FieldTy::Handle(Handle::Receiver),
                Some(Role::HostFile) => FieldTy::Handle(Handle::HostFile),
                Some(Role::Causes) => FieldTy::Handle(Handle::Causes),
                Some(Role::Text) => {
                    if compact {
                        FieldTy::Text
                    } else {
                        FieldTy::Locator(Box::new(FieldTy::Text))
                    }
                }
                Some(Role::FixedSpan) => {
                    let span = FieldTy::FixedSpan(Box::new(self.field_in(args[0], true)?));
                    if compact {
                        span
                    } else {
                        FieldTy::Locator(Box::new(span))
                    }
                }
                Some(Role::SeqSpan) => {
                    let span = FieldTy::SeqSpan(Box::new(self.field_in(args[0], true)?));
                    if compact {
                        span
                    } else {
                        FieldTy::Locator(Box::new(span))
                    }
                }
                Some(Role::IndexedSpan) => {
                    let span = FieldTy::IndexedSpan(Box::new(self.field_in(args[0], true)?));
                    if compact {
                        span
                    } else {
                        FieldTy::Locator(Box::new(span))
                    }
                }
                // A SeqIndex is held by its table block, a stable value in its region
                // (MEM-12, FORM M14).
                Some(Role::SeqIndex) => FieldTy::Stable(self.seq_index_block(ty, args[0])?),
                Some(Role::Link) if compact => FieldTy::Link(self.compact_or_shape(args[0])?),
                // A view field is the viewed type's own bytes, cleaned by nobody (FORM #56).
                Some(Role::View) => view_field(self.field_in(args[0], compact)?),
                Some(Role::Dest | Role::Writer) => FieldTy::Borrow,
                Some(Role::Child | Role::SpanBuilder) => {
                    FieldTy::Array(Box::new(FieldTy::Scalar(Scalar::U64)), 2)
                }
                Some(Role::Builder) => FieldTy::Array(Box::new(FieldTy::Scalar(Scalar::U64)), 5),
                Some(Role::Unique | Role::Site) => FieldTy::Scalar(Scalar::U64),
                Some(_) => return None,
                None if compact
                    && self.decls[d.index()].structural
                    && crate::decls::is_core_prelude(&self.decls[d.index()].path, &["Option"]) =>
                {
                    // MEM-10's optional link is a compact variant of its own, whose empty
                    // case is the link's zero distance (a niche the layout assigns).
                    match self.types.kind(args[0]).clone() {
                        TyKind::Adt(link, _)
                            if self.decls[link.index()].role == Some(Role::Link) =>
                        {
                            FieldTy::Inline(self.compact(ty)?)
                        }
                        _ => FieldTy::Inline(self.shape(ty)?),
                    }
                }
                None if self.decls[d.index()].repr == Repr::Stable
                    && self.decls[d.index()].compact.is_some() =>
                {
                    let k = FieldTy::Inline(self.compact(ty)?);
                    if compact {
                        k
                    } else {
                        FieldTy::Locator(Box::new(k))
                    }
                }
                None if !compact && self.decls[d.index()].repr == Repr::Stable => {
                    FieldTy::Stable(self.shape(ty)?)
                }
                None => FieldTy::Inline(self.shape(ty)?),
            },
            TyKind::Array(x, n) => {
                if !matches!(
                    crate::decls::kind(self.decls, self.types, x, &[]),
                    crate::Kind::Plain | crate::Kind::Own
                ) {
                    return None;
                }
                FieldTy::Array(Box::new(self.field(x)?), n)
            }
            TyKind::Tuple(_) | TyKind::Unit | TyKind::Never => FieldTy::Inline(self.shape(ty)?),
            TyKind::Param(_) if self.module => FieldTy::Inline(self.shape(ty)?),
            TyKind::View(x) => view_field(self.field_in(x, compact)?),
            TyKind::Fn(_, _, _, callable) => match callable {
                Callable::Plain => FieldTy::Fn,
                Callable::Own | Callable::OwnSend => FieldTy::OwnFn,
                Callable::Scoped => FieldTy::Borrow,
            },
            _ => return None,
        })
    }
}

/// A variant row whose bytes are one scalar word is carried as that word (FORM #78);
/// layout's `word` decides. Every other row keeps the type rule's [`carried`]. A
/// substituted generic row is decided again once its payload is known.
pub(crate) fn word_carriage(shapes: &mut [Shape], row: usize) {
    crate::cost_scope!("phase", "layout/word carriage");
    if matches!(shapes[row].body, Body::Variant(_)) {
        shapes[row].carried = hirsel_core_layout::word(shapes, &shapes[row])
            .map_or(Carried::Address, |s| Carried::Value(Ty::Scalar(s)));
    }
}

/// The field holding a view of a value held as `inner`: its bytes again when `inner`
/// already is a view.
fn view_field(inner: FieldTy) -> FieldTy {
    match inner {
        FieldTy::View(_) => inner,
        _ => FieldTy::View(Box::new(inner)),
    }
}

/// How every signature carries a value of `ty`: the closed rule, read by the body
/// emitter's `rep` and published on each row (FORM #64) so a generic instance's
/// signature reads it for its substituted rows. None for a type no value is built of.
pub(crate) fn carried(
    decls: &crate::Rows<crate::decls::TypeDecl>,
    types: &impl TyRead,
    ty: TyId,
) -> Option<Carried> {
    Some(match types.kind(ty) {
        TyKind::Scalar(s) => Carried::Value(Ty::Scalar(*s)),
        TyKind::Fn(_, _, _, Callable::Plain) => Carried::Value(Ty::Ptr),
        TyKind::Unit | TyKind::Never | TyKind::Error => Carried::Nothing,
        // A Region value is the region's pointer, scoped and never cleaned by its
        // holder: the block that opened it closes it (MEM-5, MEM-7). A writer is the
        // pointer to its frame in the build (MEM-4), scoped and never cleaned.
        TyKind::Adt(d, _)
            if matches!(
                decls[d.index()].role,
                Some(Role::Region | Role::Writer | Role::Dest | Role::StaticFn)
            ) =>
        {
            if decls[d.index()].role == Some(Role::StaticFn) {
                Carried::Value(Ty::Scalar(Scalar::U64))
            } else {
                Carried::Value(Ty::Ptr)
            }
        }
        // A capability or `Args` is a provider handle, plain and never cleaned by the
        // program (FORM e68de117); `Unique` and `Site` are one plain word (core.md
        // "Core types").
        TyKind::Adt(d, _)
            if handle(&decls[d.index()])
                || matches!(decls[d.index()].role, Some(Role::Unique | Role::Site)) =>
        {
            Carried::Value(Ty::Scalar(Scalar::U64))
        }
        // A value anchored in a region is held like its type; the anchor only bounds
        // where it may go (MEM-6). A view is carried exactly as its type (FORM #56).
        TyKind::Anchored(x, _) | TyKind::View(x) => return carried(decls, types, *x),
        TyKind::Tuple(_)
        | TyKind::Array(..)
        | TyKind::Adt(..)
        | TyKind::Fn(..)
        | TyKind::Param(_) => Carried::Address,
        _ => return None,
    })
}

/// A capability or `Args`: a `u64` provider handle (FORM e68de117).
pub(crate) fn handle(decl: &crate::decls::TypeDecl) -> bool {
    decl.role == Some(Role::Capability) || crate::decls::is_core_prelude(&decl.path, &["Args"])
}

fn record(fields: Vec<(&str, FieldTy)>) -> Body {
    Body::Record(
        fields
            .into_iter()
            .map(|(name, ty)| Field {
                name: name.into(),
                ty,
            })
            .collect(),
    )
}

/// Recipes for every declaration without parameters whose fields all map.
pub(crate) fn recipes(p: &mut Program) {
    let Program { decls, types, .. } = p;
    let (mut shapes, mut of) = (vec![], HashMap::default());
    let mut s = Shapes {
        decls,
        types,
        shapes: &mut shapes,
        of: &mut of,
        module: false,
        hooks: vec![],
    };
    let mut roots = vec![];
    for (i, decl) in s.decls.own_iter().enumerate() {
        let d = s.decls.base_len() + i;
        if !decl.generics.is_empty()
            || decl.role.is_some()
            || matches!(decl.body, DeclBody::Alias(_))
        {
            continue;
        }
        let ty = s.types.intern(TyKind::Adt(crate::DeclId(d as u32), vec![]));
        let mark = s.shapes.len();
        // A compact declaration's recipe is its compact row's (FORM #65).
        let row = if decl.repr == Repr::Stable && decl.compact.is_some() {
            s.shape(ty).and_then(|_| s.compact(ty))
        } else {
            s.shape(ty)
        };
        match row {
            Some(r) => roots.push((d, r)),
            None => s.truncate(mark),
        }
    }
    match compute(s.shapes) {
        Ok(recipes) => {
            for (d, r) in roots {
                p.decls[d].summary.recipe = Some(recipes[r.index()].clone());
            }
        }
        Err(LayoutError::NativeRange { ty, field, bytes }) => {
            // PROG-6: past the engine's 64-bit size range is the implementation's
            // resource refusal, at the field that reaches it.
            if let Some(decl) = s.decls.iter().find(|d| d.path.to_string() == ty) {
                let site = decl
                    .all_fields()
                    .find(|f| f.name == field)
                    .map_or(decl.site, |f| f.site);
                p.resource_refusals.push(crate::ResourceRefusal {
                    site,
                    message: format!(
                        "the native layout of `{ty}` needs {bytes} bytes at field `{field}`, \
                         past the engine's size range of {} bytes",
                        u64::MAX
                    ),
                });
            }
        }
        Err(error) => {
            let (name, field, why) = match &error {
                LayoutError::Field { ty, field, why } => (ty, Some(field.as_str()), *why),
                LayoutError::Cycle { ty } => (ty, None, "inline containment cycle"),
                LayoutError::TooLarge { ty } => (ty, None, "compact extent exceeds its width"),
                LayoutError::Empty { ty } => (ty, None, "compact encoding has zero size"),
                LayoutError::Carried { ty } => (ty, None, "row carried unlike its body"),
                LayoutError::NativeRange { .. } => unreachable!("matched above"),
            };
            let declaration = s.decls.iter().find(|d| {
                let path = d.path.to_string();
                path == *name || format!("compact {path}") == *name
            });
            if let Some(decl) = declaration {
                let site = field
                    .and_then(|name| decl.all_fields().find(|f| f.name == name))
                    .map_or(decl.site, |f| f.site);
                if p.refusals.iter().any(|r| r.primary == site) {
                    return;
                }
                let rule = if why.contains("overflow") || why.contains("2^64") {
                    "OWN-2"
                } else if decl.compact.is_some() {
                    "MEM-9"
                } else if decl.repr == Repr::Stable {
                    "MEM-2"
                } else if matches!(error, LayoutError::Cycle { .. }) {
                    "OWN-2"
                } else {
                    "OWN-19"
                };
                p.refusals.push(crate::refusal(rule, site, why));
            } else if let Some(&(d, _)) = roots.first() {
                p.gaps.push(crate::Gap::new(
                    s.decls[d].site,
                    "nested layout failure is not diagnosed",
                ));
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use hirsel_core_ids::published::{Descriptor, Provider, Table, bind};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    struct Prefix {
        row: crate::TypeDecl,
        reads: Arc<AtomicUsize>,
    }
    impl Provider<crate::TypeDecl> for Prefix {
        fn row(&self, _: u32) -> std::io::Result<crate::TypeDecl> {
            self.reads.fetch_add(1, Ordering::Relaxed);
            Ok(self.row.clone())
        }
    }
    #[test]
    fn recipes_skip_undemanded_published_prefix() {
        let (index, modules) = crate::tests::program_inputs("type Local { n: u64 }\n");
        let mut p = Program::declare(&index, &modules);
        assert!(p.refusals.is_empty(), "{:?}", p.refusals);
        let mut local = p
            .decls
            .iter()
            .find(|d| d.path.0.name == "Local")
            .unwrap()
            .clone();
        local.summary.recipe = None;
        let reads = Arc::new(AtomicUsize::new(0));
        let descriptor = Descriptor {
            namespace: hirsel_core_ids::ContentHash::of_bytes(b"layout-prefix-test"),
            table: 0,
            len: 128,
            rows: Default::default(),
            lookup: Default::default(),
        };
        let prefix = Table::published(bind(
            descriptor,
            Arc::new(Prefix {
                row: local.clone(),
                reads: Arc::clone(&reads),
            }),
        ));
        p.decls = crate::Rows::overlay(Arc::new(prefix));
        p.decls.push(local);
        recipes(&mut p);
        assert_eq!(
            reads.load(Ordering::Relaxed),
            0,
            "unrelated library declarations were decoded"
        );
        let recipe = p.decls[128].summary.recipe.as_ref().unwrap();
        let hirsel_core_layout::Layout::Fixed(fixed) = &recipe.layout else {
            panic!("fixed u64 record")
        };
        assert_eq!((fixed.size, fixed.align), (8, 8));
    }
}
