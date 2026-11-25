use crate::{CheckProduct, NativeStop};
use hirsel_core_ids::DefPath;
use hirsel_core_ir::{Callee, Inst, Module, Param, PlaceTy, Signature};
use hirsel_core_layout::law;
use std::collections::{BTreeMap, BTreeSet};

type Locations = BTreeMap<String, (DefPath, u32)>;

#[derive(Debug, Default, serde::Deserialize, serde::Serialize)]
pub(in crate::test_run) struct Sites(BTreeMap<String, Locations>);

impl Sites {
    pub(super) fn get(&self, callee: &str) -> Option<&Locations> {
        self.0.get(callee)
    }

    pub(in crate::test_run) fn for_test(
        product: &CheckProduct,
        test: &DefPath,
        limit: usize,
    ) -> Result<Self, NativeStop> {
        let reached = product.reachable(test, limit)?;
        let mut by_definition = BTreeMap::<(u64, DefPath), BTreeSet<u32>>::new();
        for identity in reached.require()? {
            let instance = reached.materialized.get(identity);
            let module = instance.ir()?;
            for body in module.functions.iter().filter_map(|f| f.body.as_ref()) {
                for (inst, anchor) in body.insts.iter().zip(&body.inst_anchors) {
                    let (Inst::Call { call }, Some(anchor)) = (inst, anchor) else {
                        continue;
                    };
                    let call = &body.calls[call.index()];
                    let callee = match call.callee {
                        Callee::Direct(id) => hirsel_core_ir::law::callee(
                            &module.instances[module.functions[id.index()].instance.index()]
                                .definition,
                        ),
                        Callee::Indirect(_) => {
                            indirect(module, &module.signatures[call.signature.index()])
                        }
                        Callee::Runtime(_) => None,
                    };
                    if let Some(callee) = callee {
                        by_definition
                            .entry((callee, module.items[anchor.item.index()].clone()))
                            .or_default()
                            .insert(anchor.start);
                    }
                }
            }
        }
        let items = by_definition
            .keys()
            .map(|(_, path)| path.clone())
            .collect::<Vec<_>>();
        let sources = product.native_sources(&items)?;
        let positions = sources.positions();
        let mut result = Self::default();
        for ((callee, definition), offsets) in by_definition {
            let locations = result
                .0
                .entry(law::CALLEES[callee as usize].to_owned())
                .or_default();
            for (ordinal, offset) in (1u32..).zip(offsets) {
                if let Some((file, line, column)) = positions.position(&definition, offset) {
                    locations.insert(
                        format!("{}:{line}:{column}", file.path),
                        (definition.clone(), ordinal),
                    );
                }
            }
        }
        Ok(result)
    }
}

fn indirect(module: &Module, signature: &Signature) -> Option<u64> {
    let dest = signature
        .dest
        .map(|ty| module.types[ty.index()].name.as_str());
    if matches!(signature.params.get(1), Some(Param::Address { place: PlaceTy::Declared(ty), .. })
        if module.types[ty.index()].name == "std@1/model/Bounds")
    {
        return match (dest, signature.params.len()) {
            (Some("std@1/model/Outcome"), 2) => Some(law::MODEL_EXPLORE),
            (_, 2) => Some(law::MODEL_CHECK),
            (Some(dest), 4) if dest.starts_with("std@1/model/Conformance[") => {
                Some(law::MODEL_CONFORMS)
            }
            (_, 4) => Some(law::MODEL_CONFORMS_CHECK),
            _ => None,
        };
    }
    let config = signature.params.iter().position(|param| {
        matches!(param, Param::Address { place: PlaceTy::Declared(ty), .. }
            if module.types[ty.index()].name == "std@1/prop/Config")
    })?;
    match (dest, config) {
        (Some("std@1/prop/AllOutcome"), 0) => Some(law::ALL),
        (Some("std@1/prop/Outcome"), 0) => Some(law::SEARCH),
        (_, 0) => Some(law::CHECK),
        (_, 1) => Some(law::CHECK_TASKS),
        (_, 2) => Some(law::CHECK_CORPUS),
        (_, 3) => Some(law::CHECK_TASKS_CORPUS),
        _ => None,
    }
}
