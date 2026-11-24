use super::*;

// ---------------------------------------------------------------------------------------------
// Syntax access.

#[derive(Clone, Copy)]
pub(super) struct Src<'a> {
    pub(super) item: Item<'a>,
}

impl<'a> Src<'a> {
    pub(super) fn kind(&self, n: NodeId) -> K {
        self.item.tree.node(n).kind
    }
    pub(super) fn kids(&self, n: NodeId) -> &'a [NodeId] {
        self.item.tree.children(n)
    }
    pub(super) fn kid(&self, n: NodeId, i: usize) -> R<NodeId> {
        self.kids(n)
            .get(i)
            .copied()
            .ok_or_else(|| format!("missing child {i} of `{}`", self.text(n)))
    }
    pub(super) fn span(&self, n: NodeId) -> (u32, u32) {
        let s = self.item.tree.node(n).origin.span;
        (s.start, s.end)
    }
    pub(super) fn text(&self, n: NodeId) -> &'a str {
        let (a, b) = self.span(n);
        let o = self.item.file_offset as usize;
        &self.item.text[o + a as usize..o + b as usize]
    }
    pub(super) fn tok(&self, n: NodeId) -> &'a str {
        self.item
            .tree
            .node(n)
            .token
            .map(|t| self.item.tree.token_text(self.item.text, t))
            .unwrap_or("")
    }
    /// The source from a node's start up to (excluding) another node's start, whitespace-collapsed.
    pub(super) fn header(&self, from: NodeId, upto: u32) -> String {
        let (a, _) = self.span(from);
        let o = self.item.file_offset as usize;
        self.item.text[o + a as usize..o + upto as usize]
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }
    pub(super) fn walk(&self, n: NodeId, f: &mut impl FnMut(NodeId)) {
        f(n);
        for &c in self.kids(n) {
            self.walk(c, f);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Module model: types and function signatures from the syntax of the extracted file.

pub(super) struct TypeInfo<'a> {
    pub(super) name: String,
    pub(super) src: Src<'a>,
    pub(super) root: NodeId,
    pub(super) generics: Vec<String>,
    pub(super) body: TypeBody,
    pub(super) order: usize,
}

pub(super) enum TypeBody {
    Record(Vec<(String, NodeId)>),
    Variant(Vec<(String, Vec<NodeId>)>),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Mode {
    View,
    Mut,
    Take,
}

pub(super) struct ParamInfo {
    pub(super) name: String,
    pub(super) mode: Mode,
    pub(super) ty: NodeId,
    pub(super) tx: bool,
}

#[derive(Clone, PartialEq, Eq)]
pub(super) enum State {
    /// The store transaction: `Hirsel.store.Tx <tables data>`.
    Tx,
    /// A `mut` parameter of another type, which becomes the state.
    Param(String),
    /// Any state (`{S : Type}`): a fallible function that touches none.
    Generic,
}

pub(super) struct FnInfo<'a> {
    pub(super) name: String,
    pub(super) path: DefPath,
    pub(super) src: Src<'a>,
    pub(super) root: NodeId,
    pub(super) generics: Vec<String>,
    pub(super) params: Vec<ParamInfo>,
    pub(super) result: Option<NodeId>,
    /// `Some(state)` when the function returns `Result` and runs in `Hirsel.Prog`.
    pub(super) prog: Option<State>,
    pub(super) order: usize,
}

pub(super) struct ModuleInfo<'a> {
    pub(super) prefix: String,
    pub(super) types: BTreeMap<String, TypeInfo<'a>>,
    pub(super) fns: BTreeMap<String, FnInfo<'a>>,
    /// Constructor name -> (Lean name, arity or None for a record constructor, owning type).
    pub(super) ctors: BTreeMap<String, (String, Option<usize>, String)>,
    pub(super) tables: Option<String>,
}

pub(super) fn type_info<'a>(src: Src<'a>, root: NodeId, order: usize) -> R<TypeInfo<'a>> {
    let name = src.tok(root).to_owned();
    let mut generics = Vec::new();
    let mut body = None;
    for &c in src.kids(root) {
        match src.kind(c) {
            K::Generics => generics = src.kids(c).iter().map(|&g| src.tok(g).to_owned()).collect(),
            K::Fields => {
                let mut fields = Vec::new();
                for &f in src.kids(c) {
                    if src.kind(f) != K::Field {
                        bail!("type {name}: unsupported field form {:?}", src.kind(f));
                    }
                    fields.push((src.tok(f).to_owned(), src.kid(f, 0)?));
                }
                body = Some(TypeBody::Record(fields));
            }
            K::Variants => {
                let mut cases = Vec::new();
                for &v in src.kids(c) {
                    if src.kind(v) != K::Variant {
                        bail!("type {name}: unsupported variant form {:?}", src.kind(v));
                    }
                    let mut payload = Vec::new();
                    for &p in src.kids(v) {
                        match src.kind(p) {
                            K::Payload => payload.extend(src.kids(p).iter().copied()),
                            other => bail!("type {name}: unsupported payload form {other:?}"),
                        }
                    }
                    cases.push((src.tok(v).to_owned(), payload));
                }
                body = Some(TypeBody::Variant(cases));
            }
            K::Public | K::Opaque | K::Bounds | K::Attribute => {}
            other => bail!("type {name}: unsupported declaration part {other:?}"),
        }
    }
    let body = body.ok_or_else(|| format!("type {name}: no fields or variants"))?;
    Ok(TypeInfo {
        name,
        src,
        root,
        generics,
        body,
        order,
    })
}

pub(super) fn fn_info<'a>(
    src: Src<'a>,
    root: NodeId,
    path: DefPath,
    order: usize,
) -> R<FnInfo<'a>> {
    let name = src.tok(root).to_owned();
    let mut generics = Vec::new();
    let mut params = Vec::new();
    let mut result = None;
    for &c in src.kids(root) {
        match src.kind(c) {
            K::Generics => generics = src.kids(c).iter().map(|&g| src.tok(g).to_owned()).collect(),
            K::Parameters => {
                for &p in src.kids(c) {
                    let mode = match src.kind(p) {
                        K::Parameter => Mode::View,
                        K::MutParameter => Mode::Mut,
                        K::TakeParameter => Mode::Take,
                        other => bail!("{name}: unsupported parameter form {other:?}"),
                    };
                    let ty = src.kid(p, 0)?;
                    let tx = src.text(ty) == "store.Tx";
                    params.push(ParamInfo {
                        name: src.tok(p).to_owned(),
                        mode,
                        ty,
                        tx,
                    });
                }
            }
            K::ResultType => result = Some(src.kid(c, 0)?),
            K::Public | K::Block => {}
            other => bail!("{name}: unsupported function part {other:?}"),
        }
    }
    let returns_result = result
        .is_some_and(|t| src.kind(t) == K::TypeArguments && src.text(src.kids(t)[0]) == "Result");
    let has_tx = params.iter().any(|p| p.tx);
    let muts: Vec<&ParamInfo> = params
        .iter()
        .filter(|p| p.mode == Mode::Mut && !p.tx)
        .collect();
    let prog = if returns_result {
        Some(match (has_tx, muts.as_slice()) {
            (true, []) => State::Tx,
            (false, [p]) => State::Param(p.name.clone()),
            (false, []) => State::Generic,
            _ => bail!("{name}: more than one piece of mutable state"),
        })
    } else {
        if has_tx || !muts.is_empty() {
            bail!("{name}: a function with mutable state must return Result");
        }
        None
    };
    Ok(FnInfo {
        name,
        path,
        src,
        root,
        generics,
        params,
        result,
        prog,
        order,
    })
}

impl<'a> ModuleInfo<'a> {
    pub(super) fn ty(&self, src: Src, n: NodeId, generics: &[String]) -> R<String> {
        Ok(match src.kind(n) {
            K::UnitType => "Unit".into(),
            K::TypePath => {
                let t = src.text(n);
                match t {
                    "u8" => "UInt8".into(),
                    "u16" => "UInt16".into(),
                    "u32" => "UInt32".into(),
                    "u64" => "UInt64".into(),
                    "bool" => "Bool".into(),
                    "Bytes" => "Hirsel.Bytes".into(),
                    "store.Error" => "Hirsel.store.Error".into(),
                    "store.CallInfo" => "Hirsel.store.CallInfo".into(),
                    _ if generics.iter().any(|g| g == t) => t.to_owned(),
                    _ if self.types.contains_key(t) => t.to_owned(),
                    _ => bail!("no Lean type for `{t}`"),
                }
            }
            K::TypeArguments => {
                let kids = src.kids(n);
                let head = src.text(kids[0]);
                if head == "Inline" {
                    let [_, element, width] = kids else {
                        bail!("invalid Inline type")
                    };
                    let width: u64 = src
                        .text(*width)
                        .parse()
                        .map_err(|_| "unsupported Inline width")?;
                    let element = self.ty(src, *element, generics)?;
                    return Ok(format!("Vector {} {width}", atom(element)));
                }
                let args = kids[1..]
                    .iter()
                    .map(|&a| self.ty(src, a, generics).map(atom))
                    .collect::<R<Vec<_>>>()?;
                match (head, args.as_slice()) {
                    ("Arc", [a]) => a.to_owned(),
                    ("Option", [a]) => format!("Option {a}"),
                    ("List", [a]) => format!("List {a}"),
                    ("Result", [t, e]) => format!("Except {e} {t}"),
                    ("std.codec.Codec", [a]) => format!("Hirsel.std.codec.Codec {a}"),
                    ("store.Table", [k, v]) => {
                        let d = self
                            .tables
                            .as_deref()
                            .ok_or("store.Table outside the tables record")?;
                        format!("Hirsel.store.Table {d}.Data {k} {v}")
                    }
                    ("store.Index", [j, k, v, p]) => format!("Hirsel.store.Index {j} {k} {v} {p}"),
                    _ if self
                        .types
                        .get(head)
                        .is_some_and(|t| t.generics.len() == args.len()) =>
                    {
                        format!("{head} {}", args.join(" "))
                    }
                    _ => bail!("no Lean type for `{}`", src.text(n)),
                }
            }
            K::TupleType => {
                let parts = src
                    .kids(n)
                    .iter()
                    .map(|&a| self.ty(src, a, generics).map(atom))
                    .collect::<R<Vec<_>>>()?;
                parts.join(" × ")
            }
            other => bail!("no Lean type for {other:?} `{}`", src.text(n)),
        })
    }

    /// Local type names a type expression names.
    pub(super) fn type_refs(&self, src: Src, n: NodeId, out: &mut BTreeSet<String>) {
        src.walk(n, &mut |m| {
            if src.kind(m) == K::TypePath && self.types.contains_key(src.text(m)) {
                out.insert(src.text(m).to_owned());
            }
        });
    }

    pub(super) fn plain(&self, src: Src, n: NodeId, seen: &mut BTreeSet<String>) -> bool {
        let mut ok = true;
        src.walk(n, &mut |m| {
            if src.kind(m) != K::TypePath {
                return;
            }
            let t = src.text(m);
            if t.starts_with("store.Table")
                || t.starts_with("store.Index")
                || t.starts_with("std.codec")
            {
                ok = false;
            } else if let Some(ti) = self.types.get(t) {
                if !ti.generics.is_empty() {
                    ok = false;
                } else if seen.insert(t.to_owned()) {
                    ok &= self.type_plain(ti, seen);
                }
            }
        });
        ok
    }

    pub(super) fn type_plain(&self, t: &TypeInfo, seen: &mut BTreeSet<String>) -> bool {
        t.generics.is_empty()
            && match &t.body {
                TypeBody::Record(fs) => fs.iter().all(|(_, n)| self.plain(t.src, *n, seen)),
                TypeBody::Variant(cs) => cs
                    .iter()
                    .all(|(_, ps)| ps.iter().all(|n| self.plain(t.src, *n, seen))),
            }
    }

    pub(super) fn is_tables(&self, t: &TypeInfo) -> bool {
        matches!(&t.body, TypeBody::Record(fs) if !fs.is_empty() && fs.iter().all(|(_, n)| {
            let s = t.src.text(*n);
            s.starts_with("store.Table[") || s.starts_with("store.Index[")
        }))
    }
}
