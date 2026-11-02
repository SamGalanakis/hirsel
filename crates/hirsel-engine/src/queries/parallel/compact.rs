//! Body-free completion at the worker boundary. Like Go's function queue,
//! the worker releases its working function before accepting another task.
//! Immutable receipts and summaries survive until canonical serial publication.
use super::*;

pub(super) fn missing_summary() -> QueryError {
    QueryError::Operational(hirsel_types::OperationalFailure {
        operation: "definition publication".into(),
        message: "completed front result has no coherent summary".into(),
    })
}
fn inconsistent() -> QueryError {
    QueryError::Operational(hirsel_types::OperationalFailure {
        operation: "front receipt capture".into(),
        message: "completed phase reads disagree".into(),
    })
}

struct CompactStage {
    computed: bool,
    error: Option<QueryError>,
    reads: Arc<Reads>,
    detail: DependencyReads,
    products: Arc<PhaseProducts>,
    consistent: bool,
}
impl CompactStage {
    fn capture<T>(
        owner: &DefPath,
        product: Product<T>,
        inherited: &Arc<Reads>,
        source: &mut impl memos::ReadSource,
    ) -> Self {
        let (computed, result, detail, products, reads) = match product {
            Product::Hit(entry) => (
                false,
                entry.result(),
                entry.detail.clone(),
                entry.products.clone(),
                Ok(entry.memo.reads.clone()),
            ),
            Product::Computed(mut stage) => {
                stage.observed.merged.merge(inherited);
                let reads = memos::complete_reads(owner, &stage.detail, &stage.observed, source)
                    .map(Arc::new);
                (
                    true,
                    stage.result,
                    stage.detail,
                    stage.observed.products,
                    reads,
                )
            }
        };
        // Canonical equality permits retaining the predecessor allocation.
        // Phase identity and complete read coverage remain separate rows.
        let reads = reads.map(|reads| {
            if reads.as_ref() == inherited.as_ref() && reads.observed().eq(inherited.observed()) {
                inherited.clone()
            } else {
                reads
            }
        });
        let consistent = reads.is_ok();
        Self {
            computed,
            error: result.err(),
            reads: reads.unwrap_or_else(|_| Arc::new(Reads::empty())),
            detail,
            products,
            consistent,
        }
    }
}

pub(crate) struct CompactCompleted {
    pub(super) index: usize,
    text: ContentHash,
    resolution: Option<Product<Resolution>>,
    resolution_reads: Option<Arc<Reads>>,
    checked: Option<CompactStage>,
    modes: Option<CompactStage>,
    checked_summary: Option<Arc<CheckedSummary>>,
    dead_arms: Option<crate::tools::DeadArmUse>,
    lowering_inputs: Option<lowering::LoweringUse>,
    payload: Option<Vec<u8>>,
    store_write: Option<Result<(), QueryError>>,
    mode_archive: Option<crate::semantic::ModeArchive>,
    outcome: PublishedResult,
}
impl CompactCompleted {
    /// The package dead-arm lint's facts, for a producer that retains them.
    pub(super) fn take_dead_arms(&mut self) -> Option<crate::tools::DeadArmUse> {
        self.dead_arms.take()
    }
    pub(super) fn memo_snapshot(&self) -> Vec<(crate::memo::MemoKey, Arc<Reads>)> {
        let mut rows = Vec::new();
        let definition = DefId::intern(&self.outcome.definition);
        if let Some(reads) = &self.resolution_reads {
            rows.push((
                crate::memo::MemoKey::new(QueryKind::Resolve, definition),
                reads.clone(),
            ));
        }
        for (kind, stage) in [
            (QueryKind::Check, &self.checked),
            (QueryKind::Modes, &self.modes),
        ] {
            if let Some(stage) = stage {
                if stage.consistent
                    && matches!(stage.error, None | Some(QueryError::Diagnostics(_)))
                {
                    rows.push((
                        crate::memo::MemoKey::new(kind, definition),
                        stage.reads.clone(),
                    ));
                }
            }
        }
        rows
    }

    pub(super) fn checked_product(
        &self,
    ) -> Option<(Arc<CheckedSummary>, Arc<Reads>, Arc<OrderedSignatureReads>)> {
        let stage = self.checked.as_ref()?;
        if !stage.consistent || stage.error.is_some() {
            return None;
        }
        Some((
            self.checked_summary.as_ref()?.clone(),
            stage.reads.clone(),
            Arc::new(OrderedSignatureReads::new(&stage.detail.signatures)),
        ))
    }

    pub(super) fn outcome(&self) -> &PublishedResult {
        &self.outcome
    }
    pub(super) fn take_payload(&mut self) -> Option<Vec<u8>> {
        self.payload.take()
    }

    /// Drain the encoded bytes before this worker accepts another task. Only
    /// the provider verdict returns to canonical serial memo publication.
    pub(super) fn write_payload(&mut self, writer: Option<&persistent::EncodedWriter>) {
        if let Some(payload) = self.payload.take()
            && let Some(writer) = writer
            && let Some(stage) = &self.modes
        {
            let result = writer.write_mode_archive(stage.reads.clone(), payload);
            match &result {
                Ok(Some(proof)) => self.mode_archive = Some(proof.clone()),
                Err(_) => self.mode_archive = None,
                Ok(None) => (),
            }
            self.store_write = Some(result.map(|_| ()));
        }
    }

    pub(crate) fn definition(&self) -> &DefPath {
        &self.outcome.definition
    }

    pub(crate) fn capture(
        done: Completed,
        worker: &Worker<'_>,
        types: Arc<hirsel_types::TypeInterner>,
        source: &mut impl memos::ReadSource,
        records: Option<&persistent::TypeRecords>,
    ) -> Self {
        let result = done.result();
        let diagnostics = match &result {
            Ok(output) => output.diagnostics.clone(),
            Err(QueryError::Diagnostics(ds)) => ds.clone(),
            Err(_) => vec![],
        };
        let checked_output = done.checked.as_ref().and_then(|p| p.result().ok());
        let checked_summary = checked_output.as_ref().map(|output| {
            let (summary, growth) = summarize(worker.types, &output.value);
            Arc::new(CheckedSummary { summary, growth })
        });
        let dead_arms = checked_output.as_ref().map(|output| {
            dead_arms::body_use(
                worker.types,
                worker
                    .snapshot
                    .signature(DefId::intern(&done.definition))
                    .and_then(Result::ok)
                    .as_deref(),
                &output.value,
            )
        });
        let lowering_inputs = result
            .as_ref()
            .ok()
            .map(|output| lowering::LoweringUse::capture(&output.value.typed));
        let diagnostic_phases: Vec<_> = diagnostics
            .iter()
            .map(|d| {
                let mode = done.modes.as_ref().and_then(|p| {
                    let products = match p {
                        Product::Hit(entry) => &entry.products,
                        Product::Computed(stage) => &stage.observed.products,
                    };
                    products
                        .mode_diagnostics
                        .iter()
                        .find(|(other, _)| other.rule == d.rule && other.primary == d.primary)
                        .map(|(_, phase)| phase.clone())
                });
                fn contains<T>(p: &Product<T>, d: &Diagnostic) -> bool {
                    let result = p.result();
                    let ds = match &result {
                        Ok(output) => &output.diagnostics,
                        Err(QueryError::Diagnostics(ds)) => ds,
                        Err(_) => return false,
                    };
                    ds.iter()
                        .any(|other| other.rule == d.rule && other.primary == d.primary)
                }
                let phase = mode.unwrap_or_else(|| {
                    if done.resolution.as_ref().is_some_and(|p| contains(p, d)) {
                        "name resolution".into()
                    } else if done.checked.as_ref().is_some_and(|p| contains(p, d)) {
                        "body checking".into()
                    } else {
                        done.phase.phase_name().into()
                    }
                });
                (d.clone(), phase)
            })
            .collect();
        // Off stores stop before Body::capture or canonical type encoding.
        let payload = if let Some(records) = records {
            done.modes.as_ref().and_then(|mode| {
                persistent::capture_mode(
                    records,
                    types,
                    |anchor| crate::NativeRecordSource::record_owner(&worker.owners, anchor),
                    &result,
                    checked_output
                        .as_ref()
                        .zip(done.checked.as_ref())
                        .map(|(output, checked)| (&**output, checked.detail())),
                    diagnostic_phases.clone(),
                    mode.detail().clone(),
                )
            })
        } else {
            None
        };
        let mut inherited = Arc::new(Reads::empty());
        let mut consistent = true;
        if let Some(product) = &done.resolution {
            match product.complete_reads(&done.definition, &inherited, source) {
                Ok(reads) => inherited = Arc::new(reads),
                Err(_) => consistent = false,
            }
        }
        let mut resolution_reads = done
            .resolution
            .as_ref()
            .filter(|product| {
                consistent && matches!(product.result(), Ok(_) | Err(QueryError::Diagnostics(_)))
            })
            .map(|_| inherited.clone());
        let mut checked = done.checked.map(|product| {
            let mut stage = CompactStage::capture(&done.definition, product, &inherited, source);
            // An inconsistent predecessor cannot grant a coherent descendant.
            stage.consistent &= consistent;
            inherited = stage.reads.clone();
            consistent &= stage.consistent;
            stage
        });
        let modes = done.modes.map(|product| {
            let mut stage = CompactStage::capture(&done.definition, product, &inherited, source);
            // An inconsistent predecessor cannot grant a coherent descendant.
            stage.consistent &= consistent;
            inherited = stage.reads.clone();
            consistent &= stage.consistent;
            stage
        });
        if done.phase == QueryKind::Signature {
            match inherited.union(&done.signature_reads) {
                Ok(reads) => inherited = Arc::new(reads),
                Err(_) => consistent = false,
            }
        }
        if consistent {
            if let Some(reads) = &mut resolution_reads
                && !Arc::ptr_eq(reads, &inherited)
                && let Some(shared) = reads.share_rows(&inherited)
            {
                *reads = Arc::new(shared);
            }
            if let Some(stage) = &mut checked
                && !Arc::ptr_eq(&stage.reads, &inherited)
                && let Some(shared) = stage.reads.share_rows(&inherited)
            {
                stage.reads = Arc::new(shared);
            }
        }
        let mode_archive = modes
            .as_ref()
            .and_then(|stage| stage.products.mode_archive.clone());
        let result = if consistent {
            result.map(|output| {
                let (summary, growth) = summarize(worker.types, &output.value.typed);
                PublishedDefinition {
                    summary: Arc::new(PublishedSummary {
                        definition: DefId::intern(&done.definition),
                        summary,
                        growth,
                        storage: None,
                        checked: checked_summary.clone(),
                        reads: inherited.clone(),
                        text: done.text,
                    }),
                    diagnostics,
                }
            })
        } else {
            Err(inconsistent())
        };
        Self {
            index: done.index,
            text: done.text,
            resolution: done.resolution,
            resolution_reads,
            checked,
            modes,
            checked_summary,
            dead_arms,
            lowering_inputs,
            payload: consistent.then_some(payload).flatten(),
            store_write: None,
            mode_archive,
            outcome: PublishedResult {
                definition: done.definition,
                result,
                phase: Some(done.phase),
                reads: inherited,
                diagnostic_phases,
            },
        }
    }
}
