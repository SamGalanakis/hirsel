//! Toolchain constants and constants that depend on them (LANG-20, PROG-6): each is
//! evaluated once per target core module `target` names, and a failure on any one
//! refuses it on all.
use super::*;
use hirsel_core_layout::{Arch, Os, Target};
use hirsel_core_syntax::Tree;

/// A constant item's last child and whether it is an initialiser: a constant without
/// one ends at its type, after any attribute and visibility marker.
pub(super) fn initialiser(tree: &Tree, kids: &[NodeId]) -> (NodeId, bool) {
    let last = *kids.last().unwrap();
    let before = kids.len().checked_sub(2).map(|i| tree.node(kids[i]).kind);
    let marker = matches!(
        before,
        None | Some(NodeKind::Attribute | NodeKind::Public | NodeKind::Opaque)
    );
    (last, !marker)
}

impl Program<'_> {
    /// The values of a constant without an initialiser: only `target.os: Os` and
    /// `target.arch: Arch` in core module `target`, which the toolchain supplies.
    pub(super) fn toolchain(&mut self, at: ItemRef, ty: TyId) -> Option<[ConstValue; 3]> {
        let item = self.item(at);
        let name = self
            .index
            .symbols
            .text(item.symbol(item.tree.node(item.root).token.unwrap()));
        let module = self.index.module_path(self.modules[at.module as usize].id);
        let in_target = module.package.name == "core" && module.modules == ["target"];
        let ty_name = match (name, in_target) {
            ("os", true) => "Os",
            ("arch", true) => "Arch",
            _ => {
                self.const_refuse(
                    at,
                    item.root,
                    "a constant needs an initialiser; only the toolchain supplies \
                     `target.os` and `target.arch`",
                );
                return None;
            }
        };
        let TyKind::Adt(d, _) = *self.types.kind(ty) else {
            return None;
        };
        let decl = &self.decls[d.index()];
        let DeclBody::Variant(cases) = &decl.body else {
            return None;
        };
        if decl.path.0.name != ty_name {
            return None;
        }
        let start = self.constant_parts.len() as u32;
        let mut values = [None; 3];
        for target in Target::ALL {
            let case = if name == "os" {
                match target.os() {
                    Os::Linux => "Linux",
                    Os::Macos => "Macos",
                }
            } else {
                match target.arch() {
                    Arch::X86_64 => "X86_64",
                    Arch::Aarch64 => "Aarch64",
                }
            };
            let case = cases.iter().position(|c| c.name == case)? as u32;
            values[target.index()] = Some(ConstValue {
                ty,
                data: ConstData::Fields {
                    start,
                    len: 0,
                    case: Some(case),
                },
            });
        }
        Some(values.map(Option::unwrap))
    }

    /// Evaluate constant `i`. One that reads a target-dependent constant is evaluated
    /// for every target, and its refusals name the target they fail on.
    pub(super) fn evaluate_constant(&mut self, i: usize, string: Option<TyId>) {
        let c = &self.constants[i];
        let (at, expr, ty) = (c.at, c.expr, c.ty);
        if ty == ERROR {
            return;
        }
        let (refusals, parts) = (self.refusals.len(), self.constant_parts.len());
        let (value, targeted) = self.evaluate_at(at, expr, ty, string, None);
        if !targeted {
            self.constants[i].value = value;
            return;
        }
        self.refusals.truncate(refusals);
        self.constant_parts.truncate(parts);
        let mut values = [None; 3];
        for target in Target::ALL {
            let mark = self.refusals.len();
            values[target.index()] = self
                .evaluate_at(at, expr, ty, string, Some(target.index()))
                .0;
            for r in &mut self.refusals[mark..] {
                r.message = format!("{}, for target {}", r.message, target.triple());
            }
            if values[target.index()].is_none() {
                return;
            }
        }
        self.constants[i].targets = Some(values.map(Option::unwrap));
    }

    fn evaluate_at(
        &mut self,
        at: ItemRef,
        expr: NodeId,
        ty: TyId,
        string: Option<TyId>,
        target: Option<usize>,
    ) -> (Option<ConstValue>, bool) {
        let mut cx = ConstCx {
            p: self,
            at,
            ascribing: false,
            target,
            targeted: false,
        };
        let value = if Some(ty) == string {
            cx.static_string(expr, ty)
        } else {
            cx.expr(expr, Some(ty), true)
        };
        (value, cx.targeted)
    }
}

impl ConstCx<'_, '_> {
    /// Constant `id`'s value at this evaluation's target.
    pub(super) fn read(&mut self, id: u32) -> Option<ConstValue> {
        let c = &self.p.constants[id as usize];
        match (c.targets, self.target) {
            (Some(values), Some(t)) => Some(values[t]),
            (Some(_), None) => {
                self.targeted = true;
                None
            }
            (None, _) => c.value,
        }
    }
}

impl Program<'_> {
    /// Decide which `match` scrutinees and `if` conditions are constant expressions
    /// (PROG-6): exactly what a constant's initialiser admits, so no call and no local.
    /// Deciding refuses nothing; a branch that does not evaluate keeps every arm.
    pub(crate) fn decide_constant_branches(&mut self) {
        for m in self.library_modules..self.modules.len() {
            for i in 0..self.modules[m].syntax.items().len() {
                let at = ItemRef {
                    module: m as u32,
                    item: i as u32,
                };
                let item = self.item(at);
                if item.range.has_errors || item.tree.node(item.root).kind == NodeKind::Constant {
                    continue;
                }
                for n in item.range.nodes.clone() {
                    let n = NodeId(n);
                    let want = match item.tree.node(n).kind {
                        NodeKind::Match => None,
                        NodeKind::If => Some(BOOL),
                        _ => continue,
                    };
                    let head = item.tree.children(n)[0];
                    if item.tree.node(head).kind != NodeKind::ConditionLet {
                        self.decide_branch(at, head, want);
                    }
                }
            }
        }
    }

    fn decide_branch(&mut self, at: ItemRef, n: NodeId, want: Option<TyId>) {
        let (refusals, gaps, parts) = (
            self.refusals.len(),
            self.gaps.len(),
            self.constant_parts.len(),
        );
        let mut cx = ConstCx {
            p: self,
            at,
            ascribing: false,
            target: None,
            targeted: false,
        };
        let value = cx.expr(n, want, true);
        let values = if cx.targeted {
            Target::ALL.map(|t| {
                cx.target = Some(t.index());
                cx.expr(n, want, true)
            })
        } else {
            [value; 3]
        };
        let failed = refusals != self.refusals.len() || gaps != self.gaps.len();
        self.refusals.truncate(refusals);
        self.gaps.truncate(gaps);
        match values {
            [Some(a), Some(b), Some(c)] if !failed => {
                self.constant_branches.insert((at, n), [a, b, c]);
            }
            _ => self.constant_parts.truncate(parts),
        }
    }
}
