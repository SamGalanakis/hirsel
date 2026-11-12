//! Reconstruct the independent source observation scope of one base Instance.
//! Snapshot-wide unions retain declarations for every owner, while a base C
//! receipt observes only its selected source instance or reviewed library plan.
use hirsel_lir::operation::{NativeType, OperationArgument, OperationDeclarations};
use hirsel_lir::{FnAbi, LayoutTable, TargetSpec, layout::LayoutError};
use hirsel_syntax::{ContentHash, DefPath};

pub struct InstanceOperations<'a> {
    pub declarations: &'a (dyn OperationDeclarations + Sync),
    pub inventory: Option<&'a super::seal_inventory::Sources>,
    pub receipts: Option<&'a super::receipt_requests::Sources>,
    pub source: Option<(&'a DefPath, ContentHash)>,
    pub all_sources: bool,
}

impl OperationDeclarations for InstanceOperations<'_> {
    fn receipt_source(
        &self,
        definition: &DefPath,
        instance: ContentHash,
    ) -> Option<hirsel_lir::sealed_payload_source::ReceiptSource> {
        self.receipts
            .and_then(|sources| sources.get(&(definition.clone(), instance)).cloned())
            .or_else(|| self.declarations.receipt_source(definition, instance))
    }

    fn seal_source_witness(
        &self,
        definition: &DefPath,
        instance: ContentHash,
    ) -> Option<hirsel_lir::sealed_payload_source::SealSourceWitness> {
        self.inventory
            .and_then(|sources| sources.get(&(definition.clone(), instance)))
            .map(|source| source.witness().clone())
            .or_else(|| self.declarations.seal_source_witness(definition, instance))
    }
    fn seal_events(
        &self,
        definition: &DefPath,
        instance: ContentHash,
    ) -> Option<Vec<hirsel_lir::sealed_payload_source::SourceEvent>> {
        self.inventory
            .and_then(|sources| sources.get(&(definition.clone(), instance)))
            .map(|source| source.events().to_vec())
            .or_else(|| self.declarations.seal_events(definition, instance))
    }
    fn seal_payloads(
        &self,
        definition: &DefPath,
        instance: ContentHash,
    ) -> Option<Vec<hirsel_lir::sealed_payload_source::SourcePayload>> {
        self.inventory
            .and_then(|sources| sources.get(&(definition.clone(), instance)))
            .map(|source| source.payloads().to_vec())
            .or_else(|| self.declarations.seal_payloads(definition, instance))
    }
    fn seal_environments(
        &self,
        definition: &DefPath,
        instance: ContentHash,
    ) -> Option<hirsel_lir::sealed_payload_source::SourceEnvironments> {
        self.inventory
            .and_then(|sources| sources.get(&(definition.clone(), instance)))
            .and_then(|source| source.source().environments)
            .or_else(|| self.declarations.seal_environments(definition, instance))
    }
    fn seal_descriptor(
        &self,
        source: &NativeType,
        table: &LayoutTable,
        target: &TargetSpec,
    ) -> Result<hirsel_lir::sealed::SealDescriptor, LayoutError> {
        self.declarations.seal_descriptor(source, table, target)
    }
    fn seal_descriptor_for_source(
        &self,
        definition: &DefPath,
        instance: ContentHash,
        source: &NativeType,
        table: &LayoutTable,
        target: &TargetSpec,
    ) -> Result<hirsel_lir::sealed::SealDescriptor, LayoutError> {
        if self
            .inventory
            .and_then(|sources| sources.get(&(definition.clone(), instance)))
            .is_some_and(|expected| {
                expected.payloads().iter().any(|payload| {
                    &payload.root_type == source
                        || payload
                            .recipe
                            .inputs
                            .iter()
                            .any(|input| &input.ty == source)
                }) || expected
                    .events()
                    .iter()
                    .any(|event| &event.root_type == source)
            })
        {
            return self.declarations.seal_descriptor(source, table, target);
        }
        if let Ok(descriptor) = self
            .declarations
            .seal_descriptor_for_source(definition, instance, source, table, target)
        {
            return Ok(descriptor);
        }
        if let Some(selected) = self
            .receipts
            .and_then(|sources| sources.get(&(definition.clone(), instance)))
        {
            return super::receipt_requests::descriptor(
                selected,
                self.declarations,
                source,
                table,
                target,
            );
        }
        self.declarations
            .seal_descriptor_for_source(definition, instance, source, table, target)
    }
    fn requires_structural_reference(&self) -> bool {
        (self.all_sources || self.source.is_some())
            && self.declarations.requires_structural_reference()
    }
    fn structural_access(
        &self,
        source: &NativeType,
        table: &LayoutTable,
        target: &TargetSpec,
    ) -> Result<hirsel_lir::structural_access::AccessPlan, LayoutError> {
        self.declarations.structural_access(source, table, target)
    }
    fn requires_invocation_reference(&self) -> bool {
        (self.all_sources || self.source.is_some())
            && self.declarations.requires_invocation_reference()
    }
    fn invocations(
        &self,
        definition: &DefPath,
    ) -> Option<&[hirsel_lir::invocations::ReferenceInvocation]> {
        if !self.all_sources {
            self.source
                .filter(|(selected, _)| *selected == definition)?;
        }
        self.declarations.invocations(definition)
    }
    fn invocation_body(&self, definition: &DefPath, instance: ContentHash) -> Option<u32> {
        // The owner's call inventory above remains scoped. A synthesized
        // cleanup call separately checks its target's exact independently
        // selected body against the layout's declared finalizer. Filtering
        // those identities to the caller hides a required C dependency.
        self.declarations.invocation_body(definition, instance)
    }
    fn derive(
        &self,
        operation: &hirsel_core_contract::OperationId,
        arguments: &[OperationArgument],
        target: &TargetSpec,
    ) -> Result<(FnAbi, LayoutTable), LayoutError> {
        self.declarations.derive(operation, arguments, target)
    }
}
