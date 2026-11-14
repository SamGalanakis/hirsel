//! PROG-9 identity bytes: the one encoder of `code.identity`.
//!
//! Version 1 is the byte `0x01` followed by the UTF-8 text `D S`. `D` is the definition
//! identity, with every byte of a component other than an ASCII letter, digit or `_`
//! written `%XX`. `S` is the header's meaning: no parameter names; the default mode is
//! not written; region generics `in ^i`; roots `^i` (region) and `#i` (parameter by
//! position); type paths are definition identities, aliases expanded; a missing result is
//! `->()`; `waits`; outlives pairs sorted and each once. No whitespace except one space
//! between two adjacent words (keywords, identifiers, numbers, definition identities,
//! and the root names `^i` and `#i`).
use crate::decls::Role;
use crate::decls::{FnDecl, TypeDecl};
use crate::types::*;
use hirsel_core_ids::DefPath;
use std::fmt::Write;

/// The identity bytes of a top-level function with no type parameters, or `None` for a
/// header PROG-9 does not describe (one that `code.describe` refuses).
pub(crate) fn code_identity(
    decls: &crate::Rows<TypeDecl>,
    t: &impl TyRead,
    fd: &FnDecl,
) -> Option<Vec<u8>> {
    let TyKind::Fn(params, result, waits, Callable::Plain) = t.kind(fd.callable).clone() else {
        return None;
    };
    let mut w = Words {
        decls,
        s: String::new(),
        word: false,
    };
    w.definition(&fd.path);
    if !fd.generics.is_empty() {
        w.punct("[");
        for (i, g) in fd.generics.iter().enumerate() {
            if !g.region {
                return None;
            }
            if i > 0 {
                w.punct(",");
            }
            w.word("in");
            w.word(&format!("^{i}"));
        }
        w.punct("]");
    }
    w.callable(t, &params, result, waits)?;
    let mut pairs = Vec::with_capacity(fd.outlives.len());
    for &(a, b) in &fd.outlives {
        // The front keeps the reflexive transitive closure, each pair once; `a: a` is
        // no clause.
        if a != b {
            pairs.push((root_key(t, a)?, root_key(t, b)?));
        }
    }
    pairs.sort_unstable();
    for (i, (a, b)) in pairs.iter().enumerate() {
        if i == 0 {
            w.word("where");
        } else {
            w.punct(",");
        }
        w.root(*a);
        w.punct(":");
        w.root(*b);
    }
    let mut bytes = Vec::with_capacity(w.s.len() + 1);
    bytes.push(1);
    bytes.extend_from_slice(w.s.as_bytes());
    Some(bytes)
}

/// A root a header can name: a region generic (`^i`, sorted first) or a parameter
/// (`#i`), each by position.
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

    fn root(&mut self, (kind, i): (u8, u32)) {
        self.word(&format!("{}{i}", if kind == 0 { '^' } else { '#' }));
    }

    fn definition(&mut self, path: &DefPath) {
        let item = &path.0;
        let mut d = String::new();
        escape(&item.package.name, &mut d);
        d.push('@');
        d.push_str(&item.package.version);
        for part in item.modules.iter().chain(std::iter::once(&item.name)) {
            d.push('/');
            escape(part, &mut d);
        }
        self.word(&d);
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
                }
                self.word("fn");
                self.callable(t, &ps, r, w)?;
            }
            TyKind::Error | TyKind::Hole | TyKind::Never | TyKind::Param(_) => return None,
        }
        Some(())
    }
}

/// A component of a definition identity: ASCII letters, digits and `_` as themselves,
/// every other byte as `%` and two upper-case hex digits.
fn escape(part: &str, out: &mut String) {
    for &b in part.as_bytes() {
        if b.is_ascii_alphanumeric() || b == b'_' {
            out.push(b as char);
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
}
