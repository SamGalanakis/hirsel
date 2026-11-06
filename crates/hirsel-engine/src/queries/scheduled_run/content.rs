//! Immutable semantic tokens shared by the node and its named-content view.
use super::*;

#[derive(Clone)]
struct Bytes {
    value: Arc<Vec<Vec<u8>>>,
    decode_bytes: u64,
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
            value: Arc::new(value),
            decode_bytes,
        })
    }
    fn load(&self, allowance: u64) -> Result<Vec<Vec<u8>>, QueryError> {
        if self.decode_bytes > allowance {
            return Err(operational("Run content exceeds decode admission"));
        }
        Ok((*self.value).clone())
    }
    fn weight(&self, weight: &mut hirsel_query_products::retained::MetadataWeight) {
        weight.arc(&self.value);
        weight.vec(&self.value, |weight, field| weight.flat_vec(field));
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
                    } else if source == *node.content.value {
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
                    .map(|source| source.load(allowance))
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
    pub(super) fn canonical(&self, path: &DefPath) -> (String, &Vec<Vec<u8>>, Vec<String>) {
        let mut dependencies: Vec<_> = self
            .dependencies
            .iter()
            .map(|id| id.path().to_string())
            .collect();
        dependencies.push("$hirsel/runtime@1".into());
        (path.to_string(), &self.content.value, dependencies)
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
            content: self.content.load(queries.weight.scratch)?,
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
        assert!(saved.load(saved.decode_bytes - 1).is_err());
        assert_eq!(saved.load(saved.decode_bytes).unwrap(), source);
    }
}
