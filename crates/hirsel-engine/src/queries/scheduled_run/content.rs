//! Immutable semantic tokens shared by the node and its named-content view.
use super::*;

#[derive(Clone)]
struct Bytes {
    value: Arc<Tokens>,
    decode_bytes: u64,
}
enum Tokens {
    Live(Vec<Vec<u8>>),
    Spilled(Reference),
}
impl Bytes {
    fn new(allowance: u64, value: Vec<Vec<u8>>) -> Result<Self, QueryError> {
        let mut weight = hirsel_query_products::retained::MetadataWeight::default();
        weight.add(std::mem::size_of::<Vec<Vec<u8>>>());
        weight.vec(&value, |weight, field| weight.flat_vec(field));
        let decode_bytes = weight
            .bytes()
            .checked_mul(2)
            .ok_or(QueryError::NotImplemented)?;
        if decode_bytes > allowance {
            return Err(operational("Run content exceeds scratch admission"));
        }
        Ok(Self {
            value: Arc::new(Tokens::Live(value)),
            decode_bytes,
        })
    }
    fn load(
        &self,
        store: &dyn ResultStore,
        reservation: &hirsel_engine_admission::MemoryReservation,
        allowance: u64,
    ) -> Result<Vec<Vec<u8>>, QueryError> {
        if self.decode_bytes > allowance {
            return Err(operational("Run content exceeds decode admission"));
        }
        match &*self.value {
            Tokens::Live(value) => Ok(value.clone()),
            Tokens::Spilled(reference) => {
                let bytes = store
                    .get_content(*reference)
                    .ok_or(QueryError::NotImplemented)?;
                if reference.domain != INPUT_CONTENT
                    || INPUT_CONTENT.reference(&bytes) != Some(*reference)
                {
                    return Err(operational(
                        "spilled Run content differs from its descriptor",
                    ));
                }
                let requirement = hirsel_product_codec::DecodeRequirement::<
                    hirsel_product_codec::RunContent,
                >::open(bytes)
                .ok_or(QueryError::NotImplemented)?;
                if requirement.required_bytes() > allowance {
                    return Err(operational("Run content exceeds decode admission"));
                }
                requirement
                    .decode_content(reservation)
                    .ok_or(QueryError::NotImplemented)
            }
        }
    }
    fn spill(&mut self, queries: &FrontRunQueries) -> Result<(), QueryError> {
        let Tokens::Live(value) = &*self.value else {
            return Ok(());
        };
        let bytes: hirsel_artifact_store::Bytes = bincode::serialize(value)
            .map_err(|_| QueryError::NotImplemented)?
            .into();
        let requirement =
            hirsel_product_codec::DecodeRequirement::<hirsel_product_codec::RunContent>::open(
                bytes.clone(),
            )
            .ok_or(QueryError::NotImplemented)?;
        if requirement.required_bytes() > queries.weight.scratch {
            return Err(operational("Run spill exceeds scratch admission"));
        }
        let reference = queries
            .store
            .put_content(INPUT_CONTENT, &bytes)
            .ok_or(QueryError::NotImplemented)?;
        self.decode_bytes = requirement.required_bytes();
        self.value = Arc::new(Tokens::Spilled(reference));
        Ok(())
    }
    fn weight(&self, weight: &mut hirsel_query_products::retained::MetadataWeight) {
        weight.arc(&self.value);
        if let Tokens::Live(value) = &*self.value {
            weight.vec(value, |weight, field| weight.flat_vec(field));
        }
    }
}
enum Header {
    Body,
    Named {
        source: Option<Bytes>,
        reads: Box<[DefId]>,
    },
}
pub(super) struct Content {
    header: Header,
    body: bool,
    references: Box<[DefId]>,
}
impl Content {
    pub(super) fn new(
        queries: &FrontRunQueries,
        content: DefinitionContent,
        node: &Node,
    ) -> Result<Self, QueryError> {
        let snapshot = queries.lower.signatures();
        let references = hirsel_proof_export::references(snapshot.types(), &content)
            .iter()
            .map(DefId::intern)
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let (header, body) = match content {
            DefinitionContent::Body { tir, .. } => (Header::Body, tir.is_some()),
            DefinitionContent::Named { source, reads, .. } => (
                Header::Named {
                    source: if source.is_empty() {
                        None
                    } else if matches!(&*node.content.value, Tokens::Live(value) if source == *value)
                    {
                        Some(node.content.clone())
                    } else {
                        return Err(operational(
                            "Run named content differs from its semantic node",
                        ));
                    },
                    reads: reads
                        .iter()
                        .map(DefId::intern)
                        .collect::<Vec<_>>()
                        .into_boxed_slice(),
                },
                false,
            ),
        };
        Ok(Self {
            header,
            body,
            references,
        })
    }
    pub(super) fn reference_bytes(
        &self,
        weight: &mut hirsel_query_products::retained::MetadataWeight,
    ) {
        weight.add(
            self.references
                .len()
                .saturating_mul(std::mem::size_of::<DefPath>()),
        );
        for id in &self.references {
            weight.path(id.path());
        }
    }
    pub(super) fn references(&self) -> Vec<DefPath> {
        self.references.iter().map(|id| id.path().clone()).collect()
    }
    pub(super) fn spilled_node(&mut self, node: &Node) {
        if let Header::Named {
            source: Some(source),
            ..
        } = &mut self.header
        {
            *source = node.content.clone();
        }
    }
    pub(super) fn has_body(&self) -> bool {
        self.body
    }
    pub(super) fn load(
        &self,
        queries: &FrontRunQueries,
        path: &DefPath,
        allowance: u64,
    ) -> Result<DefinitionContent, QueryError> {
        let input = queries.input(path)?;
        let mut weight = hirsel_query_products::retained::MetadataWeight::default();
        weight.add(std::mem::size_of::<DefinitionContent>());
        let working = match &self.header {
            Header::Body if self.body => queries.lower.settled_definition_decode_bytes(path)?,
            Header::Body => 0,
            Header::Named { source, reads, .. } => {
                weight.add(
                    reads
                        .len()
                        .checked_mul(std::mem::size_of::<DefPath>())
                        .ok_or(QueryError::NotImplemented)?,
                );
                for read in reads {
                    weight.path(read.path());
                }
                source.as_ref().map_or(0, |source| source.decode_bytes)
            }
        };
        if weight
            .bytes()
            .checked_mul(2)
            .and_then(|size| size.checked_add(working))
            .is_none_or(|size| size > allowance)
        {
            return Err(operational("Run content exceeds scratch admission"));
        }
        Ok(match &self.header {
            Header::Body => DefinitionContent::Body {
                signature: if matches!(input.signature.kind, SignatureKind::Type(_)) {
                    input.owner_signature.clone()
                } else {
                    input.signature.clone()
                },
                tir: if self.body {
                    Some(queries.lower.settled_definition(path)?)
                } else {
                    None
                },
            },
            Header::Named { source, reads } => DefinitionContent::Named {
                signature: input.signature.clone(),
                source: source
                    .as_ref()
                    .map(|source| {
                        source.load(&*queries.store, &queries.ownership._allowance, allowance)
                    })
                    .transpose()?
                    .unwrap_or_default(),
                reads: reads.iter().map(|id| id.path().clone()).collect(),
            },
        })
    }
    pub(super) fn weight(&self, weight: &mut hirsel_query_products::retained::MetadataWeight) {
        weight.add(std::mem::size_of_val(&*self.references));
        if let Header::Named { reads, .. } = &self.header {
            weight.add(std::mem::size_of_val(&**reads));
        }
    }
}
pub(super) struct Node {
    content: Bytes,
    pub dependencies: Box<[DefId]>,
    pub observations: Box<[(u32, ContentHash, ContentHash)]>,
}
pub(super) struct CanonicalNode<'a> {
    pub identity: String,
    pub content: std::borrow::Cow<'a, Vec<Vec<u8>>>,
    pub dependencies: Vec<String>,
}
impl Node {
    pub(super) fn new(
        queries: &FrontRunQueries,
        content: Vec<Vec<u8>>,
        dependencies: &BTreeSet<DefPath>,
        observations: Vec<(u32, ContentHash, ContentHash)>,
    ) -> Result<Self, QueryError> {
        Ok(Self {
            content: Bytes::new(queries.weight.scratch, content)?,
            observations: observations.into_boxed_slice(),
            dependencies: dependencies
                .iter()
                .map(DefId::intern)
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        })
    }
    pub(super) fn canonical<'a>(
        &'a self,
        path: &DefPath,
        store: &dyn ResultStore,
        reservation: &hirsel_engine_admission::MemoryReservation,
        allowance: u64,
    ) -> Result<CanonicalNode<'a>, QueryError> {
        let mut dependencies: Vec<_> = self
            .dependencies
            .iter()
            .map(|id| id.path().to_string())
            .collect();
        dependencies.push("$hirsel/runtime@1".into());
        let content = match &*self.content.value {
            Tokens::Live(value) => std::borrow::Cow::Borrowed(value),
            Tokens::Spilled(_) => {
                std::borrow::Cow::Owned(self.content.load(store, reservation, allowance)?)
            }
        };
        Ok(CanonicalNode {
            identity: path.to_string(),
            content,
            dependencies,
        })
    }
    pub(super) fn spill(&mut self, queries: &FrontRunQueries) -> Result<(), QueryError> {
        self.content.spill(queries)
    }
    pub(super) fn decode_bytes(&self, path: &DefPath) -> Option<u64> {
        // Bincode's length-framed path bounds Display's separators too. A
        // growing String may retain up to twice its final formatted length.
        let path_bytes = |path: &DefPath| bincode::serialized_size(path).ok()?.checked_mul(2);
        let mut bytes = self
            .content
            .decode_bytes
            .checked_add(std::mem::size_of::<hirsel_run_schema::ProgramNode>() as u64)?
            .checked_add(path_bytes(path)?.checked_mul(2)?)?;
        bytes = bytes.checked_add(
            ((self.dependencies.len() + 1) * 2 * std::mem::size_of::<String>()) as u64,
        )?;
        for dependency in &self.dependencies {
            bytes = bytes.checked_add(path_bytes(dependency.path())?)?;
        }
        bytes.checked_add(2 * "$hirsel/runtime@1".len() as u64)
    }
    pub(super) fn program(
        &self,
        queries: &FrontRunQueries,
        path: &DefPath,
    ) -> Result<hirsel_run_schema::ProgramNode, QueryError> {
        let mut dependencies: Vec<_> = self
            .dependencies
            .iter()
            .map(|id| id.path().to_string())
            .collect();
        dependencies.push("$hirsel/runtime@1".into());
        Ok(hirsel_run_schema::ProgramNode {
            identity: path.to_string(),
            content: self.content.load(
                &*queries.store,
                &queries.ownership._allowance,
                queries.weight.scratch,
            )?,
            dependencies,
        })
    }
    pub(super) fn weight(&self, weight: &mut hirsel_query_products::retained::MetadataWeight) {
        weight.add(std::mem::size_of_val(&*self.dependencies));
        self.content.weight(weight);
        weight.add(std::mem::size_of_val(&*self.observations));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn live_node_content_keeps_order_and_refuses_before_cloning() {
        let source = vec![vec![7; 8192], vec![], vec![7; 8192], vec![9; 3]];
        let saved = Bytes::new(1 << 20, source.clone()).unwrap();
        let store = hirsel_engine_store::MemStore::default();
        let budget = hirsel_engine_admission::ProcessBudget::new(1 << 20);
        let reservation = budget.reserve(1 << 20).unwrap();
        assert!(
            saved
                .load(&store, &reservation, saved.decode_bytes - 1)
                .is_err()
        );
        assert_eq!(
            saved
                .load(&store, &reservation, saved.decode_bytes)
                .unwrap(),
            source
        );
    }
}
