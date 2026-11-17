//! Release composition consumes the existing reachable/materialized products.
//! The release unit is the whole program: every reached body, the program's and
//! each library's alike, is an internal definition of one LLVM module that the
//! whole-program pipeline optimises. Code generation then runs per part, one
//! per out-of-line definition, and a part's object is keyed by the part's
//! bitcode: an unchanged definition is generated once, whichever program and
//! build reached it.
use super::*;
use serde::{Deserialize, Serialize};

/// The release unit's key: its inputs and the role facts each unit consumed.
fn unit_key(
    inputs: &impl serde::Serialize,
    consumed: &[&[hirsel_core_ir::roles::BodyFacts]],
) -> io::Result<ContentHash> {
    crate::native::key(
        "hirsel.core-release-unit.v4",
        &(hirsel_core_ir::roles::FACTS_SCHEMA, consumed, inputs),
    )
}

/// A part's key: the code generator (`generator`: compiler, libLLVM, target,
/// debug lines) and the part's bitcode, which is all a part's object is a
/// function of.
fn part_key(generator: &impl serde::Serialize, bitcode: &[u8]) -> io::Result<ContentHash> {
    crate::native::key(
        "hirsel.core-release-part.v1",
        &(generator, ContentHash::of_bytes(bitcode)),
    )
}

/// The release unit's record: its parts in link order, each by its key and
/// whether it is in the shared tier.
#[derive(Serialize, Deserialize)]
struct Parts {
    schema: u32,
    parts: Vec<(bool, ContentHash)>,
}
const PARTS_SCHEMA: u32 = 1;
/// Whether a stored payload is a release unit's record.
#[cfg(test)]
pub(crate) fn is_unit_record(payload: &[u8]) -> bool {
    decode::<Parts>(payload).is_some_and(|r| r.schema == PARTS_SCHEMA)
}

/// A part of a monomorphic core or std instance carries no program type, so
/// its object goes to the user's shared cache; every other part (generic and
/// closure-bound instances, glue, the program's own code) to the store.
fn shared_symbols(reached: &[Identity]) -> std::collections::HashSet<String> {
    reached
        .iter()
        .filter(|i| {
            i.dictionaries.is_empty()
                && i.closures.is_empty()
                && (i.definition.0.package.name_is("core") || i.definition.0.package.name_is("std"))
        })
        .map(Identity::symbol)
        .collect()
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
        // Exact Step planning needs the same helper bodies LLVM will optimize.
        // Resolve the existing unit tables before lowering, as the evaluator does.
        let entry_identity = reached
            .iter()
            .find(|i| &i.definition == entry)
            .expect("reached entry");
        let roots = modules
            .iter()
            .enumerate()
            .map(|(unit, m)| {
                if let Some(identity) = reached.get(unit) {
                    return (identity.clone(), materialized.get(identity).function);
                }
                let (i, f) = m
                    .functions
                    .iter()
                    .enumerate()
                    .find(|(_, f)| f.body.is_some())
                    .expect("reached definition");
                (m.identity(f.instance), hirsel_core_ir::FuncId(i as u32))
            })
            .collect::<Vec<_>>();
        let units = modules
            .iter()
            .zip(&roots)
            .map(
                |(module, (identity, function))| hirsel_core_materialize::Unit {
                    identity,
                    module,
                    function: *function,
                },
            )
            .collect::<Vec<_>>();
        let (joined, _) =
            hirsel_core_materialize::join(&units, entry_identity).map_err(|message| {
                NativeStop::Emit {
                    definition: entry.clone(),
                    message,
                }
            })?;
        modules = vec![Cow::Owned(joined)];
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
        let target = hirsel_core_layout::Target::from_triple(&options.target)
            .expect("supported native target");
        let generator = (compiler, llvm, &options.target, options.debug_lines);
        let unit = unit_key(
            &(
                &generator,
                options.logical_charge_report.is_some(),
                !hirsel_llvm_lazy::core::facts_enabled(),
                runtime,
                instance,
                &refs,
            ),
            &consumed.iter().map(Vec::as_slice).collect::<Vec<_>>(),
        )?;
        let shared = Store::new(&crate::PackageProduct::default_cache().join("release-parts"))?;
        let tier = |in_shared: bool| if in_shared { &shared } else { store };
        let emit = |message: String| NativeStop::Emit {
            definition: entry.clone(),
            message,
        };
        // An unchanged rebuild reads the unit's record and every part it names.
        let mut held = Vec::new();
        if let Some(record) = store.get(unit)?.and_then(|b| decode::<Parts>(&b))
            && record.schema == PARTS_SCHEMA
        {
            for &(in_shared, key) in &record.parts {
                match crate::native_object::load(tier(in_shared), key)? {
                    Some((bare, object, file)) => {
                        held.push((tier(in_shared), bare, object, file, false))
                    }
                    None => break,
                }
            }
            if held.len() != record.parts.len() {
                held.clear();
            }
        }
        if held.is_empty() {
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
            let symbols = shared_symbols(reached);
            let mut parts = Vec::new();
            let mut failure = None;
            let objects = hirsel_llvm_lazy::emit_program_parts(
                &units,
                &[instance.symbol()],
                target,
                &mut |name, bitcode| {
                    let in_shared = symbols.contains(name.get(..65).unwrap_or(name));
                    let loaded = part_key(&generator, bitcode).and_then(|key| {
                        parts.push((in_shared, key));
                        crate::native_object::load(tier(in_shared), key)
                    });
                    match loaded {
                        Ok(Some(hit)) => {
                            held.push((tier(in_shared), hit.0, hit.1, hit.2, false));
                            true
                        }
                        Ok(None) => false,
                        Err(error) => {
                            failure.get_or_insert(error);
                            true
                        }
                    }
                },
            )
            .map_err(emit)?;
            if let Some(error) = failure {
                return Err(error.into());
            }
            // Objects generated now join the hits in part order.
            let mut hits = std::mem::take(&mut held).into_iter();
            for (&(in_shared, key), object) in parts.iter().zip(objects) {
                held.push(match object {
                    None => hits.next().expect("a reused part was loaded"),
                    Some(bytes) => {
                        let object = hirsel_llvm_lazy::core::finish_native_object(
                            bytes,
                            key,
                            options.debug_lines,
                        )
                        .map_err(emit)?;
                        let (bare, file) =
                            crate::native_object::publish(tier(in_shared), key, &object)?;
                        (tier(in_shared), bare, object, file, true)
                    }
                });
            }
            store.put_keyed(
                unit,
                &encode(&Parts {
                    schema: PARTS_SCHEMA,
                    parts,
                })?,
            )?;
        }
        let mut private_variants = modules
            .iter()
            .map(|m| {
                m.code_variants
                    .keys()
                    .filter(|f| m.functions[f.index()].body.is_some())
                    .count()
            })
            .sum();
        Ok(held
            .into_iter()
            .map(|(store, bare, object, file, compiled)| ObjectProduct {
                private_variants: std::mem::take(&mut private_variants),
                instance: instance.clone(),
                path: crate::native_object::path(store, bare.address),
                identity: bare.address,
                check: bare.check,
                held: Some(file),
                debug: object.debug,
                debug_record: None,
                compiled,
            })
            .collect())
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
