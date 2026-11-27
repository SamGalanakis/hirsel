//! Versioned structural export of a linked checked module for the Lean consumer.
//! The wire keeps linked row numbers and pools. FORM text and PROG-10 each
//! project a different part of this data and can be read by their own tools.

use hirsel_core_ids::ContentHash;
use hirsel_core_ir::normal::{self, FunctionAnnex};
use hirsel_core_ir::release_recipe::Launch;
use hirsel_core_ir::source::SourceMap;
use hirsel_core_ir::{FuncId, Identity, Inst, LayoutQuery, Module, text, wf};
use hirsel_core_layout::{RecipeRow, Target};
use serde::{Deserialize, Serialize};
use std::io::Write;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
enum Format {
    #[serde(rename = "hirsel-checked-coreir-v1")]
    V1,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
enum ByteOrder {
    Little,
}

/// Recipes are in the linked module's type order. Their schema is versioned
/// with the snapshot, independently of the PROG-10 normal form.
#[derive(Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Layout {
    target: String,
    byte_order: ByteOrder,
    recipes: Vec<RecipeRow>,
}

/// One definition, at the corresponding index of the linked function table.
#[derive(Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Definition {
    instance: Identity,
    code: ContentHash,
    /// Hash of this root's PROG-10 canonical encoding.
    body: ContentHash,
    /// Sites and hints in the canonical encoding's root body order.
    source: FunctionAnnex,
    /// Physical values of raw LayoutConst instructions, in instruction order.
    layout_constants: Vec<(usize, u64)>,
}

#[derive(Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    normal_version: u16,
    text: ContentHash,
    /// Identifies the layout used by every row in definitions.
    layout: ContentHash,
    definitions: Vec<Option<Definition>>,
}

#[derive(Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Snapshot {
    format: Format,
    roots: Vec<FuncId>,
    pub(super) module: Module,
    external_call_sites: bool,
    law_observation: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    store_teardown: bool,
    /// A coverage build's scope (`docs/coverage.md`); absent otherwise, so a
    /// plain snapshot keeps its bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    coverage: Option<hirsel_core_ir::coverage::Scope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    coverage_rebound: Option<hirsel_core_ir::coverage::Rebound>,
    /// Configured release launch, when one was supplied. A source extraction has
    /// no configured launch or live grant handles.
    launch: Option<Launch>,
    layout: Layout,
    item_sites: Vec<u64>,
    manifest: Manifest,
}

impl Snapshot {
    /// Capture the linked module before the native-only transformations.
    pub(super) fn capture(
        module: Module,
        roots: Vec<FuncId>,
        target: Target,
        launch: Option<Launch>,
        sources: &SourceMap,
    ) -> Result<Self, String> {
        wf::module(&module)?;
        if roots.is_empty()
            || roots.iter().any(|root| {
                module
                    .functions
                    .get(root.index())
                    .is_none_or(|function| function.body.is_none())
            })
        {
            return Err("snapshot roots must name defined functions".into());
        }
        if launch
            .as_ref()
            .is_some_and(|launch| roots.len() != 1 || launch.target != target.triple())
        {
            return Err("configured launch needs one root and a matching target".into());
        }
        let recipes = hirsel_core_layout::compute(&module.types)
            .map_err(|error| format!("layout: {error:?}"))?;
        let layout = Layout {
            target: target.triple().into(),
            byte_order: ByteOrder::Little,
            recipes,
        };
        let item_sites = sources.item_sites(&module.items)?;
        let positions = sources.positions();
        let definitions = module
            .functions
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let instructions = &row.body.as_ref()?.insts;
                let function = FuncId(index as u32);
                let (body, source) = normal::encode_with_root_annex(&module, function, &positions);
                let layout_constants = instructions
                    .iter()
                    .enumerate()
                    .filter_map(|(index, instruction)| match instruction {
                        Inst::LayoutConst { query, .. } if *query != LayoutQuery::Target => {
                            Some((index, query.native(&module.types, &layout.recipes)))
                        }
                        _ => None,
                    })
                    .collect();
                Some(Definition {
                    instance: module.identity(row.instance),
                    code: module.code_identity(function),
                    body: body.hash(),
                    source,
                    layout_constants,
                })
            })
            .collect();
        let layout_bytes = serde_json::to_vec(&layout).map_err(|error| error.to_string())?;
        let manifest = Manifest {
            normal_version: normal::VERSION,
            text: ContentHash::of_bytes(text::print(&module).as_bytes()),
            layout: ContentHash::of_parts("hirsel.snapshot-layout.v1", [&layout_bytes[..]]),
            definitions,
        };
        Ok(Self {
            format: Format::V1,
            roots,
            external_call_sites: module.external_call_sites,
            law_observation: module.law_observation,
            store_teardown: module.store_teardown,
            coverage: module.coverage.clone(),
            coverage_rebound: module.coverage_rebound.as_deref().cloned(),
            module,
            launch,
            layout,
            item_sites,
            manifest,
        })
    }

    pub(super) fn write(&self, output: impl Write) -> serde_json::Result<()> {
        serde_json::to_writer(output, self)
    }

    /// Read the structural wire. Compare with the checked input before using an
    /// external snapshot as a proof input. Deserialization alone checks schema.
    #[cfg(test)]
    pub(super) fn read(input: impl std::io::Read) -> serde_json::Result<Self> {
        let mut snapshot: Self = serde_json::from_reader(input)?;
        snapshot.module.external_call_sites = snapshot.external_call_sites;
        snapshot.module.law_observation = snapshot.law_observation;
        snapshot.module.store_teardown = snapshot.store_teardown;
        snapshot.module.coverage = snapshot.coverage.clone();
        snapshot.module.coverage_rebound =
            snapshot.coverage_rebound.clone().map(std::sync::Arc::new);
        Ok(snapshot)
    }

    /// Print decoded IR with the independent FORM printer.
    #[cfg(test)]
    pub(super) fn print(&self) -> String {
        text::print(&self.module)
    }
}

#[cfg(test)]
mod tests;
