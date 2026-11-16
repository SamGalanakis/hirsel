//! SEM-10's codec derive. Each type that declares `codec` gets four items, written as
//! Hirsel text after the file's own and parsed with it, so the checker checks them as it
//! checks the file (EXPANSION.md section 0). Their names hold `$`, which only generated text
//! may lex, so no program names them or collides with them; `codec.of` reaches the maker
//! `D$codec` through the checker, and a site in a generated item is reported at its
//! declaration `D`.
//!
//! The text is generated from the syntax alone: field and payload types, default literals
//! and the declared bounds are copied as written, and each codec a field needs comes from a
//! typed `let` of `codec.of()`.
use crate::*;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

/// What a package's generated text calls std's codec module (`codec` inside std,
/// `std.codec` in a package that depends on std), and the module's PROG-1 path, which
/// prefixes each definition's schema path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Derive {
    pub codec: String,
    pub path: String,
}

/// The parse of a file whose types declare `codec`, with their generated items appended.
/// A file without one, or with a syntax refusal, is returned as parsed.
pub fn derive(
    output: ParseOutput,
    mode: ParseMode,
    symbols: &mut Symbols,
    d: &Derive,
) -> ParseOutput {
    if mode != ParseMode::Source
        || !output.diagnostics.is_empty()
        || output.resource_refusal.is_some()
    {
        return output;
    }
    let generated = generate(&output.syntax, d);
    if generated.is_empty() {
        return output;
    }
    let mut text = output.syntax.source;
    if !text.ends_with('\n') {
        text.push('\n');
    }
    let from = text.len();
    text.push_str(&generated);
    parser::parse_derived(&text, mode, symbols, from)
}

/// The declaration a generated item `D$..` was made for: `D`, whose site reports it.
pub fn derived_from(name: &str) -> Option<&str> {
    name.split_once('$').map(|(d, _)| d)
}

/// Where a site in item `item` of `syntax` is reported when the derive generated that
/// item: the item declaring its type, and the span of the type's name in it. None for an
/// item someone wrote.
pub fn declaration_of(syntax: &SyntaxFile, item: usize) -> Option<(usize, Span)> {
    let named = |i: &Item<'_>| {
        let token = syntax.tree.node(i.root).token?;
        Some((token, syntax.tree.token_text(&syntax.source, token)))
    };
    if item >= syntax.items().len() {
        return None; // a module's own site, as a parse failure's
    }
    let (_, name) = named(&syntax.item(item))?;
    let declared = derived_from(name)?;
    syntax.items().enumerate().find_map(|(i, other)| {
        if !matches!(
            syntax.tree.node(other.root).kind,
            NodeKind::TypeDecl | NodeKind::StableType
        ) {
            return None;
        }
        let (token, name) = named(&other)?;
        (name == declared).then(|| (i, other.token_span(token)))
    })
}

struct Decl<'a> {
    name: &'a str,
    /// Each parameter with its written bounds, if any.
    generics: Vec<(&'a str, Option<&'a str>)>,
    body: Body<'a>,
    /// The bare names of this module's types its field and payload types mention.
    mentions: BTreeSet<&'a str>,
}
enum Body<'a> {
    Record(Vec<Entry<'a>>),
    Sum(Vec<Variant<'a>>),
}
struct Entry<'a> {
    name: &'a str,
    /// The type as written, for a default's codec.
    text: &'a str,
    ty: Ty<'a>,
    default: Option<&'a str>,
    removed: bool,
}
struct Variant<'a> {
    name: &'a str,
    payload: Vec<(Option<&'a str>, Ty<'a>)>,
    removed: bool,
}
/// A field type as section 0's table reads it.
enum Ty<'a> {
    /// `put_X`, `take_X`, `schema_X` and the width X encodes to.
    Scalar(&'static str, u64),
    List(&'a str),
    Map(&'a str, &'a str),
    Option(&'a str),
    Result(&'a str, &'a str),
    /// A nominal type or a parameter: through its codec.
    With(&'a str),
}

struct File<'a> {
    syntax: &'a SyntaxFile,
}
impl<'a> File<'a> {
    fn text(&self, item: &Item<'a>, n: NodeId) -> &'a str {
        let span = self.syntax.tree.node(n).origin.span;
        let base = item.file_offset as usize;
        &self.syntax.source[base + span.start as usize..base + span.end as usize]
    }
    fn name(&self, n: NodeId) -> &'a str {
        let token = self.syntax.tree.node(n).token.expect("a named node");
        self.syntax.tree.token_text(&self.syntax.source, token)
    }
    fn kind(&self, n: NodeId) -> NodeKind {
        self.syntax.tree.node(n).kind
    }
    fn children(&self, n: NodeId) -> &'a [NodeId] {
        self.syntax.tree.children(n)
    }
    fn ty(&self, item: &Item<'a>, n: NodeId) -> Ty<'a> {
        let c = self.children(n);
        match self.kind(n) {
            NodeKind::UnitType => Ty::Scalar("unit", 0),
            NodeKind::GroupType => self.ty(item, c[0]),
            NodeKind::TypePath => match self.text(item, n) {
                "bool" => Ty::Scalar("bool", 1),
                "char" => Ty::Scalar("char", 4),
                "u8" => Ty::Scalar("u8", 1),
                "u16" => Ty::Scalar("u16", 2),
                "u32" => Ty::Scalar("u32", 4),
                "u64" => Ty::Scalar("u64", 8),
                "i8" => Ty::Scalar("i8", 1),
                "i16" => Ty::Scalar("i16", 2),
                "i32" => Ty::Scalar("i32", 4),
                "i64" => Ty::Scalar("i64", 8),
                "f32" => Ty::Scalar("f32", 4),
                "f64" => Ty::Scalar("f64", 8),
                "String" => Ty::Scalar("string", 8),
                "Bytes" => Ty::List("u8"),
                other => Ty::With(other),
            },
            NodeKind::TypeArguments => {
                let arg = |i: usize| self.text(item, c[i]);
                match (self.text(item, c[0]), c.len()) {
                    ("List", 2) => Ty::List(arg(1)),
                    ("Option", 2) => Ty::Option(arg(1)),
                    ("Map", 3) => Ty::Map(arg(1), arg(2)),
                    ("Result", 3) => Ty::Result(arg(1), arg(2)),
                    _ => Ty::With(self.text(item, n)),
                }
            }
            _ => Ty::With(self.text(item, n)),
        }
    }
    /// Every bare type name under `n`.
    fn mentions(&self, item: &Item<'a>, n: NodeId, into: &mut BTreeSet<&'a str>) {
        if self.kind(n) == NodeKind::TypePath {
            into.insert(self.text(item, n));
        }
        for &c in self.children(n) {
            self.mentions(item, c, into);
        }
    }
    fn decl(&self, item: &Item<'a>) -> Option<Decl<'a>> {
        let root = item.root;
        if !matches!(self.kind(root), NodeKind::TypeDecl | NodeKind::StableType) {
            return None;
        }
        let mut generics = vec![];
        let mut codec = false;
        let mut body = None;
        let mut mentions = BTreeSet::new();
        for &c in self.children(root) {
            match self.kind(c) {
                NodeKind::Generics => {
                    for &g in self.children(c) {
                        if self.kind(g) != NodeKind::Generic {
                            return None; // a region parameter: no codec type has one
                        }
                        let bounds = self.children(g).first().map(|&b| self.text(item, b));
                        generics.push((self.name(g), bounds));
                    }
                }
                NodeKind::Bounds => {
                    codec = self.children(c).iter().any(|&b| self.name(b) == "codec");
                }
                NodeKind::CodecVia => return None, // not derived yet: `of` refuses it as a gap
                NodeKind::Fields => {
                    let entries = self.children(c).iter().map(|&f| {
                        let fc = self.children(f);
                        self.mentions(item, fc[0], &mut mentions);
                        Entry {
                            name: self.name(f),
                            text: self.text(item, fc[0]),
                            ty: self.ty(item, fc[0]),
                            default: fc.get(1).map(|&d| self.text(item, d)),
                            removed: self.kind(f) == NodeKind::RemovedField,
                        }
                    });
                    body = Some(Body::Record(entries.collect()));
                }
                NodeKind::Variants => {
                    let variants = self.children(c).iter().map(|&v| Variant {
                        name: self.name(v),
                        payload: self
                            .children(v)
                            .iter()
                            .map(|&p| {
                                let t = self.children(p)[0];
                                self.mentions(item, t, &mut mentions);
                                (
                                    self.syntax.tree.node(p).token.map(|_| self.name(p)),
                                    self.ty(item, t),
                                )
                            })
                            .collect(),
                        removed: self.kind(v) == NodeKind::RemovedVariant,
                    });
                    body = Some(Body::Sum(variants.collect()));
                }
                _ => {}
            }
        }
        for (g, _) in &generics {
            mentions.remove(g);
        }
        Some(Decl {
            name: self.name(root),
            generics,
            body: body.filter(|_| codec)?,
            mentions,
        })
    }
}

/// The generated text for every type of `syntax` that declares `codec`, outside a
/// recursive group: a decode of one is a frame loop (SEM-10), not yet generated, so `of`
/// at it is refused as a gap.
fn generate(syntax: &SyntaxFile, d: &Derive) -> String {
    let file = File { syntax };
    let decls: Vec<_> = syntax.items().filter_map(|item| file.decl(&item)).collect();
    let names: BTreeMap<_, _> = decls.iter().enumerate().map(|(i, d)| (d.name, i)).collect();
    let mut out = String::new();
    for (i, decl) in decls.iter().enumerate() {
        if !reaches(&decls, &names, i, i, &mut BTreeSet::new()) {
            Gen::new(d, decl).items(&mut out);
        }
    }
    out
}

/// Whether `from`'s fields reach `to` through this module's codec declarations.
fn reaches(
    decls: &[Decl],
    names: &BTreeMap<&str, usize>,
    from: usize,
    to: usize,
    seen: &mut BTreeSet<usize>,
) -> bool {
    decls[from]
        .mentions
        .iter()
        .filter_map(|m| names.get(m))
        .any(|&n| n == to || (seen.insert(n) && reaches(decls, names, n, to, seen)))
}

struct Gen<'a> {
    c: &'a str,
    path: String,
    decl: &'a Decl<'a>,
    /// `D[P1, ..]`, `[P1: codec + .., ..]`.
    ty: String,
    generics: String,
    /// The codec lets of the function being written, by type text.
    lets: BTreeMap<&'a str, String>,
    let_text: String,
}
impl<'a> Gen<'a> {
    fn new(d: &'a Derive, decl: &'a Decl<'a>) -> Self {
        let params: Vec<_> = decl.generics.iter().map(|(n, _)| *n).collect();
        let (ty, generics) = if params.is_empty() {
            (decl.name.to_owned(), String::new())
        } else {
            let bounds: Vec<_> = decl
                .generics
                .iter()
                .map(|(n, b)| match b {
                    Some(b) if b.split('+').any(|w| w.trim() == "codec") => format!("{n}: {b}"),
                    Some(b) => format!("{n}: codec + {b}"),
                    None => format!("{n}: codec"),
                })
                .collect();
            (
                format!("{}[{}]", decl.name, params.join(", ")),
                format!("[{}]", bounds.join(", ")),
            )
        };
        Gen {
            c: &d.codec,
            path: format!("{}.{}", d.path, decl.name),
            decl,
            ty,
            generics,
            lets: BTreeMap::new(),
            let_text: String::new(),
        }
    }
    /// The local holding `of[t]()`, let at the top of the current function.
    fn of(&mut self, t: &'a str) -> String {
        let n = self.lets.len();
        let c = self.c;
        let text = &mut self.let_text;
        self.lets
            .entry(t)
            .or_insert_with(|| {
                writeln!(text, "    let c${n}: {c}.Codec[{t}] = {c}.of()").unwrap();
                format!("c${n}")
            })
            .clone()
    }
    /// The function's lets, then its body; the lets start empty for the next function.
    fn function(&mut self, out: &mut String, header: &str, body: &str) {
        let lets = std::mem::take(&mut self.let_text);
        self.lets.clear();
        let _ = writeln!(out, "{header} {{\n{lets}{body}}}\n");
    }
    fn put(&mut self, ty: &Ty<'a>, v: &str) -> String {
        let c = self.c;
        match *ty {
            Ty::Scalar(x, _) => format!("{c}.put_{x}({v}, mut out$)"),
            Ty::List(e) => format!("{c}.put_list({v}, {}, mut out$)", self.of(e)),
            Ty::Option(e) => format!("{c}.put_option({v}, {}, mut out$)", self.of(e)),
            Ty::Map(k, w) => format!("{c}.put_map({v}, {}, {}, mut out$)", self.of(k), self.of(w)),
            Ty::Result(t, e) => format!(
                "{c}.put_result({v}, {}, {}, mut out$)",
                self.of(t),
                self.of(e)
            ),
            Ty::With(t) => format!("{c}.put_with({v}, {}, mut out$)", self.of(t)),
        }
    }
    fn take(&mut self, ty: &Ty<'a>) -> String {
        let c = self.c;
        match *ty {
            Ty::Scalar(x, _) => format!("{c}.take_{x}(b$, mut at$)?"),
            Ty::List(e) => format!("{c}.take_list(b$, mut at$, {})?", self.of(e)),
            Ty::Option(e) => format!("{c}.take_option(b$, mut at$, {})?", self.of(e)),
            Ty::Map(k, w) => format!("{c}.take_map(b$, mut at$, {}, {})?", self.of(k), self.of(w)),
            Ty::Result(t, e) => format!(
                "{c}.take_result(b$, mut at$, {}, {})?",
                self.of(t),
                self.of(e)
            ),
            Ty::With(t) => format!("{c}.take_with(b$, mut at$, {})?", self.of(t)),
        }
    }
    fn schema(&mut self, ty: &Ty<'a>) -> String {
        let c = self.c;
        match *ty {
            Ty::Scalar(x, _) => format!("    {c}.schema_{x}(mut s$)?\n"),
            Ty::List(e) => format!("    {c}.schema_list(mut s$, {})?\n", self.of(e)),
            Ty::Option(e) => format!("    {c}.schema_option(mut s$, {})?\n", self.of(e)),
            Ty::Map(k, w) => format!(
                "    {c}.schema_map(mut s$, {}, {})?\n",
                self.of(k),
                self.of(w)
            ),
            Ty::Result(t, e) => format!(
                "    {c}.schema_result(mut s$, {}, {})?\n",
                self.of(t),
                self.of(e)
            ),
            Ty::With(t) => format!("    {c}.schema_of(mut s$, {})?\n", self.of(t)),
        }
    }
    /// The fewest bytes a value of `ty` encodes to: a constant, or a `least` call.
    fn least(&mut self, ty: &Ty<'a>) -> Least {
        match *ty {
            Ty::Scalar(_, n) => Least::fixed(n),
            Ty::List(_) | Ty::Map(..) => Least::fixed(8),
            Ty::Option(_) | Ty::Result(..) => Least::fixed(4),
            Ty::With(t) => {
                let c = self.of(t);
                Least {
                    fixed: 0,
                    calls: vec![format!("{}.least({c})", self.c)],
                }
            }
        }
    }
    /// Statements, each but the last followed by `?`; none is `Ok(())`.
    fn sequence(steps: &[String], indent: &str) -> String {
        if steps.is_empty() {
            return format!("{indent}Ok(())\n");
        }
        let last = steps.len() - 1;
        steps
            .iter()
            .enumerate()
            .map(|(i, s)| format!("{indent}{s}{}\n", if i == last { "" } else { "?" }))
            .collect()
    }
    fn items(mut self, out: &mut String) {
        let decl = self.decl;
        let (name, ty, generics, c) = (
            self.decl.name,
            self.ty.clone(),
            self.generics.clone(),
            self.c,
        );
        let body = match &decl.body {
            Body::Record(entries) => self.record_encode(entries),
            Body::Sum(variants) => self.sum_encode(variants),
        };
        let header = format!(
            "fn {name}$encode{generics}(x$: {ty}, mut out$: Bytes) -> Result[(), Capacity]"
        );
        self.function(out, &header, &body);
        let body = match &decl.body {
            Body::Record(entries) => self.record_decode(entries),
            Body::Sum(variants) => self.sum_decode(variants),
        };
        let header = format!(
            "fn {name}$decode{generics}(b$: Bytes, mut at$: u64) -> Result[{ty}, {c}.Stop]"
        );
        self.function(out, &header, &body);
        let body = self.schema_body();
        let header = format!(
            "fn {name}$schema{generics}(mut s$: {c}.Schema, c$: {c}.Codec[{ty}]) -> Result[(), Capacity]"
        );
        self.function(out, &header, &body);
        let (lets, least) = self.least_body();
        let header = format!("fn {name}$codec{generics}() -> {c}.Codec[{ty}]");
        let body =
            format!("{lets}    {c}.make({name}$encode, {name}$decode, {name}$schema, {least})\n");
        self.function(out, &header, &body);
    }
    fn record_encode(&mut self, entries: &'a [Entry<'a>]) -> String {
        let steps: Vec<_> = entries
            .iter()
            .filter(|e| !e.removed)
            .map(|e| self.put(&e.ty, &format!("x$.{}", e.name)))
            .collect();
        Self::sequence(&steps, "    ")
    }
    /// The construction, its fields in declared order, each the value its take returns.
    fn record_decode(&mut self, entries: &'a [Entry<'a>]) -> String {
        let args: String = entries
            .iter()
            .filter(|e| !e.removed)
            .map(|e| format!("        {}: {},\n", e.name, self.take(&e.ty)))
            .collect();
        format!("    Ok({}(\n{args}    ))\n", self.decl.name)
    }
    fn sum_encode(&mut self, variants: &'a [Variant<'a>]) -> String {
        let c = self.c;
        let mut arms = String::new();
        for (tag, v) in variants.iter().enumerate().filter(|(_, v)| !v.removed) {
            let mut steps = vec![format!("{c}.put_tag({tag}, mut out$)")];
            let binds: Vec<_> = v
                .payload
                .iter()
                .enumerate()
                .map(|(i, (label, ty))| {
                    steps.push(self.put(ty, &format!("p${i}")));
                    label.map_or(format!("p${i}"), |l| format!("{l}: p${i}"))
                })
                .collect();
            let pattern = if binds.is_empty() {
                v.name.to_owned()
            } else {
                format!("{}({})", v.name, binds.join(", "))
            };
            if steps.len() == 1 {
                let _ = writeln!(arms, "        {pattern} => {},", steps[0]);
            } else {
                let _ = write!(
                    arms,
                    "        {pattern} => {{\n{}        }}\n",
                    Self::sequence(&steps, "            ")
                );
            }
        }
        format!("    match x$ {{\n{arms}    }}\n")
    }
    /// The tag, then the variant it names; a removed variant's tag is `Invalid` at the tag,
    /// as is no value's. `take_tag` refuses a tag past the count, so the last branch has no test.
    fn sum_decode(&mut self, variants: &'a [Variant<'a>]) -> String {
        let c = self.c;
        let removed = variants.iter().any(|v| v.removed);
        let mut body = String::new();
        if removed {
            body.push_str("    let start$ = at$\n");
        }
        let _ = writeln!(
            body,
            "    let tag$ = {c}.take_tag(b$, mut at$, {})?",
            variants.len()
        );
        let last = variants.len().saturating_sub(1);
        for (tag, v) in variants.iter().enumerate() {
            let value = if v.removed {
                "Error(Bad(Invalid(start$)))".to_owned()
            } else if v.payload.is_empty() {
                format!("Ok({})", v.name)
            } else {
                let args: Vec<_> = v
                    .payload
                    .iter()
                    .map(|(label, ty)| {
                        let take = self.take(ty);
                        label.map_or(take.clone(), |l| format!("{l}: {take}"))
                    })
                    .collect();
                format!("Ok({}({}))", v.name, args.join(", "))
            };
            let _ = match (tag, tag == last) {
                (0, true) => writeln!(body, "    {value}"),
                (0, false) => writeln!(body, "    if tag$ == 0 {{\n        {value}"),
                (_, false) => writeln!(body, "    }} else if tag$ == {tag} {{\n        {value}"),
                (_, true) => writeln!(body, "    }} else {{\n        {value}\n    }}"),
            };
        }
        body
    }
    /// `define`, an `argument` per parameter, `record` or `sum` (returning at once when the
    /// definition is already written), every entry in declared order with tombstones, `close`.
    fn schema_body(&mut self) -> String {
        let (c, decl) = (self.c, self.decl);
        let mut s = format!(
            "    {c}.define(mut s$, \"{}\", {})?\n",
            self.path,
            self.decl.generics.len()
        );
        for (p, _) in &decl.generics {
            let a = self.of(p);
            let _ = writeln!(s, "    {c}.argument(mut s$, {a})?");
        }
        let (kind, n) = match &decl.body {
            Body::Record(entries) => ("record", entries.len()),
            Body::Sum(variants) => ("sum", variants.len()),
        };
        let _ = writeln!(
            s,
            "    if {c}.{kind}(mut s$, {n})? {{\n        return Ok(())\n    }}"
        );
        match &decl.body {
            Body::Record(entries) => {
                for e in entries {
                    match (e.removed, e.default) {
                        (true, _) => {
                            let _ = writeln!(s, "    {c}.removed_field(mut s$, \"{}\")?", e.name);
                            s += &self.schema(&e.ty);
                        }
                        (false, Some(v)) => {
                            let f = self.of(e.text);
                            let _ = writeln!(
                                s,
                                "    {c}.field_default(mut s$, \"{}\", {f}, {v})?",
                                e.name
                            );
                        }
                        (false, None) => {
                            let _ = writeln!(s, "    {c}.field(mut s$, \"{}\")?", e.name);
                            s += &self.schema(&e.ty);
                        }
                    }
                }
            }
            Body::Sum(variants) => {
                for v in variants {
                    let call = if v.removed {
                        "removed_variant"
                    } else {
                        "variant"
                    };
                    let _ = writeln!(
                        s,
                        "    {c}.{call}(mut s$, \"{}\", {})?",
                        v.name,
                        v.payload.len()
                    );
                    for (_, ty) in &v.payload {
                        s += &self.schema(ty);
                    }
                }
            }
        }
        let _ = write!(s, "    {c}.close(mut s$)\n    Ok(())\n");
        s
    }
    /// The maker's LEAST: the sum over a record's live fields; for a sum, 4 plus the least
    /// over its live variants, a running minimum in lets when a payload's is a call.
    fn least_body(&mut self) -> (String, String) {
        let decl = self.decl;
        match &decl.body {
            Body::Record(entries) => {
                let l = entries
                    .iter()
                    .filter(|e| !e.removed)
                    .fold(Least::fixed(0), |l, e| l.plus(self.least(&e.ty)));
                (String::new(), l.text())
            }
            Body::Sum(variants) => {
                let mut fixed: Option<u64> = None;
                let mut called = vec![];
                for v in variants.iter().filter(|v| !v.removed) {
                    let l = v
                        .payload
                        .iter()
                        .fold(Least::fixed(0), |l, (_, ty)| l.plus(self.least(ty)));
                    if l.calls.is_empty() {
                        fixed = Some(fixed.map_or(l.fixed, |f| f.min(l.fixed)));
                    } else {
                        called.push(l.text());
                    }
                }
                if called.is_empty() {
                    return (String::new(), (4 + fixed.unwrap_or(0)).to_string());
                }
                let mut lets = String::new();
                let mut least = fixed.map(|f| f.to_string());
                for (i, v) in called.into_iter().enumerate() {
                    least = Some(match least {
                        None => v,
                        Some(m) => {
                            let _ = writeln!(
                                lets,
                                "    let v${i} = {v}\n    let m${i} = if v${i} < {m} {{ v${i} }} else {{ {m} }}"
                            );
                            format!("m${i}")
                        }
                    });
                }
                let least = least.map_or("4".to_owned(), |m| format!("4 + {m}"));
                (lets, least)
            }
        }
    }
}

/// A LEAST: a constant plus `least` calls.
struct Least {
    fixed: u64,
    calls: Vec<String>,
}
impl Least {
    fn fixed(n: u64) -> Self {
        Least {
            fixed: n,
            calls: vec![],
        }
    }
    fn plus(mut self, other: Least) -> Self {
        self.fixed += other.fixed;
        self.calls.extend(other.calls);
        self
    }
    fn text(self) -> String {
        let mut terms = self.calls;
        if self.fixed > 0 || terms.is_empty() {
            terms.insert(0, self.fixed.to_string());
        }
        terms.join(" + ")
    }
}
