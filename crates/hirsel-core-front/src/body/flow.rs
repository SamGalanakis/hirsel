//! Source facts consumed while determining a flow body's PROG-9 reach.
use super::*;
use crate::decls::{H_CAPABILITY, H_RESOURCE, holds_components};
use hirsel_core_resolve::Site;

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Facts {
    pub waits: bool,
    pub stages: Vec<(Site, hirsel_core_ir::RevisionRoot)>,
    pub violations: Vec<Violation>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Violation {
    pub at: u32,
    pub when: u64,
    pub message: String,
}

impl Cx<'_, '_, '_> {
    fn add_flow_violation(&mut self, n: NodeId, when: u64, message: impl FnOnce(&Self) -> String) {
        let at = self.span(n).start;
        // Keep the earliest witness for a fixed violation or a set of parameters.
        let violations = &self.out.flow.violations;
        let found = violations.iter().position(|v| v.when == when);
        if found.is_some_and(|i| violations[i].at <= at) {
            return;
        }
        let violation = Violation {
            at,
            when,
            message: message(self),
        };
        if let Some(i) = found {
            self.out.flow.violations[i] = violation;
        } else {
            self.out.flow.violations.push(violation);
        }
    }

    pub(super) fn flow_expression(&mut self, n: NodeId, ty: TyId) {
        if ty <= HOLE {
            return;
        }
        let (holds, parameters) = *self
            .flow_holds
            .entry(ty)
            .or_insert_with(|| holds_components(&self.p.decls, &self.t, ty));
        let holds = holds & (H_RESOURCE | H_CAPABILITY);
        if holds != 0 || parameters != 0 {
            let when = if holds != 0 { 0 } else { parameters };
            self.add_flow_violation(n, when, |cx| {
                let shown = cx.shown(ty);
                let name = &cx.fd.path;
                format!(
                    "an expression in `{name}` has type `{shown}` holding a resource or capability"
                )
            });
        }
    }

    pub(super) fn flow_callable(&mut self, n: NodeId) {
        self.add_flow_violation(n, 0, |cx| {
            format!("a call in `{}` goes through a callable value", cx.fd.path)
        });
    }

    pub(super) fn flow_waits(&mut self, n: NodeId) {
        self.add_flow_violation(n, 0, |cx| {
            format!("a closure written in `{}` is declared `waits`", cx.fd.path)
        });
    }

    pub(super) fn flow_stage(&mut self, argument: NodeId, value: Option<ValueId>) {
        let function = value
            .and_then(|v| self.static_fns.get(&v))
            .map(|&(f, ..)| f);
        if let Some(function) = function {
            self.out.flow.stages.push((
                self.p.site(self.at, argument),
                self.e.module.revision_root(function),
            ));
        } else if !self.broken {
            self.refuse(
                "CONC-14",
                argument,
                "the stage body's static reach cannot be closed",
            );
        }
    }
}
