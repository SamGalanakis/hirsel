//! Installed semantic inputs use the ordinary NativeRecords codec and content
//! references. The manifest indexes descriptors; opening it decodes no headers.
use super::*;
use hirsel_syntax::DefPath;

pub(crate) const SEMANTIC_INPUT: Domain = Domain {
    kind: 0x4601_000c,
    schema: 1,
};
pub(crate) const SEMANTIC_HEADER: Domain = Domain {
    kind: 0x4601_000d,
    schema: 1,
};
pub(crate) const SEMANTIC_BODY: Domain = Domain {
    kind: 0x4601_000e,
    schema: 1,
};

pub(crate) const SEMANTIC_SOURCE: Domain = Domain {
    kind: 0x4601_000f,
    schema: 1,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InstalledDefinitionKey {
    pub compiler: ContentHash,
    pub config: ContentHash,
    pub manifest: ContentHash,
    pub definition: DefPath,
    pub specialization: Vec<(String, DefPath)>,
    pub source: ContentHash,
    pub implementation: ContentHash,
    pub dependencies: ContentHash,
}
impl InstalledDefinitionKey {
    pub fn address(&self) -> Address {
        Address(ContentHash::of_parts(
            "hirsel.installed-definition.v1",
            [&bincode::serialize(self).expect("installed definition key encodes")[..]],
        ))
    }
}

/// Physical ownership descriptors, separate from source Check obligations and
/// from the bodies a namespace permits a consumer to demand.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InstalledDefinitionOwnership {
    pub header: Reference,
    pub header_decode_bytes: u64,
    pub body: Reference,
    pub body_decode_bytes: u64,
    pub semantic: Reference,
    pub semantic_decode_bytes: u64,
    pub source: Reference,
    pub source_logical: PathBuf,
    pub source_digest: ContentHash,
    pub source_bytes: u64,
    pub source_base: u32,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InstalledDefinition {
    pub key: InstalledDefinitionKey,
    pub record: Reference,
    pub ownership: InstalledDefinitionOwnership,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct InstalledSpecializations {
    pub(crate) reads: Vec<u8>,
    pub(crate) definitions: Vec<DefPath>,
}

/// A record is ordinary compiler input. No successful verifier verdict is
/// encoded here; executed bodies still go through the existing V path.
#[derive(Serialize, Deserialize)]
pub(crate) struct InstalledSemanticInput {
    pub(crate) definition: DefPath,
    pub(crate) config: ContentHash,
    pub(crate) implementation: ContentHash,
    pub(crate) dependencies: Vec<hirsel_types::SignatureDependency>,
    pub(crate) reads: Vec<u8>,
    pub(crate) semantic_dependencies: Vec<DefPath>,
    pub(crate) discovery: crate::queries::catalog::SourceFacts,
    pub(crate) specializations: Option<InstalledSpecializations>,
    pub(crate) ownership: InstalledDefinitionOwnership,
}
impl InstalledSemanticInput {
    fn dependency_identity(&self) -> std::io::Result<ContentHash> {
        let bytes = bincode::serialize(&(
            &self.dependencies,
            &self.reads,
            &self.semantic_dependencies,
            &self.specializations,
        ))
        .map_err(std::io::Error::other)?;
        Ok(ContentHash::of_parts(
            "hirsel.installed-definition.dependencies.v1",
            [bytes.as_slice()],
        ))
    }
    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, String> {
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_limit(MAX_RECORD as u64)
            .reject_trailing_bytes()
            .deserialize(bytes)
            .map_err(|error| format!("invalid installed semantic input: {error}"))
    }
}
impl InstalledPackage {
    pub fn definition(&self, path: &DefPath) -> Option<&InstalledDefinition> {
        self.definitions.get(path)
    }
    pub fn definitions(&self) -> impl Iterator<Item = &InstalledDefinition> {
        self.definitions.values()
    }
    pub(super) fn index_definitions(
        &mut self,
        store: &hirsel_artifact_store::Store,
        records: &mut InstallationRecords,
    ) -> std::io::Result<()> {
        let manifest = ContentHash::of_bytes(&self.manifest);
        let references = records
            .contents
            .values()
            .filter(|reference| reference.domain == SEMANTIC_INPUT)
            .copied()
            .collect::<Vec<_>>();
        self.definitions.clear();
        for reference in references {
            let bytes = store
                .get(reference)
                .ok_or_else(|| std::io::Error::other("missing installed semantic input"))?;
            let record = InstalledSemanticInput::decode(&bytes).map_err(std::io::Error::other)?;
            if record.definition.0.package != self.package {
                continue;
            }
            for content in [
                record.ownership.header,
                record.ownership.source,
                record.ownership.body,
                record.ownership.semantic,
            ] {
                if records.contents.get(&content.output.0) != Some(&content) {
                    return Err(std::io::Error::other(
                        "missing installed definition content",
                    ));
                }
            }
            let dependencies = record.dependency_identity()?;
            let key = InstalledDefinitionKey {
                compiler: self.compiler,
                config: record.config,
                manifest,
                specialization: hirsel_types::specialization_parts(&record.definition)
                    .map_or_else(Vec::new, |(_, args)| args),
                definition: record.definition.clone(),
                source: record.ownership.source_digest,
                implementation: record.implementation,
                dependencies,
            };
            let address = key.address();
            let action = Action(reference.output.0);
            store.associate(address, action, reference)?;
            records.rows.insert(address.0, (action, reference));
            let definition = InstalledDefinition {
                key,
                record: reference,
                ownership: record.ownership,
            };
            if self
                .definitions
                .insert(record.definition, definition)
                .is_some()
            {
                return Err(std::io::Error::other(
                    "conflicting installed semantic inputs",
                ));
            }
        }
        Ok(())
    }
    pub(super) fn check_definitions(&self) -> Result<(), String> {
        let manifest = ContentHash::of_bytes(&self.manifest);
        let rows = self
            .rows
            .iter()
            .map(|(address, action, reference)| (*address, (*action, *reference)))
            .collect::<BTreeMap<_, _>>();
        let contents = self
            .contents
            .iter()
            .map(|reference| (reference.output.0, *reference))
            .collect::<BTreeMap<_, _>>();
        for (path, definition) in &self.definitions {
            let key = &definition.key;
            if path != &key.definition
                || path.0.package != self.package
                || key.compiler != self.compiler
                || key.manifest != manifest
                || key.source != definition.ownership.source_digest
                || definition.ownership.source.len != definition.ownership.source_bytes
                || definition.record.domain != SEMANTIC_INPUT
                || definition.ownership.header.domain != SEMANTIC_HEADER
                || definition.ownership.body.domain != SEMANTIC_BODY
                || definition.ownership.source.domain != SEMANTIC_SOURCE
                || key.specialization
                    != hirsel_types::specialization_parts(path)
                        .map_or_else(Vec::new, |(_, args)| args)
            {
                return Err("invalid installed definition descriptor".into());
            }
            if rows.get(&key.address().0) != Some(&(definition.record.output.0, definition.record))
            {
                return Err("missing installed definition row".into());
            }
            for reference in [
                definition.record,
                definition.ownership.header,
                definition.ownership.source,
                definition.ownership.body,
                definition.ownership.semantic,
            ] {
                if contents.get(&reference.output.0) != Some(&reference) {
                    return Err("missing installed definition content".into());
                }
            }
        }
        Ok(())
    }
}

impl InstalledSourceRecords {
    pub(crate) fn definition(&self, path: &DefPath) -> Option<&InstalledDefinition> {
        self.definitions.get(path)
    }

    pub(crate) fn semantic(
        &self,
        path: &DefPath,
        config: ContentHash,
        implementation: ContentHash,
        limit: u64,
    ) -> Result<Option<InstalledSemanticInput>, String> {
        let Some(descriptor) = self.definitions.get(path) else {
            return Ok(None);
        };
        if descriptor.key.config != config || descriptor.key.implementation != implementation {
            return Err("installed semantic input identity mismatch".into());
        }
        let bytes = self.content(descriptor.record, limit)?;
        let record = InstalledSemanticInput::decode(&bytes)?;
        if record.definition != *path
            || record.config != config
            || record.implementation != implementation
            || record.ownership != descriptor.ownership
            || record
                .dependency_identity()
                .map_err(|error| error.to_string())?
                != descriptor.key.dependencies
        {
            return Err("installed semantic input differs from manifest".into());
        }
        Ok(Some(record))
    }
    pub(crate) fn content(&self, reference: Reference, limit: u64) -> Result<Bytes, String> {
        if reference.len > limit {
            return Err("installed content exceeds admission".into());
        }
        let bytes = match self.operation.upgrade() {
            Some(operation) => operation.content(reference),
            None => self.store.get(reference),
        }
        .ok_or_else(|| "missing installed semantic content".to_owned())?;
        if reference.domain.reference(&bytes) != Some(reference) {
            return Err("corrupt installed semantic content".into());
        }
        Ok(bytes)
    }
}
