//! The Precompute writer publishes one semantic-input record per definition.
//! It uses NativeRecords' existing type/source relocation and allocation weights.
use super::*;
use hirsel_engine_store::{
    InstalledDefinitionOwnership, InstalledSemanticInput, SEMANTIC_BODY, SEMANTIC_HEADER,
    SEMANTIC_INPUT, SEMANTIC_SOURCE,
};

impl FrontRunQueries {
    pub(super) fn publish_installed_input(
        &self,
        path: &DefPath,
        input: &Input,
    ) -> Result<(), QueryError> {
        let Some(writer) = self.store.installation_output() else {
            return Ok(());
        };
        let namespace = self.operation.capture();
        if !input.source {
            return Ok(());
        }
        let syntax = namespace.syntax(path)?;
        let discovery = catalog::capture_source_facts(&syntax);
        let header = namespace.header(path)?;
        let mut reads = Vec::new();
        if let Some(observed) = &header.reads {
            observed.encode(&mut reads);
        }
        let header_bytes = self
            .records
            .encode(
                &self.anchors,
                &(input.signature.clone(), input.owner_signature.clone()),
            )
            .ok_or_else(|| operational("installed semantic header encoding"))?;
        let header_reference =
            self.publish_installed_content(&*writer, SEMANTIC_HEADER, &header_bytes)?;
        drop(header_bytes);
        let body_bytes = self
            .records
            .encode(&self.anchors, &syntax)
            .ok_or_else(|| operational("installed semantic body encoding"))?;
        let body_reference =
            self.publish_installed_content(&*writer, SEMANTIC_BODY, &body_bytes)?;
        drop(body_bytes);
        let (file, source_base) = namespace
            .sources
            .anchor(syntax.anchor)
            .ok_or_else(|| operational("installed source owner is unavailable"))?;
        let source = namespace
            .sources
            .file(file)
            .ok_or_else(|| operational("installed source input is unavailable"))?;
        let source_reference =
            self.publish_installed_content(&*writer, SEMANTIC_SOURCE, source.text().as_bytes())?;
        let record = InstalledSemanticInput {
            definition: path.clone(),
            config: self.environment,
            implementation: input.implementation,
            dependencies: header.output.dependencies.clone(),
            reads,
            semantic_dependencies: input.dependencies.iter().cloned().collect(),
            discovery,
            specializations: input.specializations.as_deref().cloned(),
            ownership: InstalledDefinitionOwnership {
                header: header_reference,
                body: body_reference,
                semantic: input.content,
                source: source_reference,
                source_logical: source.logical_path().to_owned(),
                source_digest: ContentHash::of_bytes(source.text().as_bytes()),
                source_bytes: source.text().len() as u64,
                source_base,
            },
        };
        let bytes = bincode::serialize(&record)
            .map_err(|error| operational(format!("installed semantic input encoding: {error}")))?;
        self.publish_installed_content(&*writer, SEMANTIC_INPUT, &bytes)?;
        Ok(())
    }
    fn publish_installed_content(
        &self,
        writer: &dyn ResultStore,
        domain: Domain,
        bytes: &[u8],
    ) -> Result<Reference, QueryError> {
        if bytes.len() as u64 > self.weight.scratch {
            return Err(operational(
                "installed semantic input exceeds scratch admission",
            ));
        }
        writer
            .put_content(domain, bytes)
            .filter(|reference| domain.reference(bytes) == Some(*reference))
            .ok_or_else(|| operational("installed semantic input exceeds transport admission"))
    }
}
