//! Release composition consumes the existing reachable/materialized products.
//! The release unit is the whole program: every reached body, the program's and
//! each library's alike, is an internal definition of this one LLVM module.
use super::*;

/// The release unit's key: its inputs and the role facts each unit consumed.
fn unit_key(
    inputs: &impl serde::Serialize,
    consumed: &[&[hirsel_core_ir::roles::BodyFacts]],
) -> io::Result<ContentHash> {
    crate::native::key(
        "hirsel.core-release-unit.v3",
        &(hirsel_core_ir::roles::FACTS_SCHEMA, consumed, inputs),
    )
}

impl CheckProduct {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn emit_release(
        &self,
        entry: &DefPath,
        reached: &[Identity],
        materialized: &crate::instances::InstanceSet,
        options: &NativeOptions,
        runtime: ContentHash,
        store: &Store,
    ) -> Result<Vec<ObjectProduct>, NativeStop> {
        let compiler = compiler_identity()?;
        // Release is LLVM's alone: every object here is libLLVM's output.
        let llvm = crate::tool_inputs::llvm_library()?;
        let mut modules: Vec<_> = reached
            .iter()
            .map(|instance| materialized.get(instance).ir().map(Cow::Borrowed))
            .collect::<io::Result<_>>()?;
        let body_identities = crate::callable_variants::prepare(
            &mut modules,
            reached,
            materialized,
            entry,
            options.instance_limit,
        )?;
        let refs = modules.iter().map(|m| &**m).collect::<Vec<_>>();
        // The checked body keys identify the source-neutral published modules.
        // All shape-owner modules used by generic materialization are in this
        // ordered closure. Entry and compiler identify the deterministic callable
        // preparation above, including appended private units.
        let facts_key = crate::native_facts::key(compiler, entry, &body_identities, reached, &[])?;
        let crate::native_facts::ProgramFacts { facts, consumed } =
            crate::native_facts::get(store, facts_key, &refs, &[])?;
        let instance = reached
            .iter()
            .find(|i| &i.definition == entry)
            .expect("reached entry");
        let object_key = unit_key(
            &(
                compiler,
                llvm,
                &options.target,
                options.debug_lines,
                options.logical_charge_report.is_some(),
                !hirsel_llvm_lazy::core::facts_enabled(),
                runtime,
                instance,
                &refs,
            ),
            &consumed.iter().map(Vec::as_slice).collect::<Vec<_>>(),
        )?;
        let (identity, object, compiled) =
            if let Some((hash, object)) = crate::native_object::load(store, object_key)? {
                (hash, object, false)
            } else {
                let units = modules
                    .iter()
                    .zip(&facts)
                    .map(|(module, facts)| hirsel_llvm_lazy::Unit::Core {
                        module,
                        facts,
                        observe_charge: options.logical_charge_report.is_some(),
                        debug_lines: options.debug_lines,
                    })
                    .collect::<Vec<_>>();
                let bytes = hirsel_llvm_lazy::emit_program_for_target(
                    &units,
                    &[instance.symbol()],
                    hirsel_core_layout::Target::from_triple(&options.target)
                        .expect("supported native target"),
                )
                .map_err(|message| NativeStop::Emit {
                    definition: entry.clone(),
                    message,
                })?;
                let object = hirsel_llvm_lazy::core::finish_native_object(
                    bytes,
                    object_key,
                    options.debug_lines,
                )
                .map_err(|message| NativeStop::Emit {
                    definition: entry.clone(),
                    message,
                })?;
                (
                    crate::native_object::publish(store, object_key, &object)?,
                    object,
                    true,
                )
            };
        Ok(vec![ObjectProduct {
            private_variants: modules
                .iter()
                .map(|m| {
                    m.code_variants
                        .keys()
                        .filter(|f| m.functions[f.index()].body.is_some())
                        .count()
                })
                .sum(),
            instance: instance.clone(),
            path: crate::native_object::path(store, identity.address),
            identity: identity.address,
            check: identity.check,
            verify: false,
            debug: object.debug,
            debug_record: None,
            compiled,
        }])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn consumed_role_facts_invalidate_the_release_unit_key() {
        use hirsel_core_ir::roles::{BodyFacts, ParamFacts};
        let original = vec![BodyFacts {
            code: ContentHash::of_bytes(b"callee code identity"),
            params: vec![ParamFacts {
                sealed: true,
                place_blind: true,
            }],
        }];
        let before = unit_key(&"unchanged body and signature", &[&original]).unwrap();
        assert_eq!(
            before,
            unit_key(&"unchanged body and signature", &[&original.clone()]).unwrap()
        );
        for field in 0..2 {
            let mut changed = original.clone();
            if field == 0 {
                changed[0].params[0].sealed = false;
            } else {
                changed[0].params[0].place_blind = false;
            }
            assert_ne!(
                before,
                unit_key(&"unchanged body and signature", &[&changed]).unwrap()
            );
        }
    }
}
