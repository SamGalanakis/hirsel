//! Foreign declarations (FFI-2, FFI-4 to FFI-6), before body checking or archive admission.
use crate::decls::is_core_prelude;
use crate::{
    DeclBody, DeclId, Def, ERROR, FnDecl, H_RAW_HANDLE, Mode, Program, Repr, Role, Rows, TyId,
    TyKind, TyRead, TypeDecl, UNIT, refusal,
};
use hirsel_core_ids::DefPath;
use hirsel_core_layout::Scalar;
use hirsel_core_resolve::{ModuleId, Refusal, Site, Target};
use hirsel_core_syntax::{NodeId, NodeKind};
use std::collections::BTreeMap;

pub enum AbiParameter {
    Scalar(Scalar),
    Bytes { mutable: bool },
    Handle { kind: DefPath, release: bool },
}

pub enum AbiResult {
    Unit,
    Scalar(Scalar),
    New(DefPath),
}

pub struct AbiSignature {
    pub name: String,
    pub waits: bool,
    pub params: Vec<(String, AbiParameter)>,
    pub result: AbiResult,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum Marker {
    Interface,
    Handle,
}

impl Marker {
    pub(crate) fn of(decl: &TypeDecl) -> Option<Self> {
        if decl.role == Some(Role::ForeignHandle) {
            Some(Self::Handle)
        } else if is_core_prelude(&decl.path, &["Foreign"]) {
            Some(Self::Interface)
        } else {
            None
        }
    }

    pub(crate) fn rule(self) -> &'static str {
        match self {
            Self::Interface => "FFI-4",
            Self::Handle => "FFI-6",
        }
    }

    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::Interface => "Foreign takes a foreign interface",
            Self::Handle => "foreign.Ptr takes a handle kind of this package",
        }
    }

    fn argument(self, p: &Program<'_>, ty: TyId) -> Option<TyId> {
        let TyKind::Adt(d, args) = p.types.kind(ty) else {
            return None;
        };
        let [argument] = args.as_slice() else {
            return None;
        };
        (Marker::of(&p.decls[d.index()])? == self).then_some(*argument)
    }
}

#[derive(Default)]
pub(crate) struct Names {
    interfaces: BTreeMap<DeclId, Site>,
    kinds: BTreeMap<DeclId, Site>,
}

impl Names {
    pub(crate) fn admits(
        &self,
        p: &Program<'_>,
        types: &impl TyRead,
        marker: Marker,
        ty: TyId,
        module: ModuleId,
    ) -> bool {
        let TyKind::Adt(d, _) = types.kind(ty) else {
            return false;
        };
        match marker {
            Marker::Interface => self.interfaces.contains_key(d),
            Marker::Handle => {
                self.kinds.contains_key(d)
                    && p.index.module_package(module)
                        == p.index.module_package(p.decls[d.index()].site.module)
            }
        }
    }
}

struct KindUse {
    first_use: Site,
    made: bool,
    releases: u32,
}

impl Program<'_> {
    /// The C ABI of a module whose declarations have passed FFI-2 and FFI-4 to FFI-6.
    pub fn foreign_abi(&self, module: ModuleId) -> Option<Vec<AbiSignature>> {
        if !self.index.modules[module.0 as usize].foreign || !self.refusals.is_empty() {
            return None;
        }
        let kind_name = |ty| {
            let TyKind::Adt(d, _) = self.types.kind(ty) else {
                unreachable!("a checked handle kind is a nominal type");
            };
            self.decls[d.index()].path.clone()
        };
        Some(
            self.fns
                .iter()
                .filter(|fd| fd.site.module == module)
                .map(|fd| {
                    let params = fd.params[1..]
                        .iter()
                        .map(|p| {
                            let abi = if self.foreign_bytes(p.ty) {
                                AbiParameter::Bytes {
                                    mutable: p.mode == Mode::Mut,
                                }
                            } else if let Some(kind) = Marker::Handle.argument(self, p.ty) {
                                AbiParameter::Handle {
                                    kind: kind_name(kind),
                                    release: p.mode == Mode::Take,
                                }
                            } else if let TyKind::Scalar(scalar) = self.types.kind(p.ty) {
                                AbiParameter::Scalar(*scalar)
                            } else {
                                unreachable!("a checked foreign parameter has an ABI form")
                            };
                            (self.index.symbols.text(p.name).to_owned(), abi)
                        })
                        .collect();
                    let result = if let Some(kind) = self.foreign_result_kind(fd.result) {
                        AbiResult::New(kind_name(kind))
                    } else if let TyKind::Scalar(scalar) = self.types.kind(fd.result) {
                        AbiResult::Scalar(*scalar)
                    } else {
                        AbiResult::Unit
                    };
                    AbiSignature {
                        name: fd.path.0.name.clone(),
                        waits: fd.waits,
                        params,
                        result,
                    }
                })
                .collect(),
        )
    }

    pub(crate) fn check_foreign_signatures(&mut self) {
        let (names, refusals) = self.foreign_headers();
        self.foreign_names.get_or_init(|| names);
        self.refusals.extend(refusals);
    }

    pub(crate) fn foreign_names(&self) -> &Names {
        self.foreign_names.get_or_init(|| self.foreign_headers().0)
    }

    fn foreign_headers(&self) -> (Names, Vec<Refusal>) {
        let mut names = Names::default();
        let mut refusals = Vec::new();
        if !self.index.modules.iter().any(|module| module.foreign) {
            return (names, refusals);
        }
        let mut interfaces = BTreeMap::new();
        let mut uses = BTreeMap::<(ModuleId, DeclId), KindUse>::new();
        for fd in self.fns.iter() {
            let module = fd.site.module;
            if !self.index.modules[module.0 as usize].foreign {
                continue;
            }
            let Some(first) = fd.params.first() else {
                refusals.push(refusal(
                    "FFI-2",
                    fd.site,
                    "a foreign function takes Foreign first",
                ));
                continue;
            };
            let Some(interface_ty) = Marker::Interface.argument(self, first.ty) else {
                if first.ty != ERROR {
                    refusals.push(refusal(
                        "FFI-2",
                        first.site,
                        "a foreign function takes Foreign first",
                    ));
                }
                continue;
            };
            if first.mode != Mode::View || first.bytes {
                refusals.push(refusal(
                    "FFI-2",
                    first.site,
                    "the Foreign parameter has no mode",
                ));
                continue;
            }
            let Some(interface) = self.foreign_marker_decl(interface_ty, module) else {
                continue; // The written type owns the FFI-4 diagnostic.
            };
            match interfaces.entry(module) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(interface);
                    claim(
                        &mut names.interfaces,
                        interface,
                        fd.site,
                        "FFI-4",
                        "two foreign modules name one interface",
                        &mut refusals,
                    );
                }
                std::collections::btree_map::Entry::Occupied(entry)
                    if *entry.get() != interface =>
                {
                    refusals.push(refusal(
                        "FFI-2",
                        first.site,
                        "a foreign module names one interface",
                    ));
                    continue;
                }
                _ => {}
            }

            let params = &fd.params[1..];
            let release = params.iter().any(|p| p.mode == Mode::Take);
            let result = match self.types.kind(fd.callable) {
                TyKind::Fn(_, result, ..) => *result,
                _ => fd.result,
            };
            let new_kind = self.foreign_result_kind(result);
            let returns_scalar = self.foreign_scalar(result);
            if release
                && (params.len() != 1 || !(result == UNIT || returns_scalar || result == ERROR))
            {
                refusals.push(refusal(
                    "FFI-5",
                    fd.site,
                    "a release takes only Foreign and one take handle, and returns unit or a scalar",
                ));
                continue;
            }
            for p in params {
                if let Some(kind) = Marker::Handle.argument(self, p.ty)
                    && !p.bytes
                    && matches!(p.mode, Mode::View | Mode::Take)
                {
                    if let Some(kind) = self.foreign_marker_decl(kind, module) {
                        let site = self.handle_site(fd, p.site);
                        note_kind(
                            &mut names,
                            &mut uses,
                            kind,
                            site,
                            false,
                            release,
                            &mut refusals,
                        );
                    }
                } else if p.ty != ERROR
                    && (p.bytes
                        || !match p.mode {
                            Mode::View => self.foreign_scalar(p.ty) || self.foreign_bytes(p.ty),
                            Mode::Mut => self.foreign_bytes(p.ty),
                            _ => false,
                        })
                {
                    refusals.push(refusal(
                        "FFI-5",
                        p.site,
                        "parameter is not a foreign ABI type and mode",
                    ));
                }
            }
            if let Some(kind) = new_kind {
                if let Some(kind) = self.foreign_marker_decl(kind, module) {
                    let site = self.handle_site(fd, fd.result_site);
                    note_kind(
                        &mut names,
                        &mut uses,
                        kind,
                        site,
                        true,
                        false,
                        &mut refusals,
                    );
                }
            } else if result != ERROR && result != UNIT && !returns_scalar {
                refusals.push(refusal(
                    "FFI-5",
                    fd.result_site,
                    "result is not a foreign ABI type",
                ));
            }
        }
        for use_ in uses.values() {
            if use_.releases > 1 || (use_.made && use_.releases == 0) {
                refusals.push(refusal(
                    "FFI-5",
                    use_.first_use,
                    "a made foreign handle kind has exactly one release",
                ));
            }
        }
        (names, refusals)
    }

    fn foreign_marker_decl(&self, ty: TyId, foreign: ModuleId) -> Option<DeclId> {
        let TyKind::Adt(d, args) = self.types.kind(ty) else {
            return None;
        };
        let decl = &self.decls[d.index()];
        let module = &self.index.modules[decl.site.module.0 as usize];
        (decl.public
            && decl.opaque
            && decl.repr == Repr::Own
            && args.is_empty()
            && matches!(&decl.body, DeclBody::Record(fields) if fields.is_empty())
            && !module.foreign
            && module.package == self.index.module_package(foreign))
        .then_some(*d)
    }

    fn foreign_scalar(&self, ty: TyId) -> bool {
        matches!(self.types.kind(ty), TyKind::Scalar(s) if !matches!(s, Scalar::Bool | Scalar::Char))
    }

    fn foreign_bytes(&self, ty: TyId) -> bool {
        matches!(self.types.kind(ty), TyKind::Adt(d, args)
            if self.decls[d.index()].role == Some(Role::List)
            && matches!(args.as_slice(), [element] if *element == crate::scalar(Scalar::U8)))
    }

    fn foreign_result_kind(&self, ty: TyId) -> Option<TyId> {
        match self.types.kind(ty) {
            TyKind::Adt(d, args) if is_core_prelude(&self.decls[d.index()].path, &["Option"]) => {
                Marker::Handle.argument(self, *args.first()?)
            }
            _ => None,
        }
    }

    fn handle_site(&self, fd: &FnDecl, header: Site) -> Site {
        let item = self.item(fd.at);
        for i in item.range.nodes.clone() {
            let id = NodeId(i);
            let node = item.tree.node(id);
            if node.kind == NodeKind::TypePath
                && node.origin.span.start >= header.span.start
                && node.origin.span.end <= header.span.end
                && let Some(Target::Definition(def)) = self.target(fd.at, id)
                && let Some(Def::Type(decl)) = self.by_definition[def.0 as usize]
                && self.decls[decl.index()].role == Some(Role::ForeignHandle)
            {
                return self.site(fd.at, id);
            }
        }
        header
    }

    pub(crate) fn mark_raw_handle_summaries(&mut self) {
        let base = self.decls.base_len();
        let mut work = Vec::new();
        for d in base..self.decls.len() {
            if self.decls[d].role == Some(Role::ForeignHandle) {
                self.decls[d].summary.holds |= H_RAW_HANDLE;
                work.push(d);
            }
        }
        if !self.pending.iter().any(|check| {
            matches!(
                check,
                crate::decls::Pending::ForeignMarker {
                    marker: Marker::Handle,
                    ..
                }
            )
        }) {
            return;
        }
        let mut dependents = vec![vec![]; self.decls.len() - base];
        for d in base..self.decls.len() {
            if owns_raw_handles(&self.decls[d]) {
                continue;
            }
            let mut already_raw = false;
            for field in self.decls[d].all_fields() {
                type_contains_row(&self.types, field.ty, &mut |row| {
                    let row = row.index();
                    if row < base {
                        already_raw |= self.decls[row].summary.holds & H_RAW_HANDLE != 0;
                    } else {
                        dependents[row - base].push(d);
                    }
                    false
                });
            }
            if already_raw && self.decls[d].summary.holds & H_RAW_HANDLE == 0 {
                self.decls[d].summary.holds |= H_RAW_HANDLE;
                work.push(d);
            }
        }
        while let Some(d) = work.pop() {
            for &using in &dependents[d - base] {
                if self.decls[using].summary.holds & H_RAW_HANDLE == 0 {
                    self.decls[using].summary.holds |= H_RAW_HANDLE;
                    work.push(using);
                }
            }
        }
    }

    pub(crate) fn check_raw_handle_positions(&mut self) {
        if !self.pending.iter().any(|check| {
            matches!(
                check,
                crate::decls::Pending::ForeignMarker {
                    marker: Marker::Handle,
                    ..
                }
            )
        }) {
            return;
        }
        let mut bad = Vec::new();
        for decl in self.decls.iter().skip(self.decls.base_len()) {
            if owns_raw_handles(decl) {
                continue;
            }
            if decl.repr == Repr::Resource && decl.public && !decl.opaque {
                if decl.summary.holds & H_RAW_HANDLE != 0 {
                    bad.push(refusal(
                        "FFI-6",
                        decl.site,
                        "a public resource that holds a raw handle is opaque",
                    ));
                }
                continue;
            }
            for f in decl.all_fields() {
                if holds_raw_handle(&self.decls, &self.types, f.ty) {
                    bad.push(refusal(
                        "FFI-6",
                        f.site,
                        "a raw handle field requires an owning resource",
                    ));
                }
            }
            if let DeclBody::Alias(ty) = decl.body
                && holds_raw_handle(&self.decls, &self.types, ty)
            {
                bad.push(refusal(
                    "FFI-6",
                    decl.site,
                    "a type alias cannot hold a raw handle",
                ));
            }
        }
        for fd in self.fns.iter().skip(self.fns.base_len()) {
            if fd.drop_of.is_some() || self.index.modules[fd.site.module.0 as usize].foreign {
                continue;
            }
            let item = self.item(fd.at);
            let public = item
                .tree
                .children(item.root)
                .iter()
                .any(|&c| item.tree.node(c).kind == hirsel_core_syntax::NodeKind::Public);
            let in_signature = |ty| {
                holds_raw_handle(&self.decls, &self.types, ty)
                    && (public || !self.raw_value_type(ty))
            };
            for param in &fd.params {
                if in_signature(param.ty) {
                    bad.push(refusal(
                        "FFI-6",
                        param.site,
                        "a raw handle cannot appear in this signature",
                    ));
                }
            }
            if in_signature(fd.result) {
                bad.push(refusal(
                    "FFI-6",
                    fd.result_site,
                    "a raw handle cannot appear in this signature",
                ));
            }
        }
        for c in self.constants.iter().skip(self.constants.base_len()) {
            if holds_raw_handle(&self.decls, &self.types, c.ty) {
                let item = self.item(c.at);
                let nodes = item.tree.children(item.root);
                let site = self.site(c.at, nodes[nodes.len() - 2]);
                bad.push(refusal(
                    "FFI-6",
                    site,
                    "a constant cannot hold a raw handle",
                ));
            }
        }
        self.refusals.extend(bad);
    }

    fn raw_value_type(&self, ty: TyId) -> bool {
        match self.types.kind(ty) {
            TyKind::Adt(row, args) if self.decls[row.index()].structural => {
                args.iter().all(|&arg| self.raw_value_type(arg))
            }
            TyKind::Tuple(items) => items.iter().all(|&item| self.raw_value_type(item)),
            TyKind::Adt(row, _) if self.decls[row.index()].role == Some(Role::ForeignHandle) => {
                true
            }
            _ => !holds_raw_handle(&self.decls, &self.types, ty),
        }
    }
}

fn owns_raw_handles(decl: &TypeDecl) -> bool {
    decl.repr == Repr::Resource && decl.finalizer.is_some() && (!decl.public || decl.opaque)
}

pub(crate) fn holds_raw_handle(decls: &Rows<TypeDecl>, types: &impl TyRead, ty: TyId) -> bool {
    type_contains_row(types, ty, &mut |row| {
        let decl = &decls[row.index()];
        decl.summary.holds & H_RAW_HANDLE != 0 || decl.role == Some(Role::ForeignHandle)
    })
}

fn type_contains_row(types: &impl TyRead, ty: TyId, test: &mut impl FnMut(DeclId) -> bool) -> bool {
    match types.kind(ty) {
        TyKind::Adt(decl, args) => {
            test(*decl) || args.iter().any(|&arg| type_contains_row(types, arg, test))
        }
        TyKind::Tuple(items) => items
            .iter()
            .any(|&item| type_contains_row(types, item, test)),
        TyKind::Anchored(item, _) | TyKind::Array(item, _) | TyKind::View(item) => {
            type_contains_row(types, *item, test)
        }
        TyKind::Fn(params, result, ..) => {
            params
                .iter()
                .any(|&(_, param)| type_contains_row(types, param, test))
                || type_contains_row(types, *result, test)
        }
        _ => false,
    }
}

fn claim(
    table: &mut BTreeMap<DeclId, Site>,
    decl: DeclId,
    site: Site,
    rule: &str,
    message: &str,
    refusals: &mut Vec<Refusal>,
) {
    match table.entry(decl) {
        std::collections::btree_map::Entry::Vacant(entry) => {
            entry.insert(site);
        }
        std::collections::btree_map::Entry::Occupied(entry)
            if entry.get().module != site.module =>
        {
            let mut r = refusal(rule, site, message);
            r.causal.push(*entry.get());
            refusals.push(r);
        }
        _ => {}
    }
}

fn note_kind(
    names: &mut Names,
    uses: &mut BTreeMap<(ModuleId, DeclId), KindUse>,
    kind: DeclId,
    site: Site,
    made: bool,
    release: bool,
    refusals: &mut Vec<Refusal>,
) {
    let kind_use = match uses.entry((site.module, kind)) {
        std::collections::btree_map::Entry::Vacant(entry) => {
            claim(
                &mut names.kinds,
                kind,
                site,
                "FFI-6",
                "two foreign interfaces name one handle kind",
                refusals,
            );
            entry.insert(KindUse {
                first_use: site,
                made: false,
                releases: 0,
            })
        }
        std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
    };
    kind_use.made |= made;
    kind_use.releases += u32::from(release);
}
