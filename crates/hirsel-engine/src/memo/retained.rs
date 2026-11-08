//! Allocation accounting for concrete engine entries.
use crate::comptime::StagedRegistration;
use hirsel_engine_admission::arc_bytes;
use hirsel_query_products::retained::*;
mod source_entry;
pub(crate) use source_entry::{
    retained_signature_entry_bytes, source_projection, source_shared_products,
    source_shared_products_bytes, source_signature_head_bytes, source_signature_head_components,
};
/// Owned staged declaration payloads. Signature Arcs are deduplicated within
/// the set; interned type rows remain with the pinned source snapshot.
pub(crate) fn retained_staged_declaration_bytes(registrations: &Vec<StagedRegistration>) -> u64 {
    let mut weight = Weight::default();
    weight.vec(registrations, |w, registration| {
        w.def_path(&registration.path);
        w.def_path(&registration.constant.owner);
        w.flat_vec(&registration.constant.inputs);
        w.signature(&registration.constant.signature);
    });
    weight.bytes
}

/// Owned diagnostic/error buffers, excluding the containing inline enum.

/// One scalar Lower cache entry and its exact-artifact V evidence. Compact
/// receipts outlive eviction independently; table buckets/session interners
/// belong to the session, as with worker holds.
pub(crate) fn retained_lower_entry_bytes(
    entry: &crate::queries::Entry<hirsel_tir::DefinitionRef>,
    verdict: Option<&Vec<hirsel_validate::VRead>>,
) -> u64 {
    let mut weight = Weight::default();
    weight.add(arc_bytes(entry));
    weight.add(arc_bytes(&*entry.memo.reads));
    for (identity, bytes) in entry.reads().allocations() {
        weight.allocation(identity, bytes);
    }
    weight.detail(&entry.detail);
    weight.flat_vec(&entry.covered);
    weight.vec(&entry.selections, |w, selection| {
        if let crate::comptime::Selection::Staged(_, arguments) = selection {
            w.flat_vec(arguments);
        }
    });
    if let Some(depth) = &entry.depth {
        weight.add(size_of_val(&*depth.first));
    }
    if let Some(receipt) = &entry.acceptance {
        weight.add(arc_bytes(receipt.contexts()));
        weight.add(arc_bytes(receipt.artifacts()));
    }
    // Lowering has no phase-products sink: these are the empty local products,
    // never a resolver syntax snapshot or an owner representation.
    debug_assert!(entry.products.resolution.is_none() && entry.products.representation.is_none());
    weight.add(arc_bytes(&*entry.products));
    weight.vec(
        &entry.products.mode_diagnostics,
        |w, (diagnostic, phase)| {
            w.diagnostic(diagnostic);
            w.add(phase.capacity());
        },
    );
    match &entry.memo.value {
        Ok(output) => {
            weight.add(arc_bytes(&**output));
            weight.bytes = weight
                .bytes
                .saturating_add(retained_tir_bytes(&output.value));
            weight.vec(&output.diagnostics, Weight::diagnostic);
            weight.flat_vec(&output.dependencies);
        }
        Err(error) => weight.error(error),
    }
    if let Some(reads) = verdict {
        weight.add(
            std::mem::size_of::<hirsel_validate::VerifiedDefinition>()
                + 2 * std::mem::size_of::<usize>(),
        );
        weight.flat_vec(reads);
    }
    weight.bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use hirsel_query_products::depth::DepthEvidence;
    use hirsel_query_products::reads::Reads;
    use hirsel_query_products::test_support::definition;
    use hirsel_syntax::DefPath;
    use hirsel_types::SharedPath;

    #[test]
    fn lower_entry_weight_includes_lookup_failures_and_spare_receipt_buffers() {
        use std::sync::Arc;
        let output = crate::QueryOutput {
            value: Arc::new(definition()),
            diagnostics: vec![],
            dependencies: vec![],
        };
        let key =
            crate::memo::MemoKey::new(hirsel_query_products::QueryKind::Lower, output.value.path);
        let mut entry = crate::queries::Entry {
            memo: crate::memo::Memo::new(
                key,
                Default::default(),
                Ok(Arc::new(output)),
                Arc::new(Reads::empty()),
            ),
            detail: Default::default(),
            products: Default::default(),
            covered: vec![],
            depth: None,
            acceptance: None,
            verification: Default::default(),
            selections: vec![],
            consistent: true,
        };
        let base = retained_lower_entry_bytes(&entry, None);
        entry.detail.manifest_lookups.push(
            hirsel_types::DependencyRead::ProviderLookup {
                context: (crate::default_context()).into(),
                package: None,
                segments: (vec!["missing".repeat(4096)]).into(),
                output: (hirsel_types::LookupOutput {
                    manifest: Default::default(),
                    result: Err(hirsel_types::QueryError::Operational(
                        hirsel_types::OperationalFailure {
                            operation: "lookup".into(),
                            message: "failure".repeat(4096),
                        },
                    )),
                })
                .into(),
            }
            .shared(),
        );
        let failed = retained_lower_entry_bytes(&entry, None);
        assert!(
            failed > base + 50_000,
            "negative lookup owns both request and failure text"
        );
        std::sync::Arc::make_mut(&mut entry.detail.resolver).extend((0..256).map(|_| {
            hirsel_types::DependencyRead::Signature {
                definition: hirsel_syntax::DefId::intern(&DefPath::core("dependency")),
                fingerprint: None,
            }
            .shared()
        }));
        assert!(retained_lower_entry_bytes(&entry, None) > failed);
        let no_depth = retained_lower_entry_bytes(&entry, None);
        entry.depth = Some(DepthEvidence::leaf(32));
        assert!(retained_lower_entry_bytes(&entry, None) > no_depth);
        let verdict = Vec::with_capacity(512);
        assert!(
            retained_lower_entry_bytes(&entry, Some(&verdict))
                > retained_lower_entry_bytes(&entry, None)
        );
    }
    #[test]
    fn source_allocation_tracker_overflow_charges_again_and_shared_logs_keep_their_prefix() {
        let mut small = NameAllocations::<2>::default();
        assert!(small.first(16));
        assert!(small.first(48));
        assert!(small.first(80));
        assert!(!small.first(16));
        assert!(!small.first(48));
        assert!(small.first(80));

        let read = hirsel_types::DependencyRead::Signature {
            definition: hirsel_syntax::DefId::intern(&DefPath::core("shared.source.read")),
            fingerprint: None,
        }
        .shared();
        let first = std::sync::Arc::new(SharedPath::from(vec![read]));
        let same_nodes = std::sync::Arc::new((*first).clone());
        let mut seen = NameAllocations::<64>::default();
        let mut weight = Weight {
            count_shared_repeats: true,
            source_names: Some(&mut seen),
            ..Default::default()
        };
        weight.read_log(&first);
        let before = weight.bytes;
        weight.read_log(&same_nodes);
        assert_eq!(weight.bytes, before + arc_bytes(&*same_nodes) as u64);
        weight.read_log(&first);
        assert_eq!(weight.bytes, before + arc_bytes(&*same_nodes) as u64);
    }
    #[test]
    fn read_log_weight_counts_shared_prefix_payloads_once_and_new_nodes_in_full() {
        use hirsel_types::DependencyRead;
        use std::sync::Arc;
        let original: SharedPath<_> = vec![
            DependencyRead::ProviderLookup {
                context: (crate::default_context()).into(),
                package: None,
                segments: (vec!["missing".repeat(4096)]).into(),
                output: (hirsel_types::LookupOutput {
                    manifest: Default::default(),
                    result: Err(hirsel_types::QueryError::NotImplemented),
                })
                .into(),
            }
            .shared(),
        ]
        .into();
        let original = Arc::new(original);
        let mut branch = (*original).clone();
        branch.push(
            DependencyRead::Signature {
                definition: hirsel_syntax::DefId::intern(&DefPath::core("callee")),
                fingerprint: None,
            }
            .shared(),
        );
        let branch = Arc::new(branch);
        let mut weight = Weight::default();
        weight.read_log(&original);
        let first = weight.bytes;
        assert!(first > 28_000);
        weight.read_log(&branch);
        let expected_delta = arc_bytes(&*branch)
            + branch.node_allocation_bytes()
            + branch.last().unwrap().retained_allocation().1;
        assert_eq!(weight.bytes, first + expected_delta as u64);
        weight.read_log(&original);
        weight.read_log(&branch);
        assert_eq!(weight.bytes, first + expected_delta as u64);
        let mut independent = Weight::default();
        independent.read_log(&branch);
        assert_eq!(
            independent.bytes,
            first
                + branch.node_allocation_bytes() as u64
                + branch.last().unwrap().retained_allocation().1 as u64
        );
    }
}

#[cfg(test)]
mod syntax_tests;
