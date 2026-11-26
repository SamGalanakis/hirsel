use super::*;

impl UnitModule {
    pub(super) fn into_bound(self, annex: &Annex, identity: &Identity) -> Module {
        match self {
            Self::Decoded(module) => *module,
            Self::Encoded(bytes, values) => {
                let mut module = normal::assembly_module(&bytes);
                crate::instances::bind_values(&mut module, values.as_deref().unwrap_or_default());
                let symbol = hirsel_core_ir::symbol(identity.digest());
                bind_module(&mut module, annex, Some(&symbol));
                module
            }
        }
    }

    /// Decode an encoded unit in place. Units hold most of their rows in
    /// common: each decoded row equal to one in `rows` is that one, and each
    /// other is added to `rows`.
    pub(super) fn decode(&mut self, rows: &mut std::collections::HashSet<hirsel_core_layout::Row>) {
        let Self::Encoded(bytes, values) = self else {
            return;
        };
        let mut module = normal::assembly_module(bytes);
        crate::instances::bind_values(&mut module, values.as_deref().unwrap_or_default());
        for row in &mut module.types {
            match rows.get(row) {
                Some(held) => *row = held.clone(),
                None => {
                    rows.insert(row.clone());
                }
            }
        }
        *self = Self::Decoded(Box::new(module));
    }
}

impl Unit {
    /// The owner of the unit's private callable variants, including the values
    /// bound to its body's computed constant slots.
    pub(in crate::native) fn owner(&self) -> io::Result<ContentHash> {
        let body = (self.code.body.hash, self.code.annex.hash, &self.builtins);
        match &self.values {
            None => key("hirsel.recipe-release-body.v1", &body),
            Some(values) => key("hirsel.recipe-release-body.v2", &(body, values)),
        }
    }
}

/// Prepare a release std unit from the same canonical body and computed
/// constant bindings as a program's release publication.
pub(in crate::native) fn prepare_std_unit(
    instance: &crate::instances::InstanceBody,
    sources: &hirsel_core_ir::source::SourcePositions<'_>,
) -> io::Result<Unit> {
    let (annex, module) = instance.prepare_assembly(sources)?;
    let body = ObjectRef {
        hash: instance.body,
        len: instance.code.len() as u64,
    };
    let bytes = annex.encode();
    let code = BodyRef {
        body,
        annex: ObjectRef {
            hash: ObjectKind::Annex.hash(&bytes),
            len: bytes.len() as u64,
        },
    };
    Ok(Unit {
        code,
        module: UnitModule::Decoded(Box::new(module)),
        values: instance.values.clone(),
        annex,
        builtins: instance_builtins(instance),
        memo: None,
        coverage: None,
    })
}

pub(super) fn instance_builtins(instance: &crate::instances::InstanceBody) -> Vec<u32> {
    instance
        .slots
        .iter()
        .filter_map(|identity| builtin_semantics_number(&identity.definition))
        .collect()
}

/// A reached instance's unit, as a recipe names its body and annex.
pub(super) fn prepare_unit(
    original: &Module,
    function: FuncId,
    sources: &hirsel_core_ir::source::SourcePositions<'_>,
) -> Unit {
    let (body, annex, module) = normal::prepare_assembly(original, function, sources);
    unit(original, body, annex, module)
}

fn unit(original: &Module, body: normal::EncodedBody, annex: Annex, module: Module) -> Unit {
    let builtins = body
        .slots
        .iter()
        .map(|slot| slot.identity(original))
        .filter_map(|identity| builtin_semantics_number(&identity.definition))
        .collect();
    let code = BodyRef {
        body: ObjectRef {
            hash: ObjectKind::Body.hash(&body.bytes),
            len: body.bytes.len() as u64,
        },
        annex: {
            let bytes = annex.encode();
            ObjectRef {
                hash: ObjectKind::Annex.hash(&bytes),
                len: bytes.len() as u64,
            }
        },
    };
    Unit {
        code,
        module: UnitModule::Decoded(Box::new(module)),
        values: None,
        annex,
        builtins,
        memo: None,
        coverage: None,
    }
}
