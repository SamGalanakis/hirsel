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

/// The prefix a package's generated text puts before std's codec names (empty inside
/// std's codec module, `codec.` elsewhere in std, `std.codec.` in a package that depends
/// on std), and the module's PROG-1 path, which prefixes each definition's schema path.
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
    /// `via R (to: .., from: ..)`: the codec is R's, through the two functions.
    via: Option<Via<'a>>,
    /// The bare names of this module's types its codec mentions: its field and payload
    /// types, or a via type's R.
    mentions: BTreeSet<&'a str>,
}
struct Via<'a> {
    /// R as written.
    wire: &'a str,
    to: &'a str,
    from: &'a str,
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
    payload: Vec<Payload<'a>>,
    removed: bool,
}
struct Payload<'a> {
    label: Option<&'a str>,
    /// The type as written, for a frame's.
    text: &'a str,
    ty: Ty<'a>,
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
        let mut via = None;
        let mut mentions = BTreeSet::new();
        let mut wire = BTreeSet::new();
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
                NodeKind::CodecVia => {
                    let v = self.children(c);
                    self.mentions(item, v[0], &mut wire);
                    via = Some(Via {
                        wire: self.text(item, v[0]),
                        to: self.name(v[1]),
                        from: self.name(v[2]),
                    });
                }
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
                                Payload {
                                    label: self.syntax.tree.node(p).token.map(|_| self.name(p)),
                                    text: self.text(item, t),
                                    ty: self.ty(item, t),
                                }
                            })
                            .collect(),
                        removed: self.kind(v) == NodeKind::RemovedVariant,
                    });
                    body = Some(Body::Sum(variants.collect()));
                }
                _ => {}
            }
        }
        if via.is_some() {
            mentions = wire;
        }
        for (g, _) in &generics {
            mentions.remove(g);
        }
        Some(Decl {
            name: self.name(root),
            generics,
            body: body.filter(|_| codec)?,
            via,
            mentions,
        })
    }
}

/// The generated text for every type of `syntax` that declares `codec`. A member of a
/// recursive group decodes by a frame loop (SEM-10); a group the loop cannot take (see
/// [`Group::new`]) is not derived, so `of` at its members is refused as a gap.
fn generate(syntax: &SyntaxFile, d: &Derive) -> String {
    let file = File { syntax };
    let decls: Vec<_> = syntax.items().filter_map(|item| file.decl(&item)).collect();
    let names: BTreeMap<_, _> = decls.iter().enumerate().map(|(i, d)| (d.name, i)).collect();
    let reach = |a: usize, b: usize| reaches(&decls, &names, a, b, &mut BTreeSet::new());
    // Per declaration: None outside a recursive group, else its group, if derived.
    let mut group_of: Vec<Option<Option<(usize, usize)>>> = vec![None; decls.len()];
    let mut groups = vec![];
    for i in 0..decls.len() {
        if group_of[i].is_some() || !reach(i, i) {
            continue;
        }
        let members: Vec<_> = (0..decls.len())
            .filter(|&j| j == i || (reach(i, j) && reach(j, i)))
            .collect();
        let group = Group::new(members.iter().map(|&j| &decls[j]).collect());
        for (at, &j) in members.iter().enumerate() {
            group_of[j] = Some(group.as_ref().map(|_| (groups.len(), at)));
        }
        groups.extend(group);
    }
    let mut out = String::new();
    for (i, decl) in decls.iter().enumerate() {
        match group_of[i] {
            None => Gen::new(d, decl).items(&mut out),
            Some(Some((g, at))) => {
                let mut w = Gen::new(d, decl);
                w.group = Some((&groups[g], at));
                w.items(&mut out)
            }
            Some(None) => {}
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

/// A recursive group whose decode the frame loop writes: its members reach each other
/// only through a field or payload that is a member itself, or a List, Option or Map
/// value of one, at the member's own parameters. A generic member is the group's only
/// member. Anything else (a member inside a tuple, Result, a map key or another generic
/// type, a List of Lists, or reached at other arguments: the polymorphic recursion the
/// checker refuses) leaves the group underived.
struct Group<'a> {
    /// Each member and its type text, `D` or `D[P1, ..]`.
    members: Vec<(&'a Decl<'a>, String)>,
    /// Per member, per variant (a record is one), the live steps a decode reads.
    shapes: Vec<Vec<Shape<'a>>>,
}
struct Shape<'a> {
    /// The constructor: the record's name or the variant's.
    name: &'a str,
    removed: bool,
    steps: Vec<Step<'a>>,
}
struct Step<'a> {
    /// The record field's name or the payload's label.
    label: Option<&'a str>,
    /// The type as written.
    text: &'a str,
    ty: &'a Ty<'a>,
    /// The member it decodes, and how.
    edge: Option<(Edge<'a>, usize)>,
}
#[derive(Clone, Copy)]
enum Edge<'a> {
    Direct,
    Option,
    List,
    /// A map's value, keyed by this type.
    Map(&'a str),
}
impl<'a> Group<'a> {
    fn new(members: Vec<&'a Decl<'a>>) -> Option<Self> {
        if members.iter().any(|m| m.via.is_some())
            || members.len() > 1 && members.iter().any(|m| !m.generics.is_empty())
        {
            return None;
        }
        let members: Vec<_> = members
            .into_iter()
            .map(|m| {
                let ty = if m.generics.is_empty() {
                    m.name.to_owned()
                } else {
                    let params: Vec<_> = m.generics.iter().map(|(n, _)| *n).collect();
                    format!("{}[{}]", m.name, params.join(", "))
                };
                (m, ty)
            })
            .collect();
        let bare = |t: &str| t.chars().filter(|c| !c.is_whitespace()).collect::<String>();
        let named = |t: &str| {
            t.split(|c: char| !(c.is_alphanumeric() || c == '_'))
                .any(|w| members.iter().any(|(m, _)| m.name == w))
        };
        // The member `t` is at its own parameters; Err when `t` names a member otherwise.
        let member = |t: &str| -> Result<Option<usize>, ()> {
            let t = bare(t);
            match members
                .iter()
                .position(|(m, _)| t.split('[').next() == Some(m.name))
            {
                None => Ok(None),
                Some(i) if t == bare(&members[i].1) => Ok(Some(i)),
                Some(_) => Err(()),
            }
        };
        let step = |label, text: &'a str, ty: &'a Ty<'a>| -> Option<Step<'a>> {
            let edge = if !named(text) {
                None
            } else {
                Some(match *ty {
                    Ty::With(t) => (Edge::Direct, member(t).ok()??),
                    Ty::List(e) => (Edge::List, member(e).ok()??),
                    Ty::Option(e) => (Edge::Option, member(e).ok()??),
                    Ty::Map(k, e) if !named(k) => (Edge::Map(k), member(e).ok()??),
                    _ => return None,
                })
            };
            Some(Step {
                label,
                text,
                ty,
                edge,
            })
        };
        let mut shapes = vec![];
        for (m, _) in &members {
            shapes.push(match &m.body {
                Body::Record(entries) => vec![Shape {
                    name: m.name,
                    removed: false,
                    steps: entries
                        .iter()
                        .filter(|e| !e.removed)
                        .map(|e| step(Some(e.name), e.text, &e.ty))
                        .collect::<Option<_>>()?,
                }],
                Body::Sum(variants) => variants
                    .iter()
                    .map(|v| {
                        let steps = if v.removed {
                            vec![]
                        } else {
                            v.payload
                                .iter()
                                .map(|p| step(p.label, p.text, &p.ty))
                                .collect::<Option<_>>()?
                        };
                        Some(Shape {
                            name: v.name,
                            removed: v.removed,
                            steps,
                        })
                    })
                    .collect::<Option<_>>()?,
            });
        }
        Some(Group { members, shapes })
    }
    fn sum(&self, m: usize) -> bool {
        matches!(self.members[m].0.body, Body::Sum(_))
    }
    /// Member `m`'s LEAST when it is a constant: every step's is (OWN-2 keeps the
    /// members a member holds inline acyclic).
    fn least(&self, m: usize, depth: usize) -> Option<u64> {
        if depth > self.members.len() {
            return None;
        }
        let step = |s: &Step| match (s.edge, s.ty) {
            (Some((Edge::Direct, n)), _) => self.least(n, depth + 1),
            (Some((Edge::Option, _)), _) => Some(4),
            (Some(_), _) => Some(8),
            (None, Ty::Scalar(_, n)) => Some(*n),
            (None, Ty::List(_) | Ty::Map(..)) => Some(8),
            (None, Ty::Option(_) | Ty::Result(..)) => Some(4),
            (None, Ty::With(_)) => None,
        };
        let shape = |s: &Shape| s.steps.iter().map(step).sum::<Option<u64>>();
        let live = self.shapes[m].iter().filter(|s| !s.removed).map(shape);
        if !self.sum(m) {
            return live.sum();
        }
        let mut least: Option<u64> = None;
        for l in live {
            let l = l?;
            least = Some(least.map_or(l, |m| m.min(l)));
        }
        Some(4 + least.unwrap_or(0))
    }
}

/// Each line of `text` indented four more.
fn indent(text: &str) -> String {
    text.lines()
        .map(|l| {
            if l.is_empty() {
                "\n".to_owned()
            } else {
                format!("    {l}\n")
            }
        })
        .collect()
}

/// A tuple of `xs`, or the one element itself.
fn tuple(xs: &[String]) -> String {
    match xs {
        [x] => x.clone(),
        _ => format!("({})", xs.join(", ")),
    }
}

/// A recursive group member's decode, never on the call stack (SEM-10): one loop that
/// starts a member's value (`next$` says which, in a group of several), and on finishing
/// one, pops the frame waiting for it and resumes there. Each resume point (a step of a
/// member's variant that decodes a member) has a stack of frames: tuples of the steps
/// read before it and the edge's state (the list or map being filled, the count left, the
/// next key). With several resume points a stack of their numbers says whose frame is on
/// top, and a frame with nothing to hold is not stored. The value finished last is
/// `done$`, or the slot `d{m}$` of its member in a group of several. No type is generated:
/// the checker reads a `$` in a type's name as a template parameter.
struct Machine<'a> {
    g: &'a Group<'a>,
    entry: usize,
    /// Each resume point: member, variant, step.
    resumes: Vec<(usize, usize, usize)>,
}
impl<'a> Machine<'a> {
    fn new(g: &'a Group<'a>, entry: usize) -> Self {
        let mut resumes = vec![];
        for (m, shapes) in g.shapes.iter().enumerate() {
            for (v, shape) in shapes.iter().enumerate() {
                for (k, step) in shape.steps.iter().enumerate() {
                    if step.edge.is_some() {
                        resumes.push((m, v, k));
                    }
                }
            }
        }
        Machine { g, entry, resumes }
    }
    fn tagged(&self) -> bool {
        self.resumes.len() > 1
    }
    fn multi(&self) -> bool {
        self.g.members.len() > 1
    }
    fn step(&self, r: usize) -> &'a Step<'a> {
        let (m, v, k) = self.resumes[r];
        &self.g.shapes[m][v].steps[k]
    }
    fn resume_at(&self, m: usize, v: usize, k: usize) -> usize {
        self.resumes.iter().position(|&x| x == (m, v, k)).unwrap()
    }
    /// The types a frame of resume point `r` holds.
    fn frame(&self, r: usize) -> Vec<String> {
        let (m, v, k) = self.resumes[r];
        let steps = &self.g.shapes[m][v].steps;
        let mut types: Vec<_> = steps[..k].iter().map(|s| s.text.to_owned()).collect();
        match steps[k].edge {
            Some((Edge::List, _)) => types.extend([steps[k].text.to_owned(), "u64".into()]),
            Some((Edge::Map(key), _)) => {
                types.extend([steps[k].text.to_owned(), "u64".into(), key.to_owned()])
            }
            _ => {}
        }
        types
    }
    fn decode(&self, w: &mut Gen<'a>) -> String {
        let mut s = String::new();
        if self.tagged() {
            s += "let mut ts$: List[u64] = list.new()\n";
        }
        for r in 0..self.resumes.len() {
            let types = self.frame(r);
            if !types.is_empty() {
                let _ = writeln!(s, "let mut r{r}$: List[{}] = list.new()", tuple(&types));
            }
        }
        let mut body = String::new();
        let finish = if self.multi() {
            for (i, (_, ty)) in self.g.members.iter().enumerate() {
                let _ = writeln!(s, "let mut d{i}$: Option[{ty}] = None");
            }
            let _ = writeln!(s, "let mut next$: u64 = {}", self.entry);
            let last = self.g.members.len() - 1;
            for m in 0..=last {
                let _ = match m {
                    0 => writeln!(body, "if next$ == 0 {{"),
                    _ if m == last => writeln!(body, "}} else {{"),
                    _ => writeln!(body, "}} else if next$ == {m} {{"),
                };
                body += &indent(&self.start(w, m));
            }
            body += "}\n";
            format!(
                "return match replace(mut d{}$, None) {{ Some(y$) => Ok(take y$), None => Error(Bad(Invalid(at$))) }}",
                self.entry
            )
        } else {
            body += &self.start(w, 0);
            "return Ok(take done$)".to_owned()
        };
        let mut unwind = String::new();
        if self.tagged() {
            let _ = writeln!(
                unwind,
                "let t$ = match list.pop(mut ts$) {{\n    Some(y$) => y$,\n    None => {finish}\n}}"
            );
            let last = self.resumes.len() - 1;
            for r in 0..=last {
                let _ = match r {
                    0 => writeln!(unwind, "if t$ == 0 {{"),
                    _ if r == last => writeln!(unwind, "}} else {{"),
                    _ => writeln!(unwind, "}} else if t$ == {r} {{"),
                };
                unwind += &indent(&self.resume(w, r, &finish));
            }
            unwind += "}\n";
        } else {
            unwind += &self.resume(w, 0, &finish);
        }
        let _ = write!(body, "loop {{\n{}}}\n", indent(&unwind));
        let _ = write!(s, "loop {{\n{}}}\n", indent(&body));
        indent(&s)
    }
    /// Member `m`'s value from its first byte, finished into `done$` or its slot.
    fn start(&self, w: &mut Gen<'a>, m: usize) -> String {
        let shapes = &self.g.shapes[m];
        if !self.g.sum(m) {
            let (s, value) = self.seg(w, m, 0, 0, "a", "continue");
            return s + &self.finished(m, value, true);
        }
        let c = w.c;
        let mut s = String::new();
        if shapes.iter().any(|v| v.removed) {
            s += "let start$ = at$\n";
        }
        let _ = writeln!(s, "let tag$ = {c}take_tag(b$, mut at$, {})?", shapes.len());
        let mut branches = vec![];
        for v in 0..shapes.len() {
            branches.push(if shapes[v].removed {
                "return Error(Bad(Invalid(start$)))\n".to_owned()
            } else {
                let (b, value) = self.seg(w, m, v, 0, "a", "continue");
                match (self.multi(), value) {
                    (false, Some(value)) => format!("{b}{value}\n"),
                    (_, value) => b + &self.finished(m, value, false),
                }
            });
        }
        let single = !self.multi();
        if branches.len() == 1 {
            let b = branches.pop().unwrap();
            if single {
                // The one variant's statements, then its value as `done$`.
                let (b, value) = b.trim_end().rsplit_once('\n').unwrap_or(("", b.trim_end()));
                let _ = write!(s, "{b}\nlet mut done$ = {value}\n");
            } else {
                s += &b;
            }
            return s;
        }
        let last = branches.len() - 1;
        s += if single { "let mut done$ = " } else { "" };
        for (v, b) in branches.iter().enumerate() {
            let _ = match v {
                0 => writeln!(s, "if tag$ == 0 {{"),
                _ if v == last => writeln!(s, "}} else {{"),
                _ => writeln!(s, "}} else if tag$ == {v} {{"),
            };
            s += &indent(b);
        }
        s += "}\n";
        s
    }
    /// The finished value of member `m`: into `done$`, declared at the start, or its slot.
    fn finished(&self, m: usize, value: Option<String>, start: bool) -> String {
        match value {
            None => String::new(),
            Some(v) if self.multi() => format!("d{m}$ = Some({v})\n"),
            Some(v) if start => format!("let mut done$ = {v}\n"),
            Some(v) => format!("done$ = {v}\n"),
        }
    }
    /// Push a frame of resume point `r` holding `values`, and start its member.
    fn push(&self, w: &Gen<'a>, r: usize, values: Vec<String>, jump: &str) -> String {
        let c = w.c;
        let mut s = String::new();
        if !self.frame(r).is_empty() {
            let _ = writeln!(s, "{c}full(list.push(mut r{r}$, {}))?", tuple(&values));
        }
        if self.tagged() {
            let _ = writeln!(s, "{c}full(list.push(mut ts$, {r}u64))?");
        }
        if self.multi() {
            let _ = writeln!(s, "next$ = {}", self.step(r).edge.unwrap().1);
        }
        let _ = writeln!(s, "{jump}");
        s
    }
    /// The fewest bytes member `m` encodes to, for a count's check.
    fn least(&self, w: &mut Gen<'a>, m: usize) -> String {
        match self.g.least(m, 0) {
            Some(n) => n.to_string(),
            None => {
                let g: &'a Group<'a> = self.g;
                let o = w.of(&g.members[m].1);
                format!("{}least({o})", w.c)
            }
        }
    }
    /// The steps of member `m`'s variant `v` from `from`, values in `{pre}{k}$`, and the
    /// value they make; None when a step started another member unconditionally.
    fn seg(
        &self,
        w: &mut Gen<'a>,
        m: usize,
        v: usize,
        from: usize,
        pre: &str,
        jump: &str,
    ) -> (String, Option<String>) {
        let c = w.c;
        let shape = &self.g.shapes[m][v];
        let saved = |k: usize| {
            (0..k)
                .map(|j| format!("take {pre}{j}$"))
                .collect::<Vec<_>>()
        };
        let mut s = String::new();
        for (k, step) in shape.steps.iter().enumerate().skip(from) {
            let text = step.text;
            let Some((edge, child)) = step.edge else {
                let _ = writeln!(s, "let {pre}{k}$ = {}", w.take(step.ty));
                continue;
            };
            let r = self.resume_at(m, v, k);
            match edge {
                Edge::Direct => {
                    s += &self.push(w, r, saved(k), jump);
                    return (s, None);
                }
                Edge::Option => {
                    let push = self.push(w, r, saved(k), jump);
                    let _ = write!(
                        s,
                        "if {c}take_tag(b$, mut at$, 2)? != 0 {{\n{}}}\nlet {pre}{k}$: {text} = None\n",
                        indent(&push)
                    );
                }
                Edge::List => {
                    let least = self.least(w, child);
                    let mut values = saved(k);
                    values.extend([format!("take {pre}k{k}$"), format!("{pre}n{k}$")]);
                    let push = format!(
                        "let {pre}k{k}$: {text} = {c}room({pre}n{k}$)?\n{}",
                        self.push(w, r, values, jump)
                    );
                    let _ = write!(
                        s,
                        "let {pre}n{k}$ = {c}take_count(b$, mut at$, {least})?\nif {pre}n{k}$ > 0 {{\n{}}}\nlet {pre}{k}$: {text} = list.new()\n",
                        indent(&push)
                    );
                }
                Edge::Map(key) => {
                    let least = self.least(w, child);
                    let ck = w.of(key);
                    let mut values = saved(k);
                    values.extend([
                        format!("take {pre}m{k}$"),
                        format!("{pre}n{k}$"),
                        format!("take {pre}q{k}$"),
                    ]);
                    let push = format!(
                        "let {pre}q{k}$ = {c}take_with(b$, mut at$, {ck})?\n{}",
                        self.push(w, r, values, jump)
                    );
                    let _ = write!(
                        s,
                        "let {pre}n{k}$ = {c}take_count(b$, mut at$, {c}least({ck}) + {least})?\n\
                         let mut {pre}m{k}$: {text} = map.new()\n\
                         if let Error(f$) = map.reserve(mut {pre}m{k}$, {pre}n{k}$) {{\n    return Error(Full(f$))\n}}\n\
                         if {pre}n{k}$ > 0 {{\n{}}}\nlet {pre}{k}$ = take {pre}m{k}$\n",
                        indent(&push)
                    );
                }
            }
        }
        let args: Vec<_> = shape
            .steps
            .iter()
            .enumerate()
            .map(|(k, step)| match step.label {
                Some(l) => format!("{l}: take {pre}{k}$"),
                None => format!("take {pre}{k}$"),
            })
            .collect();
        let value = if !self.g.sum(m) || !args.is_empty() {
            format!("{}({})", shape.name, args.join(", "))
        } else {
            shape.name.to_owned()
        };
        (s, Some(value))
    }
    /// Resume point `r` with the value just finished: add it to the frame's list or map,
    /// or take it as the step's value; then the steps after it.
    fn resume(&self, w: &mut Gen<'a>, r: usize, finish: &str) -> String {
        let c = w.c;
        let (m, v, k) = self.resumes[r];
        let step = self.step(r);
        let (edge, child) = step.edge.unwrap();
        let mut s = String::new();
        let mut names: Vec<_> = (0..k).map(|j| format!("b{j}$")).collect();
        match edge {
            Edge::List => names.extend(["bk$".to_owned(), "bl$".to_owned()]),
            Edge::Map(_) => names.extend(["bm$".into(), "bl$".into(), "bq$".into()]),
            _ => {}
        }
        if !names.is_empty() {
            let none = if self.tagged() {
                "return Error(Bad(Invalid(at$)))"
            } else {
                finish
            };
            let _ = writeln!(
                s,
                "let {} = match list.pop(mut r{r}$) {{\n    Some(f$) => take f$,\n    None => {none}\n}}",
                tuple(&names)
            );
        }
        if self.multi() {
            let _ = writeln!(
                s,
                "let x$ = match replace(mut d{child}$, None) {{\n    Some(y$) => take y$,\n    None => return Error(Bad(Invalid(at$)))\n}}"
            );
        } else {
            s += "let x$ = take done$\n";
        }
        let saved = || (0..k).map(|j| format!("take b{j}$")).collect::<Vec<_>>();
        match edge {
            Edge::Direct => {
                let _ = writeln!(s, "let b{k}$ = take x$");
            }
            Edge::Option => {
                let _ = writeln!(s, "let b{k}$: {} = Some(take x$)", step.text);
            }
            Edge::List => {
                let mut values = saved();
                values.extend(["take bx$".to_owned(), "bl$ - 1".to_owned()]);
                let push = self.push(w, r, values, "break");
                let _ = write!(
                    s,
                    "let mut bx$ = take bk$\n{c}full(list.push(mut bx$, take x$))?\nif bl$ > 1 {{\n{}}}\nlet b{k}$ = take bx$\n",
                    indent(&push)
                );
            }
            Edge::Map(key) => {
                let ck = w.of(key);
                let mut values = saved();
                values.extend(["take bx$".into(), "bl$ - 1".into(), "take bn$".into()]);
                let push = format!(
                    "let key_at$ = at$\nlet bn$ = {c}take_with(b$, mut at$, {ck})?\n\
                     if map.contains(bx$, bn$) {{\n    return Error(Bad(Duplicate(key_at$)))\n}}\n{}",
                    self.push(w, r, values, "break")
                );
                let _ = write!(
                    s,
                    "let mut bx$ = take bm$\n{c}full(map.insert(mut bx$, take bq$, take x$))?\nif bl$ > 1 {{\n{}}}\nlet b{k}$ = take bx$\n",
                    indent(&push)
                );
            }
        }
        let (rest, value) = self.seg(w, m, v, k + 1, "b", "break");
        s + &rest + &self.finished(m, value, false)
    }
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
    /// The recursive group the type is a member of, and which member.
    group: Option<(&'a Group<'a>, usize)>,
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
            group: None,
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
                writeln!(text, "    let c${n}: {c}Codec[{t}] = {c}of()").unwrap();
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
            Ty::Scalar(x, _) => format!("{c}put_{x}({v}, mut out$)"),
            Ty::List(e) => format!("{c}put_list({v}, {}, mut out$)", self.of(e)),
            Ty::Option(e) => format!("{c}put_option({v}, {}, mut out$)", self.of(e)),
            Ty::Map(k, w) => format!("{c}put_map({v}, {}, {}, mut out$)", self.of(k), self.of(w)),
            Ty::Result(t, e) => format!(
                "{c}put_result({v}, {}, {}, mut out$)",
                self.of(t),
                self.of(e)
            ),
            Ty::With(t) => format!("{c}put_with({v}, {}, mut out$)", self.of(t)),
        }
    }
    fn take(&mut self, ty: &Ty<'a>) -> String {
        let c = self.c;
        match *ty {
            Ty::Scalar(x, _) => format!("{c}take_{x}(b$, mut at$)?"),
            Ty::List(e) => format!("{c}take_list(b$, mut at$, {})?", self.of(e)),
            Ty::Option(e) => format!("{c}take_option(b$, mut at$, {})?", self.of(e)),
            Ty::Map(k, w) => format!("{c}take_map(b$, mut at$, {}, {})?", self.of(k), self.of(w)),
            Ty::Result(t, e) => format!(
                "{c}take_result(b$, mut at$, {}, {})?",
                self.of(t),
                self.of(e)
            ),
            Ty::With(t) => format!("{c}take_with(b$, mut at$, {})?", self.of(t)),
        }
    }
    fn schema(&mut self, ty: &Ty<'a>) -> String {
        let c = self.c;
        match *ty {
            Ty::Scalar(x, _) => format!("    {c}schema_{x}(mut s$)?\n"),
            Ty::List(e) => format!("    {c}schema_list(mut s$, {})?\n", self.of(e)),
            Ty::Option(e) => format!("    {c}schema_option(mut s$, {})?\n", self.of(e)),
            Ty::Map(k, w) => format!(
                "    {c}schema_map(mut s$, {}, {})?\n",
                self.of(k),
                self.of(w)
            ),
            Ty::Result(t, e) => format!(
                "    {c}schema_result(mut s$, {}, {})?\n",
                self.of(t),
                self.of(e)
            ),
            Ty::With(t) => format!("    {c}schema_of(mut s$, {})?\n", self.of(t)),
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
                    calls: vec![format!("{}least({c})", self.c)],
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
        let body = match (&decl.via, &decl.body) {
            (Some(v), _) => {
                let r = self.of(v.wire);
                format!("    {c}put_with({}(x$), {r}, mut out$)\n", v.to)
            }
            (None, Body::Record(entries)) => self.record_encode(entries),
            (None, Body::Sum(variants)) => self.sum_encode(variants),
        };
        let header = format!(
            "fn {name}$encode{generics}(x$: {ty}, mut out$: Bytes) -> Result[(), Capacity]"
        );
        self.function(out, &header, &body);
        let body = match (&decl.via, self.group, &decl.body) {
            (Some(v), ..) => {
                let r = self.of(v.wire);
                format!("    {c}take_via(b$, mut at$, {r}, {})\n", v.from)
            }
            (None, Some((g, at)), _) => Machine::new(g, at).decode(&mut self),
            (None, None, Body::Record(entries)) => self.record_decode(entries),
            (None, None, Body::Sum(variants)) => self.sum_decode(variants),
        };
        let header =
            format!("fn {name}$decode{generics}(b$: Bytes, mut at$: u64) -> Result[{ty}, {c}Stop]");
        self.function(out, &header, &body);
        let body = self.schema_body();
        let header = format!(
            "fn {name}$schema{generics}(mut s$: {c}Schema, c$: {c}Codec[{ty}]) -> Result[(), Capacity]"
        );
        self.function(out, &header, &body);
        let (lets, least) = self.least_body();
        let header = format!("fn {name}$codec{generics}() -> {c}Codec[{ty}]");
        let body =
            format!("{lets}    {c}make({name}$encode, {name}$decode, {name}$schema, {least})\n");
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
            let mut steps = vec![format!("{c}put_tag({tag}, mut out$)")];
            let binds: Vec<_> = v
                .payload
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    steps.push(self.put(&p.ty, &format!("p${i}")));
                    p.label.map_or(format!("p${i}"), |l| format!("{l}: p${i}"))
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
            "    let tag$ = {c}take_tag(b$, mut at$, {})?",
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
                    .map(|p| {
                        let take = self.take(&p.ty);
                        p.label.map_or(take.clone(), |l| format!("{l}: {take}"))
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
            "    {c}define(mut s$, \"{}\", {})?\n",
            self.path,
            self.decl.generics.len()
        );
        for (p, _) in &decl.generics {
            let a = self.of(p);
            let _ = writeln!(s, "    {c}argument(mut s$, {a})?");
        }
        if let Some(v) = &decl.via {
            let r = self.of(v.wire);
            let _ = writeln!(
                s,
                "    if {c}via(mut s$)? {{\n        return Ok(())\n    }}\n    {c}schema_of(mut s$, {r})?"
            );
            let _ = write!(s, "    {c}close(mut s$)\n    Ok(())\n");
            return s;
        }
        let (kind, n) = match &decl.body {
            Body::Record(entries) => ("record", entries.len()),
            Body::Sum(variants) => ("sum", variants.len()),
        };
        let _ = writeln!(
            s,
            "    if {c}{kind}(mut s$, {n})? {{\n        return Ok(())\n    }}"
        );
        match &decl.body {
            Body::Record(entries) => {
                for e in entries {
                    match (e.removed, e.default) {
                        (true, _) => {
                            let _ = writeln!(s, "    {c}removed_field(mut s$, \"{}\")?", e.name);
                            s += &self.schema(&e.ty);
                        }
                        (false, Some(v)) => {
                            let f = self.of(e.text);
                            let _ = writeln!(
                                s,
                                "    {c}field_default(mut s$, \"{}\", {f}, {v})?",
                                e.name
                            );
                        }
                        (false, None) => {
                            let _ = writeln!(s, "    {c}field(mut s$, \"{}\")?", e.name);
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
                        "    {c}{call}(mut s$, \"{}\", {})?",
                        v.name,
                        v.payload.len()
                    );
                    for p in &v.payload {
                        s += &self.schema(&p.ty);
                    }
                }
            }
        }
        let _ = write!(s, "    {c}close(mut s$)\n    Ok(())\n");
        s
    }
    /// The maker's LEAST: the sum over a record's live fields; for a sum, 4 plus the least
    /// over its live variants, a running minimum in lets when a payload's is a call.
    fn least_body(&mut self) -> (String, String) {
        let decl = self.decl;
        if let Some(v) = &decl.via {
            let r = self.of(v.wire);
            return (String::new(), format!("{}least({r})", self.c));
        }
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
                        .fold(Least::fixed(0), |l, p| l.plus(self.least(&p.ty)));
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
