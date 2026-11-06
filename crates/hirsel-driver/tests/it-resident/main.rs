//! Shared-runtime request lifecycle controls, independent of native campaigns.
hirsel_guard::install!();

fn fixture_product(
    store: &dyn hirsel_engine_store::ResultStore,
    address: hirsel_engine_store::Address,
    action: hirsel_engine_store::Action,
    reference: hirsel_engine_store::Reference,
) -> hirsel_engine_runtime::ProductRef {
    let budget = hirsel_engine_admission::ProcessBudget::new(64 << 20);
    let mut allowance = budget.reserve(64 << 20).unwrap();
    let archive =
        hirsel_engine_runtime::ProductArchive::open(store, reference, &mut allowance).unwrap();
    hirsel_engine_runtime::ProductRef::import_archive(address, action, archive)
}

fn fixture_product_bytes(
    address: hirsel_engine_store::Address,
    action: hirsel_engine_store::Action,
    domain: hirsel_engine_store::Domain,
    bytes: &[u8],
) -> hirsel_engine_runtime::ProductRef {
    use hirsel_engine_store::ResultStore;
    let store = hirsel_engine_store::MemStore::default();
    let reference = store.put_content(domain, bytes).unwrap();
    fixture_product(&store, address, action, reference)
}

include!("modules.rs");
