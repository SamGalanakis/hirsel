//! SEM-10 declaration checks. Schema history never participates in value layout.
use crate::*;
use hirsel_core_resolve::{Site, Target};
use hirsel_core_syntax::{NodeId, NodeKind};

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct CodecDecl {
    pub via: Option<CodecVia>,
    pub removed: Vec<RemovedDecl>,
    /// Parameters conditioning codec, including removed types or the via representation.
    pub params: Vec<u32>,
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
                        if part == "from" { return self.codec_from_shape(f, d, via.ty, &params); }
                        let TyKind::Adt(owner, args) = self.types.kind(f.params.first().map_or(ERROR, |p| p.ty)) else { return false };
                        f.at.module == at.module && !f.waits && f.generics.len() == decl.generics.len()
                            && f.generics.iter().zip(&params).all(|(g, &bounds)| g.bounds & !bounds == 0)
                            && f.params.len() == 1 && f.params[0].mode == Mode::View && !f.params[0].bytes
                            && owner.index() == d && args.iter().enumerate().all(|(i, &a)| matches!(self.types.kind(a), TyKind::Param(p) if *p as usize == i))
                            && f.anchors == [0] && matches!(self.types.kind(f.result), TyKind::View(r) if *r == via.ty)
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
    fn codec_from_shape(&self, f: &FnDecl, d: usize, r: TyId, params: &[u16]) -> bool {
        if f.at.module != self.decls[d].at.module
            || f.waits
            || f.generics.len() != params.len()
            || !f
                .generics
                .iter()
                .zip(params)
                .all(|(g, &bounds)| g.bounds & !bounds == 0)
            || f.params.len() != 1
            || f.params[0].mode != Mode::Take
            || f.params[0].bytes
            || f.params[0].ty != r
        {
            return false;
        }
        let TyKind::Adt(result, args) = self.types.kind(f.result) else {
            return false;
        };
        if self.decls[result.index()].path.0.name != "Result"
            || !self.decls[result.index()].structural
            || args.len() != 2
        {
            return false;
        }
        matches!(self.types.kind(args[0]), TyKind::Adt(owner, xs) if owner.index() == d && xs.iter().enumerate().all(|(i, &a)| matches!(self.types.kind(a), TyKind::Param(p) if *p as usize == i)))
    }
}
