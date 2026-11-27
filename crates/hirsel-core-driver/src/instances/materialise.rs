//! An instance's materialisation: its retained code and the values its constant
//! slots bind (PROG-10), before the walk's bookkeeping.
use super::*;

/// The value of each constant a module's slots name (PROG-10), by path.
pub(crate) type Bindings = Option<Arc<[(DefPath, hirsel_core_ir::StaticValue)]>>;

/// Bind `module`'s constant slots to `values`, which name each one.
pub(crate) fn bind_values(module: &mut Module, values: &[(DefPath, hirsel_core_ir::StaticValue)]) {
    if values.is_empty() {
        return;
    }
    module
        .bind_constants(|path| {
            values
                .iter()
                .find(|(p, _)| p == path)
                .map(|(_, v)| v.clone())
        })
        .unwrap_or_else(|path| panic!("constant {path} has no value bound"));
}

pub(super) enum MaterializedModule {
    Decoded(Module),
    Recipe {
        sites: hirsel_core_ir::normal::RetainedSites,
        decoded: std::sync::OnceLock<Module>,
    },
}

impl MaterializedModule {
    pub(super) fn ir(&self, code: &[u8], values: &Bindings) -> &Module {
        match self {
            Self::Decoded(module) => module,
            Self::Recipe { sites, decoded } => decoded.get_or_init(|| {
                let mut module = sites.restore(code);
                bind_values(&mut module, values.as_deref().unwrap_or_default());
                module
            }),
        }
    }
}

/// An instance as the walk materialises it, before its walk-specific
/// bookkeeping: a function of its identity, its definition's published
/// record and its arguments' closures, which its identity names by hash.
#[derive(Clone)]
pub(crate) struct Materialised {
    pub(super) module: Option<Arc<MaterializedModule>>,
    pub(super) function: hirsel_core_ir::FuncId,
    pub(super) children: Vec<Child>,
    pub(super) body: ContentHash,
    pub(super) code: Arc<[u8]>,
    pub(super) slots: Vec<Identity>,
    pub(super) signatures: CodeSignatures,
    pub(super) adapters: Vec<Adapter>,
    pub(super) described: Vec<Identity>,
    pub(super) selected_target: bool,
    /// The reason of a `target.unsupported` call the retained code makes (PROG-6).
    pub(super) unsupported: Option<String>,
    pub(super) values: Bindings,
}
pub(super) fn materialise(
    key: &Identity,
    product: Option<&DefinitionProduct>,
    arguments: &[&hirsel_core_ir::Closure],
    observe: &mut impl FnMut(&Identity, &'static str),
    compact_recipe: bool,
    target: hirsel_core_layout::Target,
    values: &[(DefPath, Arc<crate::comptime::Value>)],
) -> Result<Materialised, InstanceStop> {
    let input = |message: String| InstanceStop::Input {
        definition: key.definition.clone(),
        message,
    };
    let mut built = match product {
        None => {
            observe(key, "load");
            hirsel_core_materialize::materialize_glue(key, arguments, target).map_err(input)?
        }
        Some(product) => {
            // Decoded for this materialization only: holding every reached
            // body's module would add the whole program's IR to reach.
            let module = product.transient_ir().map_err(|e| input(e.to_string()))?;
            let own = Published {
                module: &module,
                function: product.check.function.unwrap(),
                template: product.check.template.as_ref(),
            };
            observe(key, "load");
            hirsel_core_materialize::materialize(&own, key, arguments, target).map_err(input)?
        }
    };
    observe(key, "materialize");
    let body = built.code.hash();
    // The constants the body names, in slot order, with their values.
    let constants = built.code.constants;
    let mut value_hashes = Vec::new();
    let bindings = if constants.is_empty() {
        None
    } else {
        let mut bindings = Vec::with_capacity(constants.len());
        value_hashes.reserve_exact(constants.len());
        for path in constants {
            let value = values
                .iter()
                .find(|(p, _)| p == &path)
                .ok_or_else(|| input(format!("constant {path} has no value")))?;
            bindings.push((path, value.1.value.clone()));
            value_hashes.push(value.1.hash);
        }
        Some(Arc::from(bindings))
    };
    let (slots, signatures, adapters) = {
        use hirsel_core_ir::normal::{self, Slot};
        let module = built.module.as_ref().expect("materialized module");
        let mut hashes = rustc_hash::FxHashMap::default();
        let mut signature = |function: FuncId| {
            let row = module.functions[function.index()].signature;
            *hashes
                .entry(row)
                .or_insert_with(|| normal::signature(module, function))
        };
        let signatures = CodeSignatures {
            own: signature(built.function),
            slots: built
                .children
                .iter()
                .map(|child| (child.identity.clone(), signature(child.function)))
                .collect(),
            values: value_hashes,
        };
        let mut children = built.children.iter();
        let slots = built
            .code
            .slots
            .iter()
            .map(|slot| match slot {
                Slot::External(_) => children
                    .next()
                    .expect("a child for each external slot")
                    .identity
                    .clone(),
                Slot::Builtin(definition) => Identity::plain(definition.clone()),
            })
            .collect();
        let adapters = module
            .functions
            .iter()
            .enumerate()
            .filter_map(|(i, function)| {
                function.body.as_ref()?;
                let key = &module.instances[function.instance.index()];
                key.definition.0.name.contains("$callable[").then(|| {
                    let code = normal::encode(module, hirsel_core_ir::FuncId(i as u32));
                    Adapter {
                        identity: module.identity(function.instance),
                        body: code.hash(),
                        slots: code
                            .slots
                            .iter()
                            .map(|slot| slot.identity(module))
                            .collect(),
                    }
                })
            })
            .collect::<Vec<_>>();
        (slots, signatures, adapters)
    };
    let described = built.code.described;
    let unsupported = built
        .module
        .as_ref()
        .and_then(|module| hirsel_core_ir::unsupported_reason(module, built.function));
    if let Some(module) = built.module.as_mut() {
        bind_values(module, bindings.as_deref().unwrap_or_default());
    }
    Ok(Materialised {
        module: built.module.map(|module| {
            Arc::new(if compact_recipe {
                MaterializedModule::Recipe {
                    sites: hirsel_core_ir::normal::RetainedSites::take(module),
                    decoded: std::sync::OnceLock::new(),
                }
            } else {
                MaterializedModule::Decoded(module)
            })
        }),
        function: built.function,
        children: built.children,
        body,
        code: built.code.bytes.into(),
        slots,
        signatures,
        adapters,
        described,
        selected_target: built.selected_target,
        unsupported,
        values: bindings,
    })
}
