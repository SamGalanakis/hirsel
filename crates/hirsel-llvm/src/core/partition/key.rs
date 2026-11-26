//! Inputs read by LLVM lowering after layout and step exposure.
use super::*;
use hirsel_core_ids::{ContentHash, encoding::hash_fixed};
use std::collections::BTreeSet;

pub(super) struct Keys {
    shared: ContentHash,
    bodies: Vec<Option<ContentHash>>,
}

impl Keys {
    pub(super) fn new(
        module: &Module,
        analyses: &Analyses,
        roles: &hirsel_core_ir::roles::Facts,
        facts: bool,
    ) -> std::io::Result<Self> {
        // Keep table indices in the key: lowering uses them in names and in
        // its references to shared rows. The analyses' outputs cover changes
        // outside a partition that affect its ABI or emitted attributes.
        let functions: Vec<_> = module
            .functions
            .iter()
            .map(|f| {
                (
                    f.instance,
                    f.signature,
                    f.body.is_some(),
                    f.versioned,
                    f.cold,
                )
            })
            .collect();
        let items = &analyses.items;
        let callbacks = items
            .callback_sites
            .as_ref()
            .map(|sites| sites.roles().collect::<Vec<_>>());
        let roles = facts.then(|| roles.consumed(module));
        let inline: Vec<_> = analyses
            .inline
            .iter()
            .map(|policy| policy.map(|p| p as u8))
            .collect();
        let shared = hash_fixed(
            "hirsel.release-lowering-shared.v2",
            &(
                (
                    &module.items,
                    &module.types,
                    &module.signatures,
                    &module.instances,
                    &module.code_variants,
                    &module.step_free,
                    functions,
                    module.external_call_sites,
                    module.law_observation,
                ),
                (
                    &items.function_symbols,
                    &items.versioned,
                    &items.step_dispatch,
                    &analyses.failures.may_fail,
                    inline,
                    &analyses.noreturn,
                    &analyses.step_neutral,
                    callbacks,
                    hirsel_core_ir::roles::FACTS_SCHEMA,
                    roles,
                ),
                (
                    &module.coverage,
                    module.coverage_rebound.as_deref(),
                    &module.coverage_moved,
                ),
            ),
        )?;
        let bodies = module
            .functions
            .iter()
            .map(|f| {
                f.body
                    .as_ref()
                    .map(|body| {
                        // Parked returns are native preparation, outside the body codec.
                        hash_fixed(
                            "hirsel.release-lowering-body.v1",
                            &(body, &body.parked_returns),
                        )
                    })
                    .transpose()
            })
            .collect::<std::io::Result<_>>()?;
        Ok(Self { shared, bodies })
    }

    pub(super) fn partition(
        &self,
        module: &Module,
        analyses: &Analyses,
        members: &[bool],
        partition: u32,
    ) -> std::io::Result<ContentHash> {
        let mut callees = BTreeSet::new();
        let mut statics = BTreeSet::new();
        let mut own = Vec::new();
        for (i, body) in bodies(module, Some(members)) {
            own.push((i, self.bodies[i].unwrap()));
            // Step planning reads these bodies to decide if a direct call
            // can be crossed. All other cross-body reads were in Analyses.
            for call in &body.calls {
                if let Callee::Direct(f) = call.callee
                    && steps::read_by_callers(module, f)
                {
                    callees.insert(f);
                }
            }
            for inst in &body.insts {
                if let Inst::ReadStatic { data, .. } = *inst {
                    statics.insert(data);
                }
            }
        }
        let callees: Vec<_> = callees
            .into_iter()
            .map(|f| (f, self.bodies[f.index()]))
            .collect();
        let descriptors: Vec<_> = analyses
            .descriptors
            .refs(Some(members))
            .map(|(f, data, spawn)| {
                statics.insert(data);
                (f, data, spawn, &analyses.descriptors.roots[&f])
            })
            .collect();
        let statics: Vec<_> = statics
            .into_iter()
            .map(|s| (s, &module.statics[s.index()]))
            .collect();
        hash_fixed(
            "hirsel.release-lowering-partition.v1",
            &(
                self.shared,
                partition,
                own,
                callees,
                descriptors,
                statics,
                module.coverage.as_ref().map(|_| self.bodies.as_slice()),
            ),
        )
    }
}
