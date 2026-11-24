//! SEM-10 declaration checks. Schema history never participates in value layout.
use crate::*;
use hirsel_core_resolve::{Site, Target};
use hirsel_core_syntax::{NodeId, NodeKind};

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct CodecDecl {
    pub via: Option<CodecVia>,
    pub removed: Vec<RemovedDecl>,
    pub defaulted_fields: bool,
    /// Parameters conditioning codec, including removed types or the via representation.
    pub params: Vec<u32>,
    /// The derived maker `D$codec` (EXPANSION section 0), the function `codec.of`
    /// resolves to at this type; None while the derive skips the type.
    pub maker: Option<u32>,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct CodecVia {
    pub ty: TyId,
    pub node: NodeId,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct RemovedDecl {
    pub name: String,
    pub position: u32,
    pub variant: bool,
    pub fields: Vec<FieldDecl>,
    pub site: Site,
}
impl TypeDecl {
    /// SEM-10 wire tags count removed variants; runtime tags count only live cases.
    pub fn wire_position(&self, live_case: usize) -> u32 {
        let mut position = live_case as u32;
        if let Some(codec) = self.codec() {
            for removed in &codec.removed {
                if removed.variant && removed.position <= position {
                    position += 1;
                }
            }
        }
        position
    }
}
impl Program<'_> {
    /// Binds each codec declaration of this program to its derived maker, the function
    /// `<Name>$codec` the syntax derive generated beside it in the same module.
    pub(crate) fn bind_codec_makers(&mut self) {
        let mut makers = std::collections::HashMap::new();
        for f in self.fns.base_len()..self.fns.len() {
            let path = &self.fns[f].path.0;
            if let Some(owner) = path.name.strip_suffix("$codec") {
                makers.insert((&path.package, &path.modules, owner), f as u32);
            }
        }
        let mut bound = vec![];
        for d in self.decls.base_len()..self.decls.len() {
            let decl = &self.decls[d];
            if decl.declared & CODEC == 0 {
                continue;
            }
            let path = &decl.path.0;
            if let Some(&f) = makers.get(&(&path.package, &path.modules, path.name.as_str())) {
                bound.push((d, f));
            }
        }
        for (d, f) in bound {
            self.decls[d].codec_mut().maker = Some(f);
        }
    }
    pub(crate) fn check_codec_declarations(&mut self) {
        for d in self.decls.base_len()..self.decls.len() {
            let decl = &self.decls[d];
            let Some(codec) = decl.codec() else { continue };
            let at = decl.at;
            let params: Vec<_> = decl
                .generics
                .iter()
                .map(|g| g.bounds | decl.declared)
                .collect();
            let mut out = vec![];
            if let Some(via) = &codec.via {
                let item = self.item(at);
                let nodes = item.tree.children(via.node);
                if decl.declared & CODEC == 0 {
                    out.push(refusal(
                        "SEM-10",
                        self.site(at, via.node),
                        "via requires a codec declaration",
                    ));
                }
                if has_bounds(&self.decls, &self.types, via.ty, &params) & CODEC == 0 {
                    out.push(refusal(
                        "OWN-4",
                        self.site(at, nodes[0]),
                        "via type R is not codec",
                    ));
                }
                for (part, node) in [("to", nodes[1]), ("from", nodes[2])] {
                    let fd = match self.target(at, node) {
                        Some(Target::Definition(id)) => match self.by_definition[id.0 as usize] {
                            Some(Def::Fn(f)) => Some(&self.fns[f as usize]),
                            _ => None,
                        },
                        _ => None,
                    };
                    let valid = fd.is_some_and(|f| {
                        self.codec_owner_shape(f, d, via.ty, &params, part == "to")
                    });
                    if !valid {
                        out.push(refusal("SEM-10", self.site(at, node), if part == "to" {
                            "via to must name a function in the declaring module with shape fn(x: T) -> View[R] in x"
                        } else {
                            "via from must name a function in the declaring module with shape fn(take R) -> Result[T, E]"
                        }));
                    }
                }
            }
            if !codec.removed.is_empty() {
                let item = self.item(at);
                for &group in item.tree.children(item.root) {
                    if !matches!(
                        item.tree.node(group).kind,
                        NodeKind::Fields | NodeKind::Variants
                    ) {
                        continue;
                    }
                    let mut names = std::collections::HashMap::new();
                    for &node in item.tree.children(group) {
                        let n = item.tree.node(node);
                        let name = item.symbol(n.token.unwrap());
                        let removed =
                            matches!(n.kind, NodeKind::RemovedField | NodeKind::RemovedVariant);
                        if names
                            .insert(name, removed)
                            .is_some_and(|old| old || removed)
                        {
                            out.push(refusal(
                                "SEM-10",
                                self.site(at, node),
                                "a tombstone keeps its name taken",
                            ));
                        }
                    }
                }
                for removed in &codec.removed {
                    if decl.declared & CODEC == 0 || codec.via.is_some() {
                        out.push(refusal(
                            "SEM-10",
                            removed.site,
                            "removed requires a codec declaration without via",
                        ));
                    }
                    for field in &removed.fields {
                        if has_bounds(&self.decls, &self.types, field.ty, &params) & CODEC == 0 {
                            out.push(refusal(
                                "OWN-4",
                                field.site,
                                format!("removed field or payload `{}` is not codec", removed.name),
                            ));
                        }
                    }
                }
            }
            self.refusals.extend(out);
        }
    }
    fn codec_owner_shape(&self, f: &FnDecl, d: usize, r: TyId, params: &[u16], to: bool) -> bool {
        if f.at.module != self.decls[d].at.module
            || f.waits
            || f.params.len() != 1
            || f.params[0].mode != if to { Mode::View } else { Mode::Take }
            || f.params[0].bytes
            || f.outlives.iter().any(|(a, b)| a != b)
        {
            return false;
        }
        let owner = if to {
            f.params[0].ty
        } else {
            let TyKind::Adt(result, args) = self.types.kind(f.result) else {
                return false;
            };
            if !self.decls[result.index()].structural
                || self.decls[result.index()].path.0.name != "Result"
                || args.len() != 2
            {
                return false;
            }
            args[0]
        };
        let TyKind::Adt(owner, xs) = self.types.kind(owner) else {
            return false;
        };
        if owner.index() != d || xs.len() != params.len() {
            return false;
        }
        // Match the function's binders to the declaration through Self. A spelling
        // or binder order has no meaning; an extra bound or repeated binder does.
        let mut t = Local::new(&self.types);
        let mut arguments = vec![ERROR; f.generics.len()];
        for (i, &x) in xs.iter().enumerate() {
            let (p, region) = match t.kind(x) {
                TyKind::Param(p) => (*p as usize, false),
                TyKind::Region(Root::Generic(p)) => (*p as usize, true),
                _ => return false,
            };
            let Some(generic) = f.generics.get(p) else {
                return false;
            };
            if arguments[p] != ERROR
                || generic.region != region
                || region != self.decls[d].generics[i].region
                || generic.bounds & !params[i] != 0
            {
                return false;
            }
            arguments[p] = t.mk(if region {
                TyKind::Region(Root::Generic(i as u32))
            } else {
                TyKind::Param(i as u32)
            });
        }
        for (i, argument) in arguments.iter_mut().enumerate() {
            if *argument == ERROR {
                // An unused region binder is erased and needs no inference;
                // an unused type binder cannot be chosen by a codec call.
                if !f.generics[i].region {
                    return false;
                }
                *argument = t.mk(TyKind::Region(Root::Generic((params.len() + i) as u32)));
            }
        }
        if to {
            if f.anchors != [0] {
                return false;
            }
            let result = subst(&mut t, f.result, &arguments);
            matches!(t.kind(result), TyKind::View(referent) if *referent == r)
        } else {
            subst(&mut t, f.params[0].ty, &arguments) == r
        }
    }
}
