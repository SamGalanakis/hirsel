//! Native source declarations and independently validated cached products.
use hirsel_native::query_model::*;
use hirsel_native::*;
pub mod arena;
pub mod declarations;
pub mod instance;
pub mod optimizer;
pub mod ready;
pub mod ready_source;
pub mod recipes;
pub mod source;
pub mod source_inputs;

#[cfg(test)]
hirsel_guard::install!();
#[cfg(test)]
fn fixture_product(
    store: &dyn hirsel_engine::store::ResultStore,
    address: hirsel_engine::store::Address,
    action: hirsel_engine::store::Action,
    reference: hirsel_engine::store::Reference,
) -> hirsel_engine::runtime::ProductRef {
    let budget = hirsel_engine_admission::ProcessBudget::new(64 << 20);
    let mut allowance = budget.reserve(64 << 20).unwrap();
    let archive =
        hirsel_engine::runtime::ProductArchive::open(store, reference, &mut allowance).unwrap();
    hirsel_engine::runtime::ProductRef::import_archive(address, action, archive)
}

#[cfg(test)]
fn fixture_product_bytes(
    address: hirsel_engine::store::Address,
    action: hirsel_engine::store::Action,
    domain: hirsel_engine::store::Domain,
    bytes: &[u8],
) -> hirsel_engine::runtime::ProductRef {
    use hirsel_engine::store::ResultStore;
    let store = hirsel_engine::store::MemStore::default();
    let reference = store.put_content(domain, bytes).unwrap();
    fixture_product(&store, address, action, reference)
}

use hirsel_engine::runtime::ArchiveAdmission;
use hirsel_engine::store::Reference;
use hirsel_syntax::ContentHash;
use std::sync::Arc;
pub mod discharge;
pub mod preparation;
