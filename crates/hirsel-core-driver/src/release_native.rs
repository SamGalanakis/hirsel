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
        let refs = modules.iter().map(|m| &**m).collect::<Vec<_>>();
        let facts = hirsel_core_ir::roles::Facts::program(&refs, &[]);
        let consumed = refs
            .iter()
            .zip(&facts)
            .map(|(module, facts)| facts.consumed(module))
            .collect::<Vec<_>>();
        let mut objects = Vec::new();
        let mut libraries = Vec::new();
        for (i, ((instance, ir), imported)) in
            reached.iter().zip(&modules).zip(&imported).enumerate()
        {
            if !imported || !ir.functions.iter().any(|f| f.body.is_some()) {
                continue;
            }
            let library_store = library_store.unwrap();
            let object_key = crate::native_library::key(
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
                &[&consumed[i]],
            )?;
            let (identity, product, compiled) = if let Some((hash, product)) =
                crate::native_library::load(
                    library_store,
                    object_key,
                    hirsel_llvm_lazy::NATIVE_LIBRARY_SCHEMA,
                )? {
                (hash, product, false)
            } else {
                let native = hirsel_llvm_lazy::compile_library(
                    ir,
                    &facts[i],
                    hirsel_core_layout::Target::from_triple(&options.target)
                        .expect("supported native target"),
                    options.logical_charge_report.is_some(),
                    options.debug_lines,
                    object_key,
                )
                .map_err(|message| NativeStop::Emit {
                    definition: instance.definition.clone(),
                    message,
                })?;
                let identity = crate::native_library::publish(
                    library_store,
                    object_key,
                    hirsel_llvm_lazy::NATIVE_LIBRARY_SCHEMA,
                    &native,
                )?;
                (identity, native, true)
            };
            objects.push(ObjectProduct {
                private_variants: 0,
                instance: instance.clone(),
                identity,
                bytes: product.object.bytes,
                debug: product.object.debug,
                compiled,
            });
            libraries.push((identity, product.bitcode, product.summary));
        }
        let instance = reached
            .iter()
            .find(|i| &i.definition == entry)
            .expect("reached entry");
        // Imported bodies and their facts are covered by the one atomic native
        // product identity. Only application-owned CoreIR is encoded again.
        let owned = modules
            .iter()
            .zip(&imported)
            .enumerate()
            .filter_map(|(i, (module, imported))| (!imported).then_some((i, &**module)))
            .collect::<Vec<_>>();
        let own_modules = owned.iter().map(|(_, module)| *module).collect::<Vec<_>>();
        let own_facts = owned
            .iter()
            .map(|(i, _)| consumed[*i].as_slice())
            .collect::<Vec<_>>();
        let dependencies = libraries
            .iter()
            .map(|(identity, _, summary)| (identity, summary))
            .collect::<Vec<_>>();
        let object_key = crate::native_library::key(
            "hirsel.core-release-unit.v2",
            &(
                compiler_identity()?,
                &options.target,
                options.debug_lines,
                options.logical_charge_report.is_some(),
                !hirsel_llvm_lazy::core::facts_enabled(),
                runtime,
                instance,
                &own_modules,
                &dependencies,
            ),
            &own_facts,
        )?;
        let (identity, object, compiled) =
            if let Some((hash, object)) = crate::native_object::load(store, object_key)? {
                (hash, object, false)
            } else {
                let text = owned
                    .iter()
                    .map(|(i, ir)| {
                        hirsel_llvm_lazy::core::emit_ir_relocatable_with_role_facts_for_target(
                            ir,
                            hirsel_core_layout::Target::from_triple(&options.target)
                                .expect("supported native target"),
                            options.logical_charge_report.is_some(),
                            options.debug_lines,
                            &facts[*i],
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|e| NativeStop::Emit {
                        definition: entry.clone(),
                        message: e.to_string(),
                    })?;
                // Preserve the producer's unit order. Declaration-only imported
                // modules need neither source emission nor a bitcode load.
                let mut texts = text.iter();
                let mut imports = libraries.iter();
                let units = modules
                    .iter()
                    .zip(&imported)
                    .filter_map(|(module, imported)| {
                        if !imported {
                            Some(hirsel_llvm_lazy::Unit::Text {
                                ir: texts.next().unwrap(),
                                imported: false,
                            })
                        } else if module.functions.iter().any(|f| f.body.is_some()) {
                            let (_, bitcode, summary) = imports.next().unwrap();
                            Some(hirsel_llvm_lazy::Unit::Import { bitcode, summary })
                        } else {
                            None
                        }
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
            bytes: object.bytes,
            debug: object.debug,
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
