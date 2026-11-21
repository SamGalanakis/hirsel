//! PROG-9 identity bytes: the one encoder of `code.identity`.
//!
//! Version 1 is the byte `0x01` followed by the UTF-8 text `D S`. `D` is the definition
//! identity, with every byte of a component other than an ASCII letter, digit or `_`
//! written `%XX`. `S` is the header's meaning: no parameter names; the default mode is
//! not written; no generics list; roots `^i` (a region generic, numbered by first
//! naming in the parameters then the result, any named in neither next in declared
//! order) and `#i` (a parameter by its position); type paths are definition identities, aliases expanded; a missing result is
//! `->()`; `waits`; outlives pairs sorted and each once. No whitespace except one space
//! between two adjacent words (keywords, identifiers, numbers, definition identities,
//! and the root names `^i` and `#i`).
use crate::decls::Role;
use crate::decls::{FnDecl, TypeDecl};
use crate::types::*;
use hirsel_core_ids::DefPath;
use std::fmt::Write;

/// The identity bytes of a top-level function with no type parameters, or `None` for a
/// header PROG-9 does not describe (one that `code.describe` refuses). Region generics
/// are numbered as the writer first meets them, so the declared order is not part of the
/// bytes.
pub(crate) fn code_identity(
    decls: &crate::Rows<TypeDecl>,
    t: &impl TyRead,
    fd: &FnDecl,
) -> Option<Vec<u8>> {
    let TyKind::Fn(params, result, waits, Callable::Plain) = t.kind(fd.callable).clone() else {
        return None;
    };
    if fd.generics.iter().any(|g| !g.region) {
        return None;
    }
    let mut w = Words {
        decls,
        s: String::new(),
        word: false,
        named: Vec::with_capacity(fd.generics.len()),
    };
    w.definition(&fd.path);
    w.callable(t, &params, result, waits)?;
    // Region generics named in neither the parameters nor the result, in declared order.
    for i in 0..fd.generics.len() as u32 {
        w.number(i);
    }
    let mut pairs = Vec::with_capacity(fd.outlives.len());
    for &(a, b) in &fd.outlives {
        // The front keeps the reflexive transitive closure, each pair once; `a: a` is
        // no clause.
        if a != b {
            pairs.push((w.key(root_key(t, a)?), w.key(root_key(t, b)?)));
        }
    }
    pairs.sort_unstable();
    for (i, (a, b)) in pairs.iter().enumerate() {
        if i == 0 {
            w.word("where");
        } else {
            w.punct(",");
        }
        w.written_root(*a);
        w.punct(":");
        w.written_root(*b);
    }
    let mut bytes = Vec::with_capacity(w.s.len() + 1);
    bytes.push(1);
    bytes.extend_from_slice(w.s.as_bytes());
    Some(bytes)
}

/// A root a header can name: a region generic (`^i`, sorted first, `i` its declared
/// position) or a parameter (`#i`, by position).
fn root_key(t: &impl TyRead, root: TyId) -> Option<(u8, u32)> {
    match t.kind(root) {
        TyKind::Region(Root::Generic(i)) => Some((0, *i)),
        TyKind::Region(Root::Parameter(i))
        | TyKind::Region(Root::CallableParameter { index: i, .. }) => Some((1, *i)),
        _ => None,
    }
}

struct Words<'a> {
    decls: &'a crate::Rows<TypeDecl>,
    s: String,
    /// The last thing written was a word, so the next word is preceded by one space.
    word: bool,
    /// Declared region generic positions in the order first named: `^k` is `named[k]`.
    named: Vec<u32>,
}

impl Words<'_> {
    fn word(&mut self, w: &str) {
        if self.word {
            self.s.push(' ');
        }
        self.s.push_str(w);
        self.word = true;
    }

    fn punct(&mut self, p: &str) {
        self.s.push_str(p);
        self.word = false;
    }

    /// The written number of the declared region generic `i`, the next one when it is
    /// named here first.
    fn number(&mut self, i: u32) -> u32 {
        match self.named.iter().position(|&n| n == i) {
            Some(k) => k as u32,
            None => {
                self.named.push(i);
                self.named.len() as u32 - 1
            }
        }
    }

    /// A root key with a region generic renumbered to its written number.
    fn key(&mut self, (kind, i): (u8, u32)) -> (u8, u32) {
        if kind == 0 {
            (0, self.number(i))
        } else {
            (1, i)
        }
    }

    fn root(&mut self, root: (u8, u32)) {
        let written = self.key(root);
        self.written_root(written);
    }

    fn written_root(&mut self, (kind, i): (u8, u32)) {
        self.word(&format!("{}{i}", if kind == 0 { '^' } else { '#' }));
    }

    fn definition(&mut self, path: &DefPath) {
        self.word(&path.definition_identity());
    }

    fn callable(
        &mut self,
        t: &impl TyRead,
        params: &[(Mode, TyId)],
        result: TyId,
        waits: bool,
    ) -> Option<()> {
        self.punct("(");
        for (i, &(mode, p)) in params.iter().enumerate() {
            if i > 0 {
                self.punct(",");
            }
            match mode {
                Mode::View => {}
                Mode::Mut => self.word("mut"),
                Mode::Take => self.word("take"),
                Mode::Bytes => self.word("bytes"),
                Mode::Steps => self.word("steps"),
            }
            self.ty(t, p)?;
        }
        self.punct(")");
        self.punct("->");
        self.ty(t, result)?;
        if waits {
            self.word("waits");
        }
        Some(())
    }

    fn list(&mut self, t: &impl TyRead, xs: &[TyId]) -> Option<()> {
        for (i, &x) in xs.iter().enumerate() {
            if i > 0 {
                self.punct(",");
            }
            self.ty(t, x)?;
        }
        Some(())
    }

    fn ty(&mut self, t: &impl TyRead, ty: TyId) -> Option<()> {
        match t.kind(ty).clone() {
            TyKind::Scalar(x) => self.word(x.name()),
            TyKind::Unit => self.punct("()"),
            TyKind::Never => self.definition(&DefPath::core("core.Never")),
            TyKind::Tuple(xs) => {
                self.punct("(");
                self.list(t, &xs)?;
                self.punct(")");
            }
            TyKind::Adt(d, xs) => {
                self.definition(&self.decls[d.index()].path);
                if !xs.is_empty() {
                    self.punct("[");
                    self.list(t, &xs)?;
                    self.punct("]");
                }
            }
            TyKind::View(x) => {
                // `View` is a prelude type: its definition identity, as any other path.
                let view = self.decls.iter().find(|d| d.role == Some(Role::View))?;
                self.definition(&view.path);
                self.punct("[");
                self.ty(t, x)?;
                self.punct("]");
            }
            TyKind::Vector(v) => {
                self.word("Simd");
                self.punct("[");
                self.word(v.lane().name());
                self.punct(",");
                self.word(&v.lanes().to_string());
                self.punct("]");
            }
            TyKind::Array(x, n) => {
                self.word("Inline");
                self.punct("[");
                self.ty(t, x)?;
                self.punct(",");
                let mut n_text = String::new();
                let _ = write!(n_text, "{n}");
                self.word(&n_text);
                self.punct("]");
            }
            // A root, as an anchor (`T in r`) or a region type argument (`in r`).
            TyKind::Region(_) => {
                self.word("in");
                self.root(root_key(t, ty)?);
            }
            TyKind::Anchored(x, r) => {
                self.ty(t, x)?;
                self.ty(t, r)?;
            }
            TyKind::Fn(ps, r, w, k) => {
                match k {
                    Callable::Plain => {}
                    Callable::Own => self.word("own"),
                    Callable::OwnSend => {
                        self.word("own");
                        self.punct("[");
                        self.word("send");
                        self.punct("]");
                    }
                    Callable::Scoped => self.word("scoped"),
                    Callable::ScopedView => {
                        self.word("scoped");
                        self.punct("[");
                        self.word("view");
                        self.punct("]");
                    }
                }
                self.word("fn");
                self.callable(t, &ps, r, w)?;
            }
            TyKind::Error | TyKind::Hole | TyKind::Param(_) => return None,
        }
        Some(())
    }
}
