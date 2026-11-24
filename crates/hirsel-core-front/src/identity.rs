//! PROG-9 identity bytes: the one encoder of `code.identity`.
//!
//! Version 1 is the byte `0x01` followed by the UTF-8 text `D A S`. `D` is the definition
//! identity, with every byte of a component other than an ASCII letter, digit or `_`
//! written `%XX`. `A` is the type arguments and their declared dictionaries, in the order
//! their parameters first occur in `S`. `S` is the header's meaning: no parameter names; the default mode is
//! not written; no generics list; roots `^i` (a region generic, numbered by first
//! naming in the parameters then the result, any named in neither next in declared
//! order) and `#i` (a parameter by its position); type paths are definition identities, aliases expanded; a missing result is
//! `->()`; `waits`; outlives pairs sorted and each once. No whitespace except one space
//! between two adjacent words (keywords, identifiers, numbers, definition identities,
//! and the root names `^i` and `#i`).
use crate::decls::Role;
use crate::decls::{FnDecl, TypeDecl, declared_bound_names};
use crate::types::*;
use hirsel_core_ids::DefPath;
use std::fmt::Write;

/// The identity of a top-level function or an instance. Arguments use the declaration's
/// generic slots, including region slots, which have no entry in `A`. Pass an empty
/// slice for a function without type parameters.
pub(crate) fn code_identity(
    decls: &crate::Rows<TypeDecl>,
    t: &impl TyRead,
    fd: &FnDecl,
    args: &[TyId],
) -> Option<Vec<u8>> {
    identity_recipe(decls, t, fd, args, None)?.render(&[])
}

/// The source spellings accompany the key's layout rows: a layout erases anchors
/// and its name is spelled for PROG-10. The key records the selected dictionaries.
pub(crate) fn instance_identity(
    decls: &crate::Rows<TypeDecl>,
    t: &impl TyRead,
    fd: &FnDecl,
    key: &hirsel_core_ir::InstanceKey,
    source_args: &[TyId],
) -> Option<DescriptorRecipe> {
    identity_recipe(
        decls,
        t,
        fd,
        source_args.get(..key.arguments.len())?,
        Some(key),
    )
}

fn identity_recipe(
    decls: &crate::Rows<TypeDecl>,
    t: &impl TyRead,
    fd: &FnDecl,
    args: &[TyId],
    key: Option<&hirsel_core_ir::InstanceKey>,
) -> Option<DescriptorRecipe> {
    let TyKind::Fn(params, result, waits, Callable::Plain) = t.kind(fd.callable).clone() else {
        return None;
    };
    let type_count = fd.generics.iter().filter(|g| !g.region).count();
    if type_count > 0 && args.len() != fd.generics.len() {
        return None;
    }
    let mut w = Words::new(decls, true);
    w.callable(t, &params, result, waits)?;
    // Region generics named in neither the parameters nor the result, in declared order.
    for (i, g) in fd.generics.iter().enumerate() {
        if g.region {
            number(&mut w.regions, i as u32);
        }
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
    if w.parameters.len() != type_count {
        return None;
    }
    let mut prefix = Words::new(decls, false);
    prefix.recipe = Some(DescriptorRecipe::default());
    prefix.definition(key.map_or(&fd.path, |key| &key.definition));
    if type_count > 0 {
        prefix.punct("[");
        for (i, &slot) in w.parameters.iter().enumerate() {
            if i > 0 {
                prefix.punct(",");
            }
            prefix.ty(t, args[slot as usize])?;
        }
        prefix.punct("]");
        let mut separator = "{";
        let mut dictionaries = Vec::new();
        if let Some(key) = key {
            let mut selected = key.dictionaries.iter();
            dictionaries.extend(fd.generics.iter().map(|generic| {
                // The instance key stores dictionaries by slot, then bound bit.
                (0..u16::BITS)
                    .filter(|&bit| generic.bounds & (1 << bit) != 0)
                    .filter_map(|_| selected.next()?.split_once('[').map(|(bound, _)| bound))
                    .collect::<Vec<_>>()
            }));
        }
        for &slot in &w.parameters {
            for bound in declared_bound_names(fd.generics[slot as usize].bounds)
                .filter(|bound| key.is_none() || dictionaries[slot as usize].contains(bound))
            {
                prefix.punct(separator);
                prefix.word(bound);
                prefix.punct("[");
                prefix.ty(t, args[slot as usize])?;
                prefix.punct("]");
                separator = ",";
            }
        }
        if separator == "," {
            prefix.punct("}");
        }
    }
    prefix.punct(&w.s);
    prefix.recipe
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct DescriptorRecipe {
    parts: Vec<DescriptorPart>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
enum DescriptorPart {
    Text { text: String, word: bool },
    Argument(u32),
}

impl DescriptorRecipe {
    pub(crate) fn render(&self, arguments: &[String]) -> Option<Vec<u8>> {
        let mut out = String::from("\x01");
        let mut previous_word = false;
        for part in &self.parts {
            match part {
                DescriptorPart::Text { text, word } => {
                    write_token(&mut out, &mut previous_word, text, *word);
                }
                DescriptorPart::Argument(i) => {
                    append_layout_type(&mut out, &mut previous_word, arguments.get(*i as usize)?)?;
                }
            }
        }
        Some(out.into_bytes())
    }
}

fn write_token(out: &mut String, previous_word: &mut bool, text: &str, word: bool) {
    if *previous_word && word {
        out.push(' ');
    }
    out.push_str(text);
    *previous_word = word;
}

/// A specialized argument is available by its closed layout name. Instance
/// layouts erase anchors and spell paths with package versions and raw
/// components. Reframe the tokens as PROG-9 words and definition identities.
fn append_layout_type(out: &mut String, previous_word: &mut bool, mut text: &str) -> Option<()> {
    fn delimiter(c: char) -> bool {
        matches!(c, ' ' | '(' | ')' | '[' | ']' | ',')
    }
    while !text.is_empty() {
        let (token, word) = if let Some(rest) = text.strip_prefix(' ') {
            text = rest;
            continue;
        } else if let Some(rest) = text.strip_prefix("->") {
            text = rest;
            ("->", false)
        } else if text.starts_with(delimiter) {
            let (punct, rest) = text.split_at(1);
            text = rest;
            if punct == "," && text.starts_with(')') {
                continue;
            }
            (punct, false)
        } else {
            let (word, rest) = text.split_at(text.find(delimiter).unwrap_or(text.len()));
            text = rest;
            (word, true)
        };
        let definition = match token {
            "View" => Some(DefPath::core("core.View")),
            "Never" => Some(DefPath::core("core.Never")),
            "in" => return None,
            _ if token.contains('@') => Some(DefPath::from_full(token)?),
            _ => None,
        };
        if let Some(definition) = definition {
            write_token(out, previous_word, &definition.definition_identity(), true);
        } else {
            write_token(out, previous_word, token, word);
        }
    }
    Some(())
}

/// Number a declared slot by its first occurrence.
fn number(slots: &mut Vec<u32>, slot: u32) -> u32 {
    match slots.iter().position(|&n| n == slot) {
        Some(i) => i as u32,
        None => {
            slots.push(slot);
            slots.len() as u32 - 1
        }
    }
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
    regions: Vec<u32>,
    /// Declared type generic positions in the order first named.
    parameters: Vec<u32>,
    header: bool,
    recipe: Option<DescriptorRecipe>,
}

impl<'a> Words<'a> {
    fn new(decls: &'a crate::Rows<TypeDecl>, header: bool) -> Self {
        Self {
            decls,
            s: String::new(),
            word: false,
            regions: vec![],
            parameters: vec![],
            header,
            recipe: None,
        }
    }

    fn word(&mut self, word: &str) {
        self.token(word, true);
    }

    fn punct(&mut self, punct: &str) {
        self.token(punct, false);
    }

    fn token(&mut self, text: &str, word: bool) {
        if let Some(recipe) = &mut self.recipe {
            recipe.parts.push(DescriptorPart::Text {
                text: text.to_owned(),
                word,
            });
        } else {
            write_token(&mut self.s, &mut self.word, text, word);
        }
    }

    /// A root key with a region generic renumbered to its written number.
    fn key(&mut self, (kind, i): (u8, u32)) -> (u8, u32) {
        if kind == 0 {
            (0, number(&mut self.regions, i))
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
            TyKind::Region(_) if self.header => {
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
            TyKind::Param(slot) if self.header => {
                let index = number(&mut self.parameters, slot);
                self.word(&format!("${index}"));
            }
            TyKind::Param(slot) => self
                .recipe
                .as_mut()?
                .parts
                .push(DescriptorPart::Argument(slot)),
            TyKind::Error | TyKind::Hole | TyKind::Region(_) => return None,
        }
        Some(())
    }
}
