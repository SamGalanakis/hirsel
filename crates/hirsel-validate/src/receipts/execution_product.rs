//! Source acceptance binds an input identity to exact product bytes. This is
//! separate from V's proof that a body is valid in a declaration universe.
use super::*;
use hirsel_artifact_store::Reference;

pub(super) const DOMAIN: Domain = Domain {
    kind: 0x5600_0009,
    schema: 1,
};

/// Independently observed compiler inputs. These identities do not grant V
/// authority. In particular, `imports` includes implementations and private
/// representations, rather than just public signature fingerprints.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExecutionProductInputs {
    pub namespace: ContentHash,
    pub imports: ContentHash,
    pub compiler: ContentHash,
    pub policy: ContentHash,
    pub options: ContentHash,
    pub entry: DefPath,
}
impl ExecutionProductInputs {
    pub(super) fn key(&self, product: Reference) -> Option<ContentHash> {
        let bytes = bincode::serialize(&(self, product)).ok()?;
        Some(ContentHash::of_parts(
            "hirsel.validate.execution-product-binding.v1",
            [bytes.as_slice()],
        ))
    }
}

/// A local source-to-product binding. It deliberately grants no permission to
/// execute a decoded body: the importer must still run V against its current
/// declaration inputs. Deserializing producer bytes cannot mint this value.
pub struct ExecutionProductBinding {
    pub(super) inputs: ExecutionProductInputs,
    pub(super) product: Reference,
    pub(super) source_check: Reference,
}
impl ExecutionProductBinding {
    pub fn inputs(&self) -> &ExecutionProductInputs {
        &self.inputs
    }
    pub fn product(&self) -> Reference {
        self.product
    }
    pub fn source_check(&self) -> Reference {
        self.source_check
    }
}

#[derive(Serialize, Deserialize)]
pub(super) struct BindingRecord {
    pub(super) inputs: ExecutionProductInputs,
    pub(super) product: Reference,
    pub(super) source_check: Reference,
}

impl VerifierCache {
    /// The source-check coordinator calls this only after its Check/Run join
    /// accepts the same immutable input snapshot. Ordinary producer archives
    /// cannot populate the verifier's private associations.
    pub fn publish_execution_product(
        &self,
        inputs: &ExecutionProductInputs,
        product: Reference,
        source_check: Reference,
    ) -> Option<()> {
        let key = inputs.key(product)?;
        self.publish(
            key,
            DOMAIN,
            &BindingRecord {
                inputs: inputs.clone(),
                product,
                source_check,
            },
        );
        Some(())
    }

    /// Re-observe inputs outside the product, then restore the binding only
    /// from this verifier. A valid but substituted TIR body or constant has a
    /// different product reference and cannot inherit source acceptance.
    pub fn execution_product(
        &self,
        current: &ExecutionProductInputs,
        product: Reference,
    ) -> Option<ExecutionProductBinding> {
        let record: BindingRecord = self.load(current.key(product)?, DOMAIN)?;
        if record.inputs != *current || record.product != product {
            return None;
        }
        Some(ExecutionProductBinding {
            inputs: record.inputs,
            product,
            source_check: record.source_check,
        })
    }
}
