//! The checker's type-size limits: PROG-6 resource refusals that end a request whose
//! declared types expand past what checking one request may spend.
//!
//! Most type queries walk a type's tree, each field of a record or variant at its
//! arguments and each part of a tuple, without remembering the subtrees they have seen.
//! An alias, record or variant that holds the one before it twice doubles that tree at
//! each step, so forty such lines make a tree of 2^40 nodes from forty lines of source.
//! The limits below are counted once per declaration, by one pass that remembers each
//! closed type it has counted, before any other pass walks a field. A declaration past
//! one is refused with a message naming the amount and the limit, and so is the request.
//!
//! - [`TYPE_NODES`]: the nodes of a declaration's full expansion, counting a type once
//!   at each place it occurs: an alias by what it names, a record or variant by its
//!   fields at their arguments, a tuple by its parts, an array by its element once,
//!   whatever its length. A core type with a role (`Box`, `List`, a span) is one node:
//!   what it points at is a type of its own.
//! - [`TYPE_DEPTH`]: the longest path in that expansion, so no recursive query of the
//!   checker goes deeper than this many types into a declaration.
//! - [`LAYOUT_BYTES`]: the native size of a declared type without parameters. A value
//!   past FAIL-17's single-frame ceiling cannot be a local of any function, so no
//!   accepted function could make one; it is refused at its declaration rather than
//!   laid out for every build.

use crate::Program;
use crate::decls::{DeclBody, TypeDecl};
use crate::types::*;
use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};

/// Nodes in one declaration's full type expansion. The largest that core, std or a
/// core program declares has 2,108 (fleet's `MoveCluster`), and a doubling chain
/// passes it at its sixteenth step.
pub const TYPE_NODES: u64 = 1 << 16;
/// Types on the longest path of one declaration's expansion. The deepest that core,
/// std or a core program declares nests 15.
pub const TYPE_DEPTH: u32 = 256;
/// Bytes of one declared type's native layout: FAIL-17's single-frame ceiling. The
/// largest that core, std or a core program declares is 20,920 bytes.
pub const LAYOUT_BYTES: u64 = hirsel_core_layout::stack::FRAME_CEILING;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Extent {
    pub nodes: u64,
    pub depth: u32,
}

impl Extent {
    const LEAF: Extent = Extent { nodes: 1, depth: 1 };
    fn within(self) -> bool {
        self.nodes <= TYPE_NODES && self.depth <= TYPE_DEPTH
    }
}

/// The expansion of a type deeper than [`TYPE_DEPTH`]: the count stopped there.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TooDeep;

/// Remembered extents of the closed and generic types counted so far in one request.
pub(crate) struct Expansion<'d> {
    decls: &'d crate::Rows<TypeDecl>,
    counted: HashMap<TyId, Extent>,
    open: HashSet<TyId>,
}

impl<'d> Expansion<'d> {
    pub(crate) fn new(decls: &'d crate::Rows<TypeDecl>) -> Self {
        Self {
            decls,
            counted: HashMap::default(),
            open: HashSet::default(),
        }
    }

    /// The types one level inside `ty` in its expansion.
    fn parts(&self, t: &mut Types, ty: TyId) -> Vec<TyId> {
        match t.kind(ty).clone() {
            TyKind::Tuple(xs) => xs,
            TyKind::Array(x, _) | TyKind::Anchored(x, _) => vec![x],
            TyKind::Adt(d, args) => {
                let decl = &self.decls[d.index()];
                match &decl.body {
                    // A core type with a role holds what it holds behind a pointer
                    // or a span, laid out and walked as a type of its own.
                    _ if decl.role.is_some() => vec![],
                    DeclBody::Alias(x) => vec![subst(t, *x, &args)],
                    _ => {
                        let fields: Vec<TyId> = decl.all_fields().map(|f| f.ty).collect();
                        fields.into_iter().map(|f| subst(t, f, &args)).collect()
                    }
                }
            }
            TyKind::Error
            | TyKind::Never
            | TyKind::Unit
            | TyKind::Scalar(_)
            | TyKind::Region(_)
            | TyKind::Param(_)
            | TyKind::Fn(..)
            | TyKind::View(_)
            | TyKind::Hole
            | TyKind::Vector(_) => vec![],
        }
    }

    /// The extent of `ty`'s expansion, counted at most [`TYPE_DEPTH`] levels down from a
    /// root. A type that reaches itself is one node where it does: only a declaration
    /// that holds itself inline, refused by OWN-2, or one behind a pointer.
    pub(crate) fn extent(&mut self, t: &mut Types, ty: TyId) -> Result<Extent, TooDeep> {
        self.extent_at(t, ty, 1)
    }

    fn extent_at(&mut self, t: &mut Types, ty: TyId, level: u32) -> Result<Extent, TooDeep> {
        if let Some(&e) = self.counted.get(&ty) {
            return Ok(e);
        }
        if level > TYPE_DEPTH {
            return Err(TooDeep);
        }
        if !self.open.insert(ty) {
            return Ok(Extent::LEAF);
        }
        let mut e = Extent::LEAF;
        let mut result = Ok(());
        for part in self.parts(t, ty) {
            match self.extent_at(t, part, level + 1) {
                Ok(p) => {
                    e.nodes = e.nodes.saturating_add(p.nodes);
                    e.depth = e.depth.max(p.depth.saturating_add(1));
                }
                Err(deep) => {
                    result = Err(deep);
                    break;
                }
            }
        }
        self.open.remove(&ty);
        result?;
        self.counted.insert(ty, e);
        Ok(e)
    }

    fn within(&mut self, t: &mut Types, ty: TyId) -> bool {
        self.extent(t, ty).is_ok_and(Extent::within)
    }
}

/// The type a declaration stands for, at its own parameters.
fn declared(t: &mut Types, decl: &TypeDecl, d: usize) -> TyId {
    if let DeclBody::Alias(x) = decl.body {
        return x;
    }
    let args = (0..decl.generics.len())
        .map(|i| t.intern(TyKind::Param(i as u32)))
        .collect();
    t.intern(TyKind::Adt(DeclId(d as u32), args))
}

impl Program<'_> {
    /// Refuse each of the request's declarations whose expansion passes [`TYPE_NODES`]
    /// or [`TYPE_DEPTH`] while every type one level inside it is within both, so a
    /// doubling chain is refused once, at the line that crosses. True when it refused.
    pub(crate) fn type_expansion_limits(&mut self) -> bool {
        let Program {
            decls,
            types,
            resource_refusals,
            ..
        } = self;
        let mut expansion = Expansion::new(decls);
        let before = resource_refusals.len();
        for d in decls.base_len()..decls.len() {
            let decl = &decls[d];
            if decl.role.is_some() {
                continue;
            }
            let ty = declared(types, decl, d);
            let message = match expansion.extent(types, ty) {
                Ok(e) if e.within() => continue,
                Ok(e) if e.nodes > TYPE_NODES => format!(
                    "the type `{}` expands to {} type nodes, past the checker's limit of \
                     {TYPE_NODES} nodes in one declaration",
                    decl.path, e.nodes
                ),
                Ok(e) => format!(
                    "the type `{}` nests types {} deep, past the checker's limit of \
                     {TYPE_DEPTH} levels in one declaration",
                    decl.path, e.depth
                ),
                Err(TooDeep) => format!(
                    "the type `{}` nests types more than {TYPE_DEPTH} deep, the checker's \
                     limit of levels in one declaration",
                    decl.path
                ),
            };
            let parts = expansion.parts(types, ty);
            if parts.into_iter().all(|x| expansion.within(types, x)) {
                resource_refusals.push(crate::ResourceRefusal {
                    site: decl.site,
                    message,
                });
            }
        }
        resource_refusals.len() > before
    }

    /// The request's functions whose bodies are checked: none once a declaration is
    /// refused for resources (PROG-6), since checking a body would spend what the
    /// limits withhold, walking the types they refused.
    pub fn checked_bodies(&self) -> std::ops::Range<usize> {
        if self.resource_refusals.is_empty() {
            self.fns.base_len()..self.fns.len()
        } else {
            0..0
        }
    }

    /// Refuse each laid-out declaration whose fixed native size passes [`LAYOUT_BYTES`]
    /// while no declared type it holds inline does.
    pub(crate) fn layout_size_limit<'r>(
        &mut self,
        roots: impl Iterator<Item = (usize, &'r hirsel_core_layout::Layout)>,
    ) {
        let over: Vec<(usize, u64)> = roots
            .filter_map(|(d, layout)| match layout {
                hirsel_core_layout::Layout::Fixed(f) if f.size > LAYOUT_BYTES => Some((d, f.size)),
                _ => None,
            })
            .collect();
        let refused: HashSet<usize> = over.iter().map(|&(d, _)| d).collect();
        for (d, size) in over {
            let decl = &self.decls[d];
            let holds_over = decl
                .all_fields()
                .any(|f| mentions(&self.decls, &self.types, f.ty, &refused));
            if holds_over {
                continue;
            }
            self.resource_refusals.push(crate::ResourceRefusal {
                site: decl.site,
                message: format!(
                    "the type `{}` is {size} bytes, past the checker's limit of \
                     {LAYOUT_BYTES} bytes for one declared type (no function's frame can \
                     hold a larger value, FAIL-17)",
                    decl.path
                ),
            });
        }
    }
}

/// Whether `ty` holds, inline, a declaration in `over`. Counted types are within
/// [`TYPE_NODES`], so this walk is bounded.
fn mentions(
    decls: &crate::Rows<TypeDecl>,
    t: &impl TyRead,
    ty: TyId,
    over: &HashSet<usize>,
) -> bool {
    match t.kind(ty) {
        TyKind::Tuple(xs) => xs.iter().any(|&x| mentions(decls, t, x, over)),
        TyKind::Array(x, _) | TyKind::Anchored(x, _) => mentions(decls, t, *x, over),
        TyKind::Adt(d, args) => {
            over.contains(&d.index())
                || (decls[d.index()].role.is_none()
                    && args.iter().any(|&x| mentions(decls, t, x, over)))
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests;
