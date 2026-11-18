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
    fn add_flow_violation(&mut self, n: NodeId, when: u64, message: String) {
        let at = self.span(n).start;
        // Keep the earliest witness for a fixed violation or a set of parameters.
        let violations = &mut self.out.flow.violations;
        if let Some(found) = violations.iter_mut().find(|v| v.when == when) {
            if at < found.at {
                *found = Violation { at, when, message };
            }
        } else {
            violations.push(Violation { at, when, message });
        }
    }

    pub(super) fn flow_expression(&mut self, n: NodeId, ty: TyId) {
        let (holds, parameters) = holds_components(&self.p.decls, &self.t, ty);
        let holds = holds & (H_RESOURCE | H_CAPABILITY);
        if holds != 0 || parameters != 0 {
            let shown = self.shown(ty);
            let name = &self.fd.path;
            self.add_flow_violation(
                n,
                if holds != 0 { 0 } else { parameters },
                format!(
                    "an expression in `{name}` has type `{shown}` holding a resource or capability"
                ),
            );
        }
    }

    pub(super) fn flow_callable(&mut self, n: NodeId) {
        let name = &self.fd.path;
        self.add_flow_violation(
            n,
            0,
            format!("a call in `{name}` goes through a callable value"),
        );
    }

    pub(super) fn flow_waits(&mut self, n: NodeId) {
        let name = &self.fd.path;
        self.add_flow_violation(
            n,
            0,
            format!("a closure written in `{name}` is declared `waits`"),
        );
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
