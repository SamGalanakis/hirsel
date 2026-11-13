//! Snapshot-only receipts for the products an ordinary check actually required.
//! Reuse PS-9's canonical signature/TIR encoding, not transient interner ids.
use super::*;
use hirsel_proof_export::ExportInput;

#[derive(Clone)]
pub struct CheckedProducts {
    pub digest: ContentHash,
    /// Optional diagnostic export; the digest always commits to the full bytes.
    pub canonical: Option<Arc<str>>,
    /// Key bytes use ReadKey::encode: DefPaths and spellings, never DefIds.
    pub reads: BTreeMap<DefPath, BTreeMap<Vec<u8>, ContentHash>>,
    pub type_rows: u64,
    pub type_bytes: u64,
    owner: Arc<dyn Send + Sync>,
}
impl std::fmt::Debug for CheckedProducts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CheckedProducts")
            .field("digest", &self.digest)
            .field("canonical", &self.canonical)
            .field("reads", &self.reads)
            .field("type_rows", &self.type_rows)
            .field("type_bytes", &self.type_bytes)
            .finish()
    }
}
impl CheckedProducts {
    /// Keep producer and output admission alive through detached receipt clones.
    /// The token controls lifetime only and grants no verification authority.
    pub fn retain_owner(mut self, owner: Arc<dyn Send + Sync>) -> Self {
        self.owner = Arc::new((self.owner, owner));
        self
    }
    /// The same receipt without canonical text. Its copied read maps are
    /// charged to `request` before the copy and stay charged while any clone
    /// of the view is held; None charges nothing.
    pub fn digest_view(
        &self,
        request: &Arc<hirsel_engine_admission::ProcessBudget>,
    ) -> Option<Arc<Self>> {
        let charge = request.reserve(self.digest_view_bytes()?).ok()?;
        let view = Self {
            digest: self.digest,
            canonical: None,
            reads: self.reads.clone(),
            type_rows: self.type_rows,
            type_bytes: self.type_bytes,
            owner: self.owner.clone(),
        };
        Some(Arc::new(view.retain_owner(Arc::new(charge))))
    }
    /// The view's own allocations: its Arc, the owner pair and charge Arc
    /// binding its grant, and the copied read maps with their paths and keys.
    fn digest_view_bytes(&self) -> Option<u64> {
        fn arc<T>() -> Option<usize> {
            Some(
                std::alloc::Layout::new::<[usize; 2]>()
                    .extend(std::alloc::Layout::new::<T>())
                    .ok()?
                    .0
                    .pad_to_align()
                    .size(),
            )
        }
        let mut weight = hirsel_query_products::retained::MetadataWeight::default();
        weight.add(arc::<Self>()?);
        weight.add(arc::<(Arc<dyn Send + Sync>, Arc<dyn Send + Sync>)>()?);
        weight.add(arc::<hirsel_engine_admission::MemoryReservation>()?);
        weight.tree::<DefPath, BTreeMap<Vec<u8>, ContentHash>>(self.reads.len());
        for (path, reads) in &self.reads {
            weight.path(path);
            weight.tree::<Vec<u8>, ContentHash>(reads.len());
            for key in reads.keys() {
                weight.add(key.len());
            }
        }
        Some(weight.bytes())
    }
    /// Exact owner/read keys ordered by persisted bytes, independent of intern
    /// assignment and scheduling. Includes private views and negative reads.
    pub fn canonical_reads(&self) -> BTreeMap<(Vec<u8>, Vec<u8>), ContentHash> {
        self.reads
            .iter()
            .flat_map(|(owner, reads)| {
                let mut bytes = Vec::new();
                hirsel_query_products::reads::encode_def_path(&mut bytes, owner);
                reads
                    .iter()
                    .map(move |(key, value)| ((bytes.clone(), key.clone()), *value))
            })
            .collect()
    }
}

/// Canonical encoding of already settled source obligations. This formats
/// ordinary immutable data and cannot demand a query or supply V authority.
pub struct CheckedProductEncoder {
    canonical: Arc<str>,
    types: Arc<hirsel_types::TypeInterner>,
}
impl CheckedProductEncoder {
    /// Already encoded bytes from the same admitted immutable builder. This
    /// constructor carries no V or native authority.
    pub(crate) fn from_canonical(
        canonical: Arc<str>,
        types: Arc<hirsel_types::TypeInterner>,
    ) -> Self {
        Self { canonical, types }
    }
    pub fn new(input: &ExportInput) -> Result<Self, QueryError> {
        let export = hirsel_proof_export::build(input);
        if !export.skipped.is_empty() {
            return Err(QueryError::Operational(hirsel_types::OperationalFailure {
                operation: "candidate products".into(),
                message: format!("incomplete canonical products: {:?}", export.skipped),
            }));
        }
        let canonical: Arc<str> = hirsel_proof_export::encode(&export, &input.locator).into();
        Ok(Self {
            canonical,
            types: input.types.clone(),
        })
    }
    /// Bind every required read to the canonical product bytes.
    pub fn finish(
        self,
        required_reads: BTreeMap<DefPath, Arc<hirsel_query_products::reads::Reads>>,
    ) -> CheckedProducts {
        let canonical = self.canonical;
        let mut reads: BTreeMap<DefPath, BTreeMap<Vec<u8>, ContentHash>> = BTreeMap::new();
        for (path, observed) in required_reads {
            let edges = reads.entry(path).or_default();
            for (key, value) in observed.iter() {
                let mut bytes = vec![];
                key.encode(&mut bytes);
                edges.insert(bytes, *value);
            }
        }
        let mut parts = Vec::new();
        for (path, observed) in &reads {
            let mut key = vec![];
            hirsel_query_products::reads::encode_def_path(&mut key, path);
            parts.push(key);
            for (key, value) in observed {
                parts.push(key.clone());
                parts.push(value.0.to_vec());
            }
        }
        let stats = self.types.stats();
        CheckedProducts {
            digest: ContentHash::of_parts(
                "hirsel.checked-products.v1",
                std::iter::once(canonical.as_bytes()).chain(parts.iter().map(Vec::as_slice)),
            ),
            canonical: Some(canonical),
            reads,
            type_rows: stats.rows,
            type_bytes: stats.row_bytes,
            owner: Arc::new(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn checked_digest_borrows_detached_canonical_text_without_copying_its_payload() {
        let types = hirsel_types::TypeInterner::new(None);
        for width in [1, 4, 16] {
            let canonical: Arc<str> = "λ🐑canonical\n".repeat(width * 1024).into();
            let expected = ContentHash::of_parts(
                "hirsel.checked-products.v1",
                std::iter::once(canonical.as_bytes()),
            );
            let pointer = canonical.as_ptr();
            let (actual, count) = crate::test_alloc::measured(|| {
                CheckedProductEncoder::from_canonical(canonical, types.clone())
                    .finish(BTreeMap::new())
            });
            assert_eq!(actual.digest, expected);
            assert_eq!(actual.canonical.as_ref().unwrap().as_ptr(), pointer);
            assert!(
                count.peak < 1024,
                "digest copied the canonical text: {}",
                count.peak
            );
        }
    }

    #[test]
    fn detached_checked_product_clones_keep_all_output_reservations() {
        let budget = hirsel_engine_admission::ProcessBudget::new(96);
        let products = CheckedProducts {
            digest: ContentHash::of_bytes(b"receipt"),
            canonical: Some("canonical receipt".into()),
            reads: BTreeMap::new(),
            type_rows: 0,
            type_bytes: 0,
            owner: Arc::new(()),
        }
        .retain_owner(Arc::new(budget.reserve(64).unwrap()));
        let original = products.clone();
        let products = products.retain_owner(Arc::new(budget.reserve(32).unwrap()));
        let detached = products.clone();
        drop(products);
        assert_eq!(budget.reserved(), 96);
        drop(original);
        assert_eq!(budget.reserved(), 96);
        assert_eq!(detached.canonical.as_deref(), Some("canonical receipt"));
        drop(detached);
        assert_eq!(budget.reserved(), 0);
    }

    #[test]
    fn digest_views_charge_their_copied_read_maps_until_the_last_clone_drops() {
        let budget = hirsel_engine_admission::ProcessBudget::new(1 << 20);
        let mut reads = BTreeMap::new();
        reads.insert(
            DefPath::core("main"),
            BTreeMap::from([(b"key".to_vec(), ContentHash::of_bytes(b"value"))]),
        );
        let products = CheckedProducts {
            digest: ContentHash::of_bytes(b"receipt"),
            canonical: Some("canonical receipt".into()),
            reads,
            type_rows: 1,
            type_bytes: 2,
            owner: Arc::new(()),
        };
        let needed = products.digest_view_bytes().unwrap();
        let short = hirsel_engine_admission::ProcessBudget::new(needed - 1);
        assert!(products.digest_view(&short).is_none());
        assert_eq!(short.reserved(), 0);
        let view = products.digest_view(&budget).unwrap();
        assert_eq!(view.canonical, None);
        assert_eq!(view.digest, products.digest);
        assert_eq!(view.reads, products.reads);
        assert_eq!((view.type_rows, view.type_bytes), (1, 2));
        let held = view.clone();
        drop(view);
        assert_eq!(budget.reserved(), needed);
        drop(held);
        assert_eq!(budget.reserved(), 0);
    }
}
