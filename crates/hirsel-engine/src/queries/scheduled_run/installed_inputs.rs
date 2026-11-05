//! The Precompute writer publishes one semantic-input record per definition.
//! It uses NativeRecords' existing type/source relocation and allocation weights.
use super::*;
use crate::store::{
    InstalledDefinitionOwnership, InstalledSemanticInput, SEMANTIC_BODY, SEMANTIC_HEADER,
    SEMANTIC_INPUT, SEMANTIC_SOURCE,
};

impl FrontRunQueries {
    pub(super) fn publish_installed_input(&self, path: &DefPath) -> Result<(), QueryError> {
        let Some(writer) = self.store.installation_output() else {
            return Ok(());
        };
        let namespace = self.front.namespace();
        let input = self
            .inputs
            .get(path)
            .ok_or_else(|| operational("missing installed semantic input"))?;
        if !input.source {
            return Ok(());
        }
        let syntax = namespace.syntax(path)?;
        let discovery = catalog::SourceFacts::capture(&syntax);
        let header = namespace.header(path)?;
        let mut reads = Vec::new();
        if let Some(observed) = &header.reads {
            observed.encode(&mut reads);
        }
        let (header_bytes, header_decode) = self
            .records
            .encode_with_decode_bytes(
                &self.anchors,
                &(input.signature.clone(), input.owner_signature.clone()),
            )
            .ok_or_else(|| operational("installed semantic header encoding"))?;
        let header_decode_bytes = header_decode
            .checked_add(crate::memo::retained::retained_signature_payload_bytes(
                &input.signature,
            ))
            .and_then(|bytes| {
                bytes.checked_add(crate::memo::retained::retained_signature_payload_bytes(
                    &input.owner_signature,
                ))
            })
            .ok_or_else(|| operational("installed semantic header size overflow"))?;
        let header_reference =
            self.publish_installed_content(&*writer, SEMANTIC_HEADER, &header_bytes)?;
        drop(header_bytes);
        let (body_bytes, body_decode) = self
            .records
            .encode_with_decode_bytes(&self.anchors, &syntax)
            .ok_or_else(|| operational("installed semantic body encoding"))?;
        let body_decode_bytes = body_decode
            .checked_add(crate::memo::retained::retained_syntax_bytes(&syntax))
            .ok_or_else(|| operational("installed semantic body size overflow"))?;
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
                header_decode_bytes,
                body: body_reference,
                body_decode_bytes,
                semantic: input.content,
                semantic_decode_bytes: input.content_decode_bytes,
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
