use hirsel_artifact_store::Reference;

/// Checked content and retained-allocation admission for immutable products.
/// This grants no query lookup, publication or validator authority.
pub trait ProductBacking: Send + Sync {
    fn get_content(&self, reference: Reference) -> Option<hirsel_artifact_store::Bytes>;
    fn admit_product_memory(&self, bytes: u64) -> Option<std::sync::Arc<dyn Send + Sync>>;
}
