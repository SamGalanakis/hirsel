//! Release composition consumes the existing reachable/materialized products.
//! Published library objects stay reusable; their bodies are inline imports.
use super::*;

fn exports<'a>(
    entry: &InstanceKey,
    modules: impl IntoIterator<Item = (&'a Module, bool)>,
) -> Vec<String> {
    let mut exports = std::collections::BTreeSet::from([format!("h{}", selection(entry.digest()))]);
    // A cached library object may call a request-owned dictionary/finalizer.
    // Those references cross the native-object boundary even if an inline copy
    // of the library body disappears from the consuming codegen unit.
    for (module, imported) in modules {
        if imported {
            for (i, function) in module.functions.iter().enumerate() {
                if function.body.is_none() {
                    exports.insert(hirsel_llvm_lazy::core::symbol(
                        module,
                        hirsel_core_ir::FuncId(i as u32),
                    ));
                }
            }
        }
    }
    exports.into_iter().collect()
}

impl CheckProduct {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn emit_release(
        &self,
        entry: &DefPath,
        reached: &[InstanceKey],
        materialized: &crate::instances::InstanceSet,
        options: &NativeOptions,
        runtime: ContentHash,
        store: &Store,
        library_store: Option<&Store>,
    ) -> Result<Vec<ObjectProduct>, NativeStop> {
        let mut emitted = HashSet::new();
        let mut modules: Vec<_> = reached
            .iter()
            .map(|instance| define_once(materialized.get(instance).ir(), &mut emitted))
            .collect();
        let mut imported: Vec<_> = reached
            .iter()
            .map(|i| library_store.is_some() && !self.definitions.contains_key(&i.definition))
            .collect();
        crate::callable_variants::prepare(
            &mut modules,
            &mut imported,
            reached,
            materialized,
            entry,
            options.instance_limit,
        )?;
        let mut objects = Vec::new();
        for ((instance, ir), imported) in reached.iter().zip(&modules).zip(&imported) {
            if !imported || !ir.functions.iter().any(|f| f.body.is_some()) {
                continue;
            }
            let library_store = library_store.unwrap();
            let object_key = key(
                "hirsel.core-object.v1",
                &(
                    compiler_identity()?,
                    &options.target,
                    options.backend,
                    options.optimize,
                    options.debug_lines,
                    options.logical_charge_report.is_some(),
                    !hirsel_llvm_lazy::core::facts_enabled(),
                    runtime,
                    instance,
                    &materialized.get(instance).product.key,
                    &**ir,
                ),
            )?;
            let (identity, bytes, compiled) =
                if let Some((hash, bytes)) = cached(library_store, object_key)? {
                    (hash, bytes, false)
                } else {
                    let bytes = emit_unit(ir, options).map_err(|message| NativeStop::Emit {
                        definition: instance.definition.clone(),
                        message,
                    })?;
                    (publish(library_store, object_key, &bytes)?, bytes, true)
                };
            objects.push(ObjectProduct {
                private_variants: 0,
                instance: instance.clone(),
                identity,
                bytes,
                compiled,
            });
        }
        let instance = reached
            .iter()
            .find(|i| &i.definition == entry)
            .expect("reached entry");
        // Inline-body edits invalidate the consuming unit, including library
        // bodies. Published semantic products are read without source checking.
        let object_key = key(
            "hirsel.core-release-unit.v1",
            &(
                compiler_identity()?,
                &options.target,
                options.debug_lines,
                options.logical_charge_report.is_some(),
                !hirsel_llvm_lazy::core::facts_enabled(),
                runtime,
                instance,
                &modules,
                &imported,
            ),
        )?;
        let (identity, bytes, compiled) = if let Some((hash, bytes)) = cached(store, object_key)? {
            (hash, bytes, false)
        } else {
            let text = modules
                .iter()
                .map(|ir| {
                    hirsel_llvm_lazy::core::emit_ir_relocatable_for_target(
                        ir,
                        hirsel_core_layout::Target::from_triple(&options.target)
                            .expect("supported native target"),
                        options.logical_charge_report.is_some(),
                        options.debug_lines,
                    )
                })
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| NativeStop::Emit {
                    definition: entry.clone(),
                    message: e.to_string(),
                })?;
            let units = text
                .iter()
                .zip(&imported)
                .map(|(ir, imported)| hirsel_llvm_lazy::Unit {
                    ir,
                    imported: *imported,
                })
                .collect::<Vec<_>>();
            let exports = exports(
                instance,
                modules
                    .iter()
                    .zip(&imported)
                    .map(|(ir, imported)| (&**ir, *imported)),
            );
            let bytes = hirsel_llvm_lazy::emit_program_for_target(
                &units,
                &exports,
                hirsel_core_layout::Target::from_triple(&options.target)
                    .expect("supported native target"),
            )
            .map_err(|message| NativeStop::Emit {
                definition: entry.clone(),
                message,
            })?;
            (publish(store, object_key, &bytes)?, bytes, true)
        };
        objects.push(ObjectProduct {
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
            identity,
            bytes,
            compiled,
        });
        Ok(objects)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cached_library_calls_keep_request_symbols_exported() {
        let library = hirsel_core_ir::text::parse(
            r#"core v1
sig g0 = () -> (u64)
inst n0 = core@1/core/call_dictionary [] []
inst n1 = app@1/main/dictionary [] []
fn f0 = n0 g0 {
b0:
 v0: u64 = call f1 g0 ()
 return v0
}
fn f1 = n1 g0 extern
"#,
        )
        .unwrap();
        let entry = InstanceKey {
            definition: DefPath::from_full("app@1/main/main").unwrap(),
            types: vec![],
            dictionaries: vec![],
        };
        let roots = exports(&entry, [(&library, true)]);
        assert!(roots.contains(&hirsel_llvm_lazy::core::symbol(
            &library,
            hirsel_core_ir::FuncId(1)
        )));
        assert!(!roots.contains(&hirsel_llvm_lazy::core::symbol(
            &library,
            hirsel_core_ir::FuncId(0)
        )));
    }
}
