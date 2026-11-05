//! Native source and instance producers over validated native pipeline inputs.
use hirsel_native::query_model::*;
pub mod arena_tasks;
pub mod batch_sources;
pub mod compilation_tasks;
pub mod demand;
mod dispatch;
use hirsel_native_cache::discharge;
pub mod emission;
pub mod execution;
pub mod graph;
mod instance_operations;
pub mod instance_tasks;
pub mod link_tasks;
pub mod local;
mod ownership_closure;
pub mod ownership_compilation;
pub mod ownership_tasks;
pub mod products;
mod receipt_requests;
pub mod root_tasks;
pub mod roots;
pub mod seal_demand;
mod seal_inventory;
pub mod tasks;

#[cfg(test)]
hirsel_guard::install!();
#[cfg(test)]
fn fixture_product(
    store: &dyn hirsel_engine::store::ResultStore,
    address: hirsel_engine::store::Address,
    action: hirsel_engine::store::Action,
    reference: hirsel_engine::store::Reference,
) -> hirsel_engine_runtime::ProductRef {
    let budget = hirsel_engine_admission::ProcessBudget::new(64 << 20);
    let mut allowance = budget.reserve(64 << 20).unwrap();
    let archive =
        hirsel_engine_runtime::ProductArchive::open(store, reference, &mut allowance).unwrap();
    hirsel_engine_runtime::ProductRef::import_archive(address, action, archive)
}

#[cfg(test)]
fn fixture_product_bytes(
    address: hirsel_engine::store::Address,
    action: hirsel_engine::store::Action,
    domain: hirsel_engine::store::Domain,
    bytes: &[u8],
) -> hirsel_engine_runtime::ProductRef {
    use hirsel_engine::store::ResultStore;
    let store = hirsel_engine::store::MemStore::default();
    let reference = store.put_content(domain, bytes).unwrap();
    fixture_product(&store, address, action, reference)
}

use hirsel_engine::store::Reference;
use hirsel_engine_runtime::ArchiveAdmission;
use hirsel_native_cache::{
    arena, declarations, instance, optimizer, ready, ready_source, recipes, source_inputs,
};
use hirsel_syntax::ContentHash;
use std::sync::Arc;
