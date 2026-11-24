//! hirsel-lean: extract Hirsel functions to Lean 4.
//!
//! The direct-style syntax tree gives the shape of each body (statements, `let mut`, `?`,
//! loops, early returns). The checked CoreIR of the same function gives what the syntax
//! alone cannot: which definition or builtin each call names, each parameter's loan, the
//! width of each integer literal, whether an arithmetic step can fail, and what a body drops
//! and which runtime operations it reaches. The two are joined by exact item-relative source
//! span (`origin.span` of a syntax node == `inst_anchors[i]` of an IR instruction).
//!
//! The extractor refuses, rather than approximates, anything it cannot model: an unjoined call,
//! an indirect call, a runtime operation that is not a pure byte comparison (host operations
//! and cancellation checks), a drop whose type has a finalizer or holds a resource, and every
//! syntax form it has no rule for.
use crate::{CheckProduct, CheckRequest, Driver, LibraryInput, Meter};
use hirsel_core_ids::DefPath;
use hirsel_core_ir::{Body as IrBody, Callee, Inst, Loan, Module, Param as IrParam, RtOp};
use hirsel_core_layout::Body as ShapeBody;
use hirsel_core_syntax::{Item, NodeId, NodeKind as K};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;

type R<T> = Result<T, String>;
macro_rules! bail {
    ($($t:tt)*) => { return Err(format!($($t)*)) };
}

mod bindings;
mod checks;
mod coreir;
mod expr;
mod stmt;
mod syntax;

use crate::cli::{Invocation, OptionId};
use bindings::{BUILTINS, PRELUDE_CTORS, PRELUDE_FNS, PreludeFn};
use checks::{check_body, exclusions};
use syntax::*;

fn check(inputs: &Path, store: &Path) -> R<Arc<CheckProduct>> {
    let manifest = inputs
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let inputs = serde_json::from_slice(&std::fs::read(inputs).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let product = Driver::default()
        .check(
            CheckRequest {
                manifest: Some(manifest.to_owned()),
                inputs,
                store: store.to_owned(),
                library: LibraryInput::Sources,
                entry: None,
            },
            &mut Meter::new(false),
        )
        .map_err(|e| e.to_string())?;
    if !product.diagnostics.is_empty() {
        bail!(
            "the package does not check: {} refusals, first {:?}",
            product.diagnostics.len(),
            product.diagnostics[0]
        );
    }
    Ok(product)
}

/// The syntax item a definition was checked from, and its file's path.
fn item_of<'a>(product: &'a CheckProduct, path: &DefPath) -> Option<(Item<'a>, usize)> {
    let (_, def) = product
        .index
        .definitions()
        .find(|(id, _)| product.index.definition_path(*id) == *path)?;
    let (fi, file) = product
        .files
        .iter()
        .enumerate()
        .find(|(_, f)| Some(f.file) == def.site.source)?;
    Some((file.syntax.item(def.site.item as usize), fi))
}

// ---------------------------------------------------------------------------------------------
// Lean names.

const LEAN_KEYWORDS: &[&str] = &[
    "at",
    "by",
    "do",
    "else",
    "end",
    "for",
    "from",
    "fun",
    "have",
    "if",
    "import",
    "in",
    "let",
    "match",
    "mut",
    "namespace",
    "open",
    "return",
    "section",
    "show",
    "then",
    "theorem",
    "try",
    "catch",
    "finally",
    "unless",
    "where",
    "with",
    "deriving",
    "instance",
    "structure",
    "inductive",
    "def",
    "abbrev",
    "class",
    "example",
    "variable",
    "universe",
    "break",
    "continue",
    "Type",
    "Prop",
    "Sort",
    "this",
    "calc",
    "suffices",
    "obtain",
    "nomatch",
];

fn mangle(name: &str) -> String {
    if LEAN_KEYWORDS.contains(&name) || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        format!("«{name}»")
    } else {
        name.to_owned()
    }
}

/// Wrap an application (anything with a top-level space) in parentheses.
fn atom(s: String) -> String {
    let wrapped = (s.starts_with('(') && s.ends_with(')') && balanced_inside(&s))
        || (s.starts_with('{') && s.ends_with('}') && balanced_inside(&s))
        || (s.starts_with('[') && s.ends_with(']') && balanced_inside(&s));
    if wrapped || !s.contains(' ') {
        s
    } else {
        format!("({s})")
    }
}

/// Whether the outer bracket pair of `s` encloses all of it.
fn balanced_inside(s: &str) -> bool {
    let mut depth = 0i32;
    let chars: Vec<char> = s.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        match c {
            '(' | '{' | '[' => depth += 1,
            ')' | '}' | ']' => {
                depth -= 1;
                if depth == 0 && i + 1 != chars.len() {
                    return false;
                }
            }
            _ => {}
        }
    }
    true
}

/// Runtime operations an extracted body may reach: pure, total, no host effect.
fn pure_runtime(op: &RtOp) -> bool {
    matches!(op, RtOp::CompareBytes)
}

// ---------------------------------------------------------------------------------------------
// The span join for one function body.

#[derive(Default)]
struct JoinStats {
    calls_joined: usize,
    builtins_joined: usize,
    helpers: usize,
    ctors: usize,
    compares_joined: usize,
    literals_joined: usize,
    literals_unanchored: usize,
    params_checked: usize,
    unresolved_syntax: Vec<String>,
    unresolved_ir: Vec<String>,
    refused: Vec<String>,
    drops_checked: usize,
    /// Instruction kind -> (count, how many can fail).
    kinds: BTreeMap<String, (usize, usize)>,
}

enum Resolved {
    Ctor(String, Option<usize>),
    Extracted(String),
    Prelude(&'static PreludeFn),
    Builtin(&'static str),
}

struct Join<'a> {
    ir: &'a Module,
    body: &'a IrBody,
    by_span: BTreeMap<(u32, u32), Vec<usize>>,
}

impl<'a> Join<'a> {
    fn new(ir: &'a Module, body: &'a IrBody) -> Self {
        let mut by_span: BTreeMap<(u32, u32), Vec<usize>> = BTreeMap::new();
        for (i, a) in body.inst_anchors.iter().enumerate() {
            if let Some(a) = a {
                by_span.entry((a.start, a.end)).or_default().push(i);
            }
        }
        Join { ir, body, by_span }
    }
    fn at(&self, span: (u32, u32)) -> impl Iterator<Item = &'a Inst> + '_ {
        self.by_span
            .get(&span)
            .into_iter()
            .flatten()
            .map(|&i| &self.body.insts[i])
    }
    fn direct_path(&self, inst: &Inst) -> Option<String> {
        if let Inst::Call { call } = inst {
            if let Callee::Direct(f) = self.body.calls[call.index()].callee {
                let key = &self.ir.instances[self.ir.functions[f.index()].instance.index()];
                return Some(key.definition.to_string());
            }
        }
        None
    }
}

// ---------------------------------------------------------------------------------------------
// Emission.

struct Fx<'a, 'm> {
    m: &'m ModuleInfo<'a>,
    f: &'m FnInfo<'a>,
    s: Src<'a>,
    join: Join<'m>,
    stats: JoinStats,
    has_causes: bool,

    /// Locals bound with `let mut` (and `mut` value parameters).
    muts: BTreeSet<String>,

    joined_ir: BTreeSet<(u32, u32)>,
    joined_failures: BTreeSet<(u32, u32)>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tail {
    /// The value of a `Result`-returning function.
    Result,
    /// The value of a pure function in `Id.run do`.
    Pure,
    /// A block whose value is `()`.
    Unit,
}

fn pad(n: usize) -> String {
    " ".repeat(n)
}

// ---------------------------------------------------------------------------------------------
// The extraction.

struct Extraction {
    lean: String,
    report: String,
    ok: bool,
}

fn extract(product: &CheckProduct, prefix: &str, names: &[String]) -> R<Extraction> {
    // Locate the file and every function item.
    let mut file_index = None;
    let mut items = Vec::new();
    for name in names {
        let path = DefPath::from_full(&format!("{prefix}/{name}"))
            .ok_or_else(|| format!("{name}: bad path"))?;
        let (item, fi) =
            item_of(product, &path).ok_or_else(|| format!("{name}: no syntax item"))?;
        if *file_index.get_or_insert(fi) != fi {
            bail!("{name}: not in the same file as the others");
        }
        items.push((name.clone(), path, item));
    }
    let file = &product.files[file_index.ok_or("no functions named")?];
    let mut m = ModuleInfo {
        prefix: prefix.to_owned(),
        types: BTreeMap::new(),
        fns: BTreeMap::new(),
        ctors: BTreeMap::new(),
        tables: None,
    };
    for (i, item) in file.syntax.items().enumerate() {
        let src = Src { item };
        if src.kind(item.root) == K::TypeDecl {
            let t = type_info(src, item.root, i)?;
            m.types.insert(t.name.clone(), t);
        }
    }
    for (name, path, item) in &items {
        let src = Src { item: *item };
        if src.kind(item.root) != K::Function {
            bail!("{name}: not a function");
        }
        let order = file
            .syntax
            .items
            .iter()
            .position(|r| r.root == item.root)
            .unwrap();
        let f = fn_info(src, item.root, path.clone(), order)?;
        m.fns.insert(name.clone(), f);
    }
    // Constructors: the module's own, then the prelude's; a name may name one constructor.
    let mut ctors: BTreeMap<String, Vec<(String, Option<usize>, String)>> = BTreeMap::new();
    for t in m.types.values() {
        match &t.body {
            TypeBody::Record(_) => ctors.entry(t.name.clone()).or_default().push((
                t.name.clone(),
                None,
                t.name.clone(),
            )),
            TypeBody::Variant(cs) => {
                for (c, ps) in cs {
                    ctors.entry(c.clone()).or_default().push((
                        format!("{}.{c}", t.name),
                        Some(ps.len()),
                        t.name.clone(),
                    ));
                }
            }
        }
    }
    for (c, lean, k) in PRELUDE_CTORS {
        ctors
            .entry((*c).into())
            .or_default()
            .push(((*lean).into(), Some(*k), String::new()));
    }
    for (c, v) in ctors {
        match v.as_slice() {
            [one] => {
                m.ctors.insert(c, one.clone());
            }
            _ => {
                // Ambiguous names stay unbound; a use of one is refused as unresolved.
            }
        }
    }
    let tables: Vec<String> = m
        .types
        .values()
        .filter(|t| m.is_tables(t))
        .map(|t| t.name.clone())
        .collect();
    if m.fns.values().any(|f| f.prog == Some(State::Tx)) {
        let [t] = tables.as_slice() else {
            bail!("expected exactly one tables record, found {tables:?}")
        };
        m.tables = Some(t.clone());
    }

    // Which types to emit: those the functions name, and what those name.
    let mut need = BTreeSet::new();
    for f in m.fns.values() {
        f.src.walk(f.root, &mut |n| {
            let k = f.src.kind(n);
            if k == K::TypePath && m.types.contains_key(f.src.text(n)) {
                need.insert(f.src.text(n).to_owned());
            }
            if matches!(k, K::Path) {
                let t = f.src.text(n);
                if let Some((_, _, owner)) = m.ctors.get(t) {
                    if !owner.is_empty() {
                        need.insert(owner.clone());
                    }
                }
            }
        });
    }
    if let Some(t) = &m.tables {
        need.insert(t.clone());
    }
    let mut ordered: Vec<String> = Vec::new();
    fn visit(
        m: &ModuleInfo,
        t: &str,
        ordered: &mut Vec<String>,
        on: &mut BTreeSet<String>,
    ) -> R<()> {
        if ordered.iter().any(|x| x == t) {
            return Ok(());
        }
        if !on.insert(t.to_owned()) {
            bail!("recursive type {t}");
        }
        let ti = &m.types[t];
        let mut deps = BTreeSet::new();
        match &ti.body {
            TypeBody::Record(fs) => fs
                .iter()
                .for_each(|(_, n)| m.type_refs(ti.src, *n, &mut deps)),
            TypeBody::Variant(cs) => cs
                .iter()
                .flat_map(|(_, ps)| ps)
                .for_each(|n| m.type_refs(ti.src, *n, &mut deps)),
        }
        let mut deps: Vec<&TypeInfo> = deps
            .iter()
            .filter(|d| *d != t)
            .map(|d| &m.types[d])
            .collect();
        deps.sort_by_key(|d| d.order);
        for d in deps {
            visit(m, &d.name, ordered, on)?;
        }
        ordered.push(t.to_owned());
        Ok(())
    }
    let mut roots: Vec<&TypeInfo> = need.iter().map(|t| &m.types[t]).collect();
    roots.sort_by_key(|t| t.order);
    let mut on = BTreeSet::new();
    for t in roots {
        visit(&m, &t.name, &mut ordered, &mut on)?;
    }

    let mut out = String::new();
    let mut report = String::new();
    writeln!(
        out,
        "-- Generated by hirsel lean extract from {prefix}. Do not edit."
    )
    .unwrap();
    writeln!(out, "import Hirsel\nimport Store\n").unwrap();
    writeln!(out, "namespace {}\n", lean_namespace(prefix)).unwrap();
    // Shapes in the IR, for the type cross-check.
    let mut shapes: BTreeMap<String, Arc<hirsel_core_layout::Shape>> = BTreeMap::new();
    for f in m.fns.values() {
        let def = product
            .definitions
            .get(&f.path)
            .ok_or_else(|| format!("{}: not checked", f.name))?;
        if let Some(gap) = def.incomplete.first() {
            bail!("{}: incomplete checked body: {gap:?}", f.name);
        }
        let ir = def.ir().map_err(|e| e.to_string())?;
        for row in &ir.types {
            shapes
                .entry(row.name.clone())
                .or_insert_with(|| row.clone());
        }
    }
    let mut shapes_checked = 0;
    let mut no_shape: Vec<String> = Vec::new();
    for t in &ordered {
        let ti = &m.types[t];
        emit_type(&m, ti, &mut out)?;
        // A generic type's shape is named by its parameters, `Entry[$0]`.
        let params: Vec<String> = (0..ti.generics.len()).map(|i| format!("${i}")).collect();
        let key = if params.is_empty() {
            format!("{prefix}/{t}")
        } else {
            format!("{prefix}/{t}[{}]", params.join(", "))
        };
        if let Some(shape) = shapes.get(&key) {
            let ok = match (&ti.body, &shape.body) {
                (TypeBody::Record(fs), ShapeBody::Record(gs)) => {
                    fs.len() == gs.len() && fs.iter().zip(gs).all(|((a, _), b)| *a == b.name)
                }
                (TypeBody::Variant(cs), ShapeBody::Variant(ds)) => {
                    cs.len() == ds.len()
                        && cs
                            .iter()
                            .zip(ds)
                            .all(|((a, ps), b)| *a == b.name && ps.len() == b.fields.len())
                }
                _ => false,
            };
            if !ok {
                bail!(
                    "type {t}: its fields or cases differ from the IR shape {}",
                    shape.name
                );
            }
            shapes_checked += 1;
        } else {
            no_shape.push(t.to_string());
        }
    }

    let mut fns: Vec<&FnInfo> = m.fns.values().collect();
    fns.sort_by_key(|f| f.order);
    // Callees first; otherwise source order.
    let mut done: Vec<String> = Vec::new();
    let mut total = JoinStats::default();
    let mut emitted = Vec::new();
    let mut pending = fns.clone();
    while !pending.is_empty() {
        let before = pending.len();
        let mut i = 0;
        while i < pending.len() {
            let f = pending[i];
            let callees = callees_of(&m, f);
            if callees.iter().all(|c| done.contains(c)) {
                emitted.push(f);
                done.push(f.name.clone());
                pending.remove(i);
                i = 0;
                continue;
            }
            i += 1;
        }
        if pending.len() == before {
            bail!(
                "recursive functions: {:?}",
                pending.iter().map(|f| &f.name).collect::<Vec<_>>()
            );
        }
    }
    for f in emitted {
        let def = &product.definitions[&f.path];
        let ir = def.ir().map_err(|e| e.to_string())?;
        let func = ir
            .functions
            .iter()
            .find(|g| ir.instances[g.instance.index()].definition == f.path && g.body.is_some())
            .ok_or_else(|| format!("{}: no IR body", f.name))?;
        let body = func.body.as_ref().unwrap();
        let sig = &ir.signatures[func.signature.index()];
        let mut fx = Fx {
            m: &m,
            f,
            s: f.src,
            join: Join::new(ir, body),
            stats: JoinStats::default(),
            has_causes: body.insts.iter().any(|inst| {
                matches!(inst, Inst::Binary { .. } | Inst::Address { .. }) && inst.can_fail(body)
            }),
            muts: BTreeSet::new(),
            joined_ir: BTreeSet::new(),
            joined_failures: BTreeSet::new(),
        };
        check_body(f, ir, body, sig, &mut fx.stats)?;
        emit_fn(&mut fx, &mut out)?;
        checks::verify_joins(&mut fx);
        let st = fx.stats;
        writeln!(
            report,
            "{:<18} calls {:>2}  builtins {:>2}  helpers {:>2}  ctors {:>2}  compares {:>2}  literals {:>2}/{:<2} params {}  drops {:>2}  unresolved {}/{}  refused {}",
            f.name, st.calls_joined, st.builtins_joined, st.helpers, st.ctors, st.compares_joined, st.literals_joined,
            st.literals_joined + st.literals_unanchored, st.params_checked, st.drops_checked,
            st.unresolved_syntax.len(), st.unresolved_ir.len(), st.refused.len()
        )
        .unwrap();
        for x in st
            .unresolved_syntax
            .iter()
            .chain(&st.unresolved_ir)
            .chain(&st.refused)
        {
            writeln!(report, "  ! {x}").unwrap();
        }
        total.calls_joined += st.calls_joined;
        total.builtins_joined += st.builtins_joined;
        total.unresolved_syntax.extend(st.unresolved_syntax);
        total.unresolved_ir.extend(st.unresolved_ir);
        total.refused.extend(st.refused);
        total.literals_joined += st.literals_joined;
        total.drops_checked += st.drops_checked;
        for (k, (n, c)) in st.kinds {
            let e = total.kinds.entry(k).or_default();
            e.0 += n;
            e.1 += c;
        }
    }
    writeln!(out, "end {}", lean_namespace(prefix)).unwrap();
    let ok = total.unresolved_syntax.is_empty()
        && total.unresolved_ir.is_empty()
        && total.refused.is_empty();
    writeln!(
        report,
        "TOTAL functions {}  types {} (IR shapes checked {}, no IR shape: {})  calls joined {}  builtins joined {}  unresolved syntax->IR {}  unresolved IR->syntax {}  refused (host op / cancel / finalizer / indirect) {}  drops checked {}",
        m.fns.len(), ordered.len(), shapes_checked, if no_shape.is_empty() { "none".to_string() } else { no_shape.join(" ") }, total.calls_joined, total.builtins_joined,
        total.unresolved_syntax.len(), total.unresolved_ir.len(), total.refused.len(), total.drops_checked
    )
    .unwrap();
    let census: Vec<String> = total
        .kinds
        .iter()
        .map(|(k, (n, c))| format!("{k} {n}/{c}"))
        .collect();
    writeln!(
        report,
        "IR census (kind count/can_fail): {}",
        census.join(", ")
    )
    .unwrap();
    Ok(Extraction {
        lean: out,
        report,
        ok,
    })
}

/// `runner@1/triggers` -> `runner.triggers`.
fn lean_namespace(prefix: &str) -> String {
    prefix
        .split('/')
        .map(|seg| mangle(seg.split('@').next().unwrap()))
        .collect::<Vec<_>>()
        .join(".")
}

fn callees_of(m: &ModuleInfo, f: &FnInfo) -> Vec<String> {
    let mut out = Vec::new();
    f.src.walk(f.root, &mut |n| {
        if f.src.kind(n) == K::Call {
            let c = f.src.text(f.src.kids(n)[0]);
            if m.fns.contains_key(c) && !out.iter().any(|x| x == c) {
                out.push(c.to_owned());
            }
        }
    });
    out
}

fn type_arguments(generics: &[String]) -> String {
    let mut out = String::new();
    for generic in generics {
        out.push(' ');
        out.push_str(generic);
    }
    out
}

fn emit_type(m: &ModuleInfo, t: &TypeInfo, out: &mut String) -> R<()> {
    let src = t.src;
    let name_end = {
        let tok = src.item.tree.node(t.root).token.unwrap();
        let mut end = src.item.token_span(tok).end;
        for &c in src.kids(t.root) {
            if src.kind(c) == K::Generics {
                end = src.span(c).1;
            }
        }
        end
    };
    let header = src.header(t.root, name_end);
    let params: String = t
        .generics
        .iter()
        .map(|g| format!(" ({g} : Type)"))
        .collect();
    let derive = m.type_plain(t, &mut BTreeSet::new());
    if m.is_tables(t) {
        let TypeBody::Record(fs) = &t.body else {
            unreachable!()
        };
        writeln!(out, "/-- The contents of the tables `{}` names. -/", t.name).unwrap();
        writeln!(out, "structure {}.Data where", t.name).unwrap();
        for (f, n) in fs {
            let text = src.text(*n);
            if text.starts_with("store.Table[") {
                let k = src.kids(*n);
                let (key, val) = (m.ty(src, k[1], &t.generics)?, m.ty(src, k[2], &t.generics)?);
                writeln!(
                    out,
                    "  {} : Hirsel.Map {} {}",
                    mangle(f),
                    atom(key),
                    atom(val)
                )
                .unwrap();
            }
        }
        writeln!(out).unwrap();
        writeln!(out, "-- `{header}`\nstructure {} where", t.name).unwrap();
        for (f, n) in fs {
            writeln!(out, "  {} : {}", mangle(f), m.ty(src, *n, &t.generics)?).unwrap();
        }
        writeln!(out).unwrap();
        writeln!(
            out,
            "/-- The handle whose tables are the fields of `{}.Data`. -/",
            t.name
        )
        .unwrap();
        writeln!(out, "def {}.handle : {} where", t.name, t.name).unwrap();
        for (f, n) in fs {
            let f = mangle(f);
            if src.text(*n).starts_with("store.Table[") {
                writeln!(
                    out,
                    "  {f} := ⟨fun d => d.{f}, fun d m => {{ d with {f} := m }}⟩"
                )
                .unwrap();
            } else {
                writeln!(out, "  {f} := ⟨⟩").unwrap();
            }
        }
        writeln!(out).unwrap();
        return Ok(());
    }
    writeln!(out, "-- `{header}`").unwrap();
    match &t.body {
        TypeBody::Record(fs) => {
            let mut deps = BTreeSet::new();
            for (_, ty) in fs {
                m.type_refs(src, *ty, &mut deps);
            }
            if deps.contains(&t.name) {
                let applied = format!("{}{}", t.name, type_arguments(&t.generics));
                writeln!(out, "inductive {}{params} where", t.name).unwrap();
                write!(out, "  | mk").unwrap();
                for (f, n) in fs {
                    write!(out, " ({} : {})", mangle(f), m.ty(src, *n, &t.generics)?).unwrap();
                }
                writeln!(out, " : {applied}").unwrap();
                let implicit: String = t
                    .generics
                    .iter()
                    .map(|g| format!(" {{{g} : Type}}"))
                    .collect();
                for (i, (f, n)) in fs.iter().enumerate() {
                    let pat: Vec<_> = (0..fs.len())
                        .map(|j| if i == j { "value" } else { "_" })
                        .collect();
                    writeln!(
                        out,
                        "def {}.{}{implicit} : {applied} → {}\n  | .mk {} => value",
                        t.name,
                        mangle(f),
                        m.ty(src, *n, &t.generics)?,
                        pat.join(" ")
                    )
                    .unwrap();
                }
            } else {
                writeln!(out, "structure {}{params} where", t.name).unwrap();
                for (f, n) in fs {
                    writeln!(out, "  {} : {}", mangle(f), m.ty(src, *n, &t.generics)?).unwrap();
                }
            }
        }
        TypeBody::Variant(cs) => {
            writeln!(out, "inductive {}{params} where", t.name).unwrap();
            for (c, ps) in cs {
                let mut parts: Vec<String> = ps
                    .iter()
                    .map(|n| m.ty(src, *n, &t.generics).map(atom))
                    .collect::<R<_>>()?;
                parts.push(format!("{}{}", t.name, type_arguments(&t.generics)));
                writeln!(out, "  | {} : {}", mangle(c), parts.join(" → ")).unwrap();
            }
        }
    }
    if derive {
        writeln!(out, "  deriving DecidableEq, Repr").unwrap();
    }
    writeln!(out).unwrap();
    Ok(())
}

fn emit_fn(fx: &mut Fx, out: &mut String) -> R<()> {
    let f = fx.f;
    let s = f.src;
    let block = *s
        .kids(f.root)
        .iter()
        .find(|&&c| s.kind(c) == K::Block)
        .ok_or("no body")?;
    let header = s.header(f.root, s.span(block).0);
    writeln!(out, "-- `{header}`").unwrap();
    let mut sig = format!("def {}", mangle(&f.name));
    for g in &f.generics {
        write!(sig, " {{{g} : Type}}").unwrap();
    }
    if f.prog == Some(State::Generic) {
        sig.push_str(" {S : Type}");
    }
    for p in &f.params {
        if p.tx || (p.mode == Mode::Mut && f.prog == Some(State::Param(p.name.clone()))) {
            continue;
        }
        if p.mode == Mode::Mut {
            fx.muts.insert(p.name.clone());
        }
        write!(
            sig,
            " ({} : {})",
            mangle(&p.name),
            fx.m.ty(s, p.ty, &f.generics)?
        )
        .unwrap();
    }
    let result = match f.result {
        Some(r) => r,
        None => bail!("{}: no result type", f.name),
    };
    match &f.prog {
        Some(state) => {
            let k = s.kids(result);
            let (t, e) = (
                fx.m.ty(s, k[1], &f.generics)?,
                fx.m.ty(s, k[2], &f.generics)?,
            );
            let st = match state {
                State::Tx => format!("(Hirsel.store.Tx {}.Data)", fx.m.tables.as_ref().unwrap()),
                State::Param(p) => {
                    let pi = f.params.iter().find(|q| &q.name == p).unwrap();
                    atom(fx.m.ty(s, pi.ty, &f.generics)?)
                }
                State::Generic => "S".into(),
            };
            writeln!(
                out,
                "{sig} : Hirsel.Prog {st} {} {} := do",
                atom(e),
                atom(t)
            )
            .unwrap();
            fx.block(out, block, 2, Tail::Result)?;
        }
        None => {
            let t = fx.m.ty(s, result, &f.generics)?;
            if fx.has_causes {
                writeln!(out, "{sig} : Except Hirsel.Cause {t} := do").unwrap();
                fx.block(out, block, 2, Tail::Pure)?;
                writeln!(out).unwrap();
                return Ok(());
            }
            let kids = s.kids(block);
            let mut has_return = false;
            s.walk(block, &mut |n| has_return |= s.kind(n) == K::Return);
            if let ([e], false) = (kids, has_return) {
                let e = s.kid(*e, 0)?;
                writeln!(out, "{sig} : {t} :=").unwrap();
                let v = if s.kind(e) == K::Match {
                    fx.match_expr(e, 2)?
                } else {
                    fx.expr(e)?
                };
                writeln!(out, "  {v}").unwrap();
            } else {
                writeln!(out, "{sig} : {t} := _root_.Id.run do").unwrap();
                fx.block(out, block, 2, Tail::Pure)?;
            }
        }
    }
    writeln!(out).unwrap();
    Ok(())
}

// ---------------------------------------------------------------------------------------------

fn probe(product: &CheckProduct, path: &DefPath) -> R<()> {
    let def = product
        .definitions
        .get(path)
        .ok_or_else(|| format!("{path}: not a checked function"))?;
    let ir = def.ir().map_err(|e| e.to_string())?;
    let func = ir
        .functions
        .iter()
        .find(|g| ir.instances[g.instance.index()].definition == *path && g.body.is_some())
        .ok_or_else(|| format!("{path}: no IR body"))?;
    let mut stats = JoinStats::default();
    exclusions(
        &path.to_string(),
        ir,
        func.body.as_ref().unwrap(),
        &mut stats,
    );
    let mut reasons: BTreeMap<String, usize> = BTreeMap::new();
    for r in &stats.refused {
        let reason = r.split_once(": ").map(|x| x.1).unwrap_or(r);
        let reason = reason.rsplit_once(" at ").map(|x| x.0).unwrap_or(reason);
        *reasons.entry(reason.to_owned()).or_default() += 1;
    }
    println!("{path}: {} refusals", stats.refused.len());
    for (r, n) in reasons {
        println!("  {n} x {r}");
    }
    Ok(())
}

pub(crate) fn run(invocation: Invocation) -> R<()> {
    let mut inputs = None;
    let mut store = None;
    let mut output = None;
    let mut target = None;
    let mut coreir = false;
    for (option, value) in invocation.options {
        let slot = match option {
            OptionId::Inputs => &mut inputs,
            OptionId::Store => &mut store,
            OptionId::Output => &mut output,
            OptionId::Target => &mut target,
            OptionId::CoreIr if !coreir => {
                coreir = true;
                continue;
            }
            _ => bail!("invalid option for {}", invocation.command.name),
        };
        if slot.is_some() {
            bail!("option {option:?} must be given only once");
        }
        *slot = value;
    }
    let inputs = inputs.as_deref().ok_or("--inputs is required")?;
    let store = store.as_deref().ok_or("--store is required")?;
    let module = invocation
        .positional
        .as_deref()
        .ok_or("a module is required")?;
    if target.is_some() && !coreir {
        bail!("--target requires --coreir");
    }
    let product = check(Path::new(inputs), Path::new(store))?;
    if invocation.command.name == "lean probe" {
        return probe(
            &product,
            &DefPath::from_full(module).ok_or("invalid definition path")?,
        );
    }
    let output = Path::new(output.as_deref().ok_or("--output is required")?);
    if coreir {
        let target = target.as_deref().unwrap_or(crate::native::TARGET);
        let target = hirsel_core_layout::Target::from_triple(target)
            .ok_or_else(|| format!("unsupported target `{target}`"))?;
        let snapshot = coreir::capture(&product, module, &invocation.arguments, target)?;
        return write_output(output, |file| {
            snapshot.write(file).map_err(|error| error.to_string())
        });
    }
    let extracted = extract(&product, module, &invocation.arguments)?;
    print!("{}", extracted.report);
    if !extracted.ok {
        bail!("extraction refused: unresolved calls or excluded operations");
    }
    if invocation.command.name == "lean status" {
        match std::fs::read(output) {
            Ok(bytes) if bytes == extracted.lean.as_bytes() => println!("current extraction"),
            Ok(_) => println!("stale"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => println!("stale"),
            Err(error) => bail!("{}: {error}", output.display()),
        }
    } else {
        write_output(output, |file| {
            std::io::Write::write_all(file, extracted.lean.as_bytes())
                .map_err(|error| error.to_string())
        })?;
    }
    Ok(())
}

fn write_output(
    output: &Path,
    write: impl FnOnce(&mut std::io::BufWriter<&mut std::fs::File>) -> R<()>,
) -> R<()> {
    use std::io::Write;
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|error| error.to_string())?;
    {
        let mut buffer = std::io::BufWriter::new(file.as_file_mut());
        write(&mut buffer)?;
        buffer.flush().map_err(|error| error.to_string())?;
    }
    file.persist(output).map_err(|error| error.to_string())?;
    Ok(())
}
