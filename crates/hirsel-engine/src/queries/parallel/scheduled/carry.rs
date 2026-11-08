//! Admitted typed front facts across fresh source graphs. Complete read checks
//! and bounded source bindings validate and relocate facts without archives.
use super::*;
use hirsel_query_products::retained::Weight;

struct Row {
    id: u32,
    generation: u64,
    owner: DefId,
    text: ContentHash,
    reads: Arc<Reads>,
    reference: Option<Reference>,
    facts: Facts,
    bindings: BTreeMap<hirsel_span::Anchor, (DefId, u32)>,
    types: std::sync::Weak<hirsel_types::TypeInterner>,
    type_rows: Option<Arc<hirsel_types::TypeInterner>>,
    decode_bytes: u64,
    charge: u64,
}
#[derive(Default)]
struct Rows {
    entries: BTreeMap<Key, Row>,
    identities: BTreeMap<u32, Key>,
    used: u64,
    generation: u64,
    subscriptions: BTreeSet<(ReadKey, DefId, u32)>,
}
/// Validated subscription generations for one immutable source view. A later
/// graph may replace a shared row, but cannot make this view trust that row.
pub(super) struct CarryChanges {
    clean: BTreeMap<Key, u64>,
    _charge: hirsel_engine_admission::MemoryReservation,
}
/// Optional host-owned carry, independently bounded from ordinary body storage.
/// Its rows are data only: current text and every read must validate before
/// adoption; Lower/Run still demand bodies and their ordinary authority.
pub struct FrontCarry {
    rows: Mutex<Rows>,
    allowance: hirsel_engine_admission::MemoryReservation,
}
impl FrontCarry {
    pub fn new(
        allowance: hirsel_engine_admission::MemoryReservation,
    ) -> Result<Arc<Self>, hirsel_engine_admission::MemoryReservation> {
        let header = size_of::<Self>() as u64 + 2 * size_of::<usize>() as u64;
        if allowance.bytes() < header.saturating_add(index_bytes(0, 0)) {
            return Err(allowance);
        }
        Ok(Arc::new(Self {
            rows: Mutex::new(Rows {
                used: header,
                ..Rows::default()
            }),
            allowance,
        }))
    }
    pub fn retained_bytes(&self) -> u64 {
        let rows = self.rows.lock().unwrap_or_else(|e| e.into_inner());
        rows.used
            .saturating_add(index_bytes(rows.entries.len(), rows.subscriptions.len()))
    }
    pub fn len(&self) -> usize {
        self.rows
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entries
            .len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn remember(
        &self,
        key: Key,
        owner: DefId,
        text: ContentHash,
        reads: Arc<Reads>,
        reference: Option<Reference>,
        facts: Facts,
        bindings: BTreeMap<hirsel_span::Anchor, (DefId, u32)>,
        types: std::sync::Weak<hirsel_types::TypeInterner>,
        decode_bytes: u64,
        type_rows: Option<Arc<hirsel_types::TypeInterner>>,
    ) {
        let mut weight = hirsel_query_products::retained::MetadataWeight::default();
        weight.tree::<hirsel_span::Anchor, (DefId, u32)>(bindings.len());
        let charge = facts
            .view()
            .weight(false, Some(&reads), false)
            .saturating_add(weight.bytes())
            .saturating_add(
                type_rows
                    .as_ref()
                    .map_or(0, |types| types.retained_local_bytes()),
            );
        let mut rows = self.rows.lock().unwrap_or_else(|e| e.into_inner());
        let old = rows.entries.get(&key).map_or(0, |r| r.charge);
        let Some(used) = rows
            .used
            .checked_sub(old)
            .and_then(|n| n.checked_add(charge))
        else {
            return;
        };
        let old_reads = rows.entries.get(&key).map_or(0, |row| row.reads.len());
        let Some(subscriptions) = rows
            .subscriptions
            .len()
            .checked_sub(old_reads)
            .and_then(|len| len.checked_add(reads.len()))
        else {
            return;
        };
        let id = if let Some(row) = rows.entries.get(&key) {
            row.id
        } else if let Ok(id) = u32::try_from(rows.entries.len()) {
            id
        } else {
            return;
        };
        let entries = rows.entries.len() + usize::from(!rows.entries.contains_key(&key));
        if used.saturating_add(index_bytes(entries, subscriptions)) > self.allowance.bytes() {
            return;
        }
        let Some(generation) = rows.generation.checked_add(1) else {
            return;
        };
        if let Some(old) = rows.entries.remove(&key) {
            for (read, _) in old.reads.iter() {
                rows.subscriptions.remove(&(*read, old.owner, old.id));
            }
        }
        for (read, _) in reads.iter() {
            rows.subscriptions.insert((*read, owner, id));
        }
        rows.identities.insert(id, key);
        rows.generation = generation;
        rows.entries.insert(
            key,
            Row {
                id,
                generation,
                owner,
                text,
                reads,
                reference,
                facts,
                bindings,
                types,
                type_rows,
                decode_bytes,
                charge,
            },
        );
        rows.used = used;
    }
}

// Count the complete trees once using Rust's minimum node occupancy bound.
// Charging a whole node for every read makes bounded carry discard most facts.
fn index_bytes(entries: usize, subscriptions: usize) -> u64 {
    let mut weight = hirsel_query_products::retained::MetadataWeight::default();
    weight.tree::<Key, Row>(entries);
    weight.tree::<u32, Key>(entries);
    weight.tree::<(ReadKey, DefId, u32), ()>(subscriptions);
    weight.bytes()
}

pub(super) struct FactsRef<'a> {
    pub(super) summary: Option<Arc<PublishedSummary>>,
    pub(super) checked: &'a CheckedProduct,
    pub(super) dead_arms: &'a Option<crate::tools::DeadArmUse>,
    pub(super) diagnostics: &'a Vec<Diagnostic>,
    pub(super) diagnostic_phases: &'a Vec<(Diagnostic, String)>,
    pub(super) lowering: Option<&'a lowering::LoweringUse>,
    pub(super) table_keys: Option<&'a Arc<crate::table_keys::TableKeyUse>>,
}
#[derive(Clone, serde::Deserialize)]
#[serde(try_from = "PackedFacts")]
pub(super) struct Facts {
    pub(super) summary: Option<Arc<PublishedSummary>>,
    pub(super) checked: CheckedProduct,
    pub(super) dead_arms: Option<crate::tools::DeadArmUse>,
    pub(super) diagnostics: Vec<Diagnostic>,
    pub(super) diagnostic_phases: Vec<(Diagnostic, String)>,
    pub(super) lowering: Option<lowering::LoweringUse>,
    pub(super) table_keys: Option<Arc<crate::table_keys::TableKeyUse>>,
}
impl Facts {
    fn view(&self) -> FactsRef<'_> {
        FactsRef {
            summary: self.summary.clone(),
            checked: &self.checked,
            dead_arms: &self.dead_arms,
            diagnostics: &self.diagnostics,
            diagnostic_phases: &self.diagnostic_phases,
            lowering: self.lowering.as_ref(),
            table_keys: self.table_keys.as_ref(),
        }
    }
    fn visit_types_mut(&mut self, mut visit: impl FnMut(&mut hirsel_types::TypeId)) {
        fn summary(
            summary: &mut DefinitionSummary,
            visit: &mut impl FnMut(&mut hirsel_types::TypeId),
        ) {
            for call in &mut summary.calls {
                for argument in &mut call.arguments {
                    if let hirsel_types::GenericArgument::Type(ty) = argument {
                        visit(ty);
                    }
                }
            }
        }
        if let Some((checked, _, _)) = &mut self.checked {
            summary(&mut Arc::make_mut(checked).summary, &mut visit);
        }
        if let Some(published) = &mut self.summary {
            let published = Arc::make_mut(published);
            summary(&mut published.summary, &mut visit);
            if let Some(checked) = &mut published.checked {
                summary(&mut Arc::make_mut(checked).summary, &mut visit);
            }
            published.share_checked_storage();
        }
        if let Some(keys) = &mut self.table_keys {
            Arc::make_mut(keys).visit_types_mut(visit);
        }
    }
    fn visit_spans_mut(&mut self, mut visit: impl FnMut(&mut Span)) {
        fn checked(summary: &mut CheckedSummary, visit: &mut impl FnMut(&mut Span)) {
            for call in &mut summary.summary.calls {
                visit(&mut call.site);
            }
            for edge in &mut summary.growth.edges {
                visit(&mut edge.3);
            }
        }
        if let Some((summary, _, _)) = &mut self.checked {
            checked(Arc::make_mut(summary), &mut visit);
        }
        if let Some(summary) = &mut self.summary {
            let summary = Arc::make_mut(summary);
            for call in &mut summary.summary.calls {
                visit(&mut call.site);
            }
            for edge in &mut summary.growth.edges {
                visit(&mut edge.3);
            }
            if let Some(value) = &mut summary.checked {
                checked(Arc::make_mut(value), &mut visit);
            }
            summary.share_checked_storage();
        }
        fn diagnostic(diagnostic: &mut Diagnostic, visit: &mut impl FnMut(&mut Span)) {
            visit(&mut diagnostic.primary);
            for label in &mut diagnostic.secondary {
                visit(&mut label.span);
            }
            if let Some(fix) = &mut diagnostic.fix {
                visit(&mut fix.span);
            }
            for obligation in &mut diagnostic.obligations {
                match obligation {
                    hirsel_diag::Obligation::GenericBound { arguments } => {
                        for argument in arguments {
                            visit(argument);
                        }
                    }
                    hirsel_diag::Obligation::ParameterMode { argument } => visit(argument),
                    hirsel_diag::Obligation::Multiplicity | hirsel_diag::Obligation::CopyProof => {}
                }
            }
        }
        for value in &mut self.diagnostics {
            diagnostic(value, &mut visit);
        }
        for (value, _) in &mut self.diagnostic_phases {
            diagnostic(value, &mut visit);
        }
        if let Some(keys) = &mut self.table_keys {
            Arc::make_mut(keys).visit_spans_mut(visit);
        }
    }
}
impl Facts {
    fn share_summary_reads(&mut self, reads: &Arc<Reads>) -> Option<()> {
        if let Some(summary) = &mut self.summary {
            if summary.reads.as_ref() != reads.as_ref() {
                return None;
            }
            if summary.reads.observed().eq(reads.observed()) {
                Arc::make_mut(summary).reads = reads.clone();
            }
        }
        Some(())
    }
}
// A published summary and its checked product often hold the same proof.
// Preserve it once on the wire, keeping the summary's independent exact reads.
#[derive(serde::Serialize)]
enum PackedSummaryRef<'a> {
    Checked {
        definition: DefId,
        text: ContentHash,
        reads: Option<&'a Arc<Reads>>,
    },
    Owned(&'a Arc<PublishedSummary>),
}
#[derive(serde::Deserialize)]
enum PackedSummary {
    Checked {
        definition: DefId,
        text: ContentHash,
        reads: Option<Arc<Reads>>,
    },
    Owned(Arc<PublishedSummary>),
}
#[derive(serde::Serialize)]
struct PackedFactsRef<'a> {
    summary: Option<PackedSummaryRef<'a>>,
    checked: &'a CheckedProduct,
    dead_arms: &'a Option<crate::tools::DeadArmUse>,
    diagnostics: &'a Vec<Diagnostic>,
    diagnostic_phases: &'a Vec<(Diagnostic, String)>,
    lowering: Option<&'a lowering::LoweringUse>,
    table_keys: Option<&'a Arc<crate::table_keys::TableKeyUse>>,
}
#[derive(serde::Deserialize)]
struct PackedFacts {
    summary: Option<PackedSummary>,
    checked: CheckedProduct,
    dead_arms: Option<crate::tools::DeadArmUse>,
    diagnostics: Vec<Diagnostic>,
    diagnostic_phases: Vec<(Diagnostic, String)>,
    lowering: Option<lowering::LoweringUse>,
    table_keys: Option<Arc<crate::table_keys::TableKeyUse>>,
}
impl serde::Serialize for FactsRef<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        PackedFactsRef {
            summary: self.summary.as_ref().map(|summary| {
                if retained::summary_shared(summary, self.checked, &summary.reads) {
                    PackedSummaryRef::Checked {
                        definition: summary.definition,
                        text: summary.text,
                        reads: if self.checked.as_ref().is_some_and(|(_, reads, _)| {
                            reads.as_ref() == summary.reads.as_ref()
                                && reads.observed().eq(summary.reads.observed())
                        }) {
                            None
                        } else {
                            Some(&summary.reads)
                        },
                    }
                } else {
                    PackedSummaryRef::Owned(summary)
                }
            }),
            checked: self.checked,
            dead_arms: self.dead_arms,
            diagnostics: self.diagnostics,
            diagnostic_phases: self.diagnostic_phases,
            lowering: self.lowering,
            table_keys: self.table_keys,
        }
        .serialize(serializer)
    }
}
impl serde::Serialize for Facts {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        FactsRef {
            summary: self.summary.clone(),
            checked: &self.checked,
            dead_arms: &self.dead_arms,
            diagnostics: &self.diagnostics,
            diagnostic_phases: &self.diagnostic_phases,
            lowering: self.lowering.as_ref(),
            table_keys: self.table_keys.as_ref(),
        }
        .serialize(serializer)
    }
}
impl TryFrom<PackedFacts> for Facts {
    type Error = &'static str;
    fn try_from(facts: PackedFacts) -> Result<Self, Self::Error> {
        let summary = match facts.summary {
            None => None,
            Some(PackedSummary::Owned(summary)) => Some(summary),
            Some(PackedSummary::Checked {
                definition,
                text,
                reads,
            }) => {
                let (checked, checked_reads, _) =
                    facts.checked.as_ref().ok_or("missing checked summary")?;
                let reads = reads.unwrap_or_else(|| checked_reads.clone());
                Some(Arc::new(PublishedSummary {
                    definition,
                    text,
                    reads,
                    storage: None,
                    summary: checked.summary.clone(),
                    growth: checked.growth.clone(),
                    checked: Some(checked.clone()),
                }))
            }
        };
        Ok(Self {
            summary,
            checked: facts.checked,
            dead_arms: facts.dead_arms,
            diagnostics: facts.diagnostics,
            diagnostic_phases: facts.diagnostic_phases,
            lowering: facts.lowering,
            table_keys: facts.table_keys,
        })
    }
}
impl FactsRef<'_> {
    pub(super) fn bytes(&self, decoded: bool) -> u64 {
        self.weight(decoded, None, false)
    }
    fn weight(&self, decoded: bool, compact: Option<&Arc<Reads>>, published: bool) -> u64 {
        let mut w = if decoded {
            Weight::decoded()
        } else {
            Weight::default()
        };
        if published {
            if let Some(summary) = &self.summary {
                w.arc(summary, |w, summary| {
                    retained::definition(w, &summary.summary);
                    w.parameter_growth(&summary.growth);
                    if let Some(checked) = &summary.checked {
                        w.arc(checked, retained::checked);
                    }
                    w.reads(&summary.reads);
                });
            }
        }
        let published_bytes = w.bytes;
        if let Some(reads) = compact {
            w.reads(reads);
        }
        if let Some(summary) = &self.summary
            && !compact.is_some_and(|reads| retained::summary_shared(summary, self.checked, reads))
        {
            w.arc(summary, |w, summary| {
                retained::definition(w, &summary.summary);
                w.parameter_growth(&summary.growth);
                if let Some(checked) = &summary.checked {
                    w.arc(checked, retained::checked);
                }
                w.reads(&summary.reads);
            });
        }
        if let Some((summary, reads, dependencies)) = self.checked {
            w.arc(summary, retained::checked);
            w.reads(reads);
            w.arc(dependencies, |w, dependencies| {
                w.add(dependencies.retained_bytes())
            });
        }
        w.vec(self.diagnostics, Weight::diagnostic);
        w.vec(self.diagnostic_phases, |w, (diagnostic, phase)| {
            w.diagnostic(diagnostic);
            w.add(phase.capacity());
        });
        if let Some(lowering) = self.lowering {
            lowering.retained(&mut w);
        }
        if let Some(keys) = self.table_keys {
            w.arc(keys, |w, keys| keys.retained(w));
        }
        let mut dead = hirsel_query_products::retained::MetadataWeight::default();
        if let Some(uses) = self.dead_arms {
            if !uses.errors.is_empty() {
                dead.tree::<DefPath, ()>(uses.errors.len());
                for path in &uses.errors {
                    dead.path(path);
                }
            }
            if !uses.constructed.is_empty() {
                dead.tree::<(DefPath, u32), ()>(uses.constructed.len());
                for (path, _) in &uses.constructed {
                    dead.path(path);
                }
            }
        }
        w.bytes
            .saturating_sub(published_bytes)
            .saturating_add(dead.bytes())
            .saturating_add(if compact.is_some() {
                0
            } else {
                size_of::<Facts>() as u64
            })
    }
}
impl FrontBodyQueries {
    fn publish_compact_facts(
        &self,
        key: Key,
        reference: Option<Reference>,
        action: hirsel_artifact_store::Action,
        reads: Arc<Reads>,
        facts: Facts,
        memos: Vec<(crate::memo::MemoKey, Arc<Reads>)>,
    ) -> Option<FrontOutcome> {
        let view = FactsRef {
            summary: facts.summary.clone(),
            checked: &facts.checked,
            dead_arms: &facts.dead_arms,
            diagnostics: &facts.diagnostics,
            diagnostic_phases: &facts.diagnostic_phases,
            lowering: facts.lowering.as_ref(),
            table_keys: facts.table_keys.as_ref(),
        };
        let summary = facts.summary.clone()?;
        let charge = view
            .weight(false, Some(&reads), true)
            .checked_add(LiveSummaryProduct::shared_payload_bytes(&summary)?)?
            .checked_add(hirsel_engine_admission::retained_arc_bytes(
                &std::mem::MaybeUninit::<FrontFacts>::uninit(),
            ))?
            .checked_add(
                (memos.len() * std::mem::size_of::<(crate::memo::MemoKey, Arc<Reads>)>()) as u64,
            )?;
        let (allowance, backing) = self.owner_grant(charge, "cached compact facts")?;
        backing.add(allowance)?;
        let lowering = facts.lowering?;
        let diagnostics = facts.diagnostics;
        let owner = FrontFacts::capture(
            self.inputs.get(&key)?.definition.path(),
            Some(QueryKind::Modes),
            reads,
            facts.diagnostic_phases,
            facts.checked,
            facts.dead_arms,
            facts.table_keys,
            memos,
            backing,
        );
        let publication = self.admit_summary(
            hirsel_artifact_store::Address(key),
            action,
            summary,
            owner.clone(),
        )?;
        Some(FrontOutcome::settled(
            reference,
            publication,
            diagnostics,
            lowering,
            owner,
        ))
    }
    /// Install before submitting tasks. The caller keeps the same carry for
    /// later fresh graphs; it holds neither this producer nor its sources.
    pub fn install_carry(&self, carry: Arc<FrontCarry>) -> Result<(), Arc<FrontCarry>> {
        let mut slot = self.carry.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_some()
            || !self
                .slots
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_empty()
        {
            return Err(carry);
        }
        let rows = carry.rows.lock().unwrap_or_else(|e| e.into_inner());
        let mut weight = hirsel_query_products::retained::MetadataWeight::default();
        weight.add(size_of::<CarryChanges>() + 2 * size_of::<usize>());
        weight.tree::<Key, u64>(rows.entries.len());
        if let Ok(charge) = self._allowance.request.reserve(weight.bytes()) {
            let mut clean: BTreeMap<_, _> = rows
                .entries
                .iter()
                .map(|(key, row)| (*key, row.generation))
                .collect();
            // Observe the reverse subscription index without visiting body
            // memos. Only subscribers to changed or unknown inputs are dirty.
            let mut previous = None;
            for (read, owner, id) in &rows.subscriptions {
                let Some((key, row)) = rows
                    .identities
                    .get(id)
                    .and_then(|key| rows.entries.get_key_value(key))
                else {
                    clean.clear();
                    break;
                };
                // Only package-scoped V observations use the owner's context;
                // lookup reads already carry their own module and qualifier.
                let current = match previous {
                    Some((last_read, last_package, observed))
                        if last_read == *read && last_package == *owner =>
                    {
                        observed
                    }
                    _ => {
                        let observed = self.observe_in(&owner.path().0.package, read);
                        previous = Some((*read, *owner, observed));
                        observed
                    }
                };
                let expected = row.reads.get(read);
                if expected.is_none() || current != expected {
                    clean.remove(key);
                }
            }
            *self.carry_changes.lock().unwrap_or_else(|e| e.into_inner()) =
                Some(Arc::new(CarryChanges {
                    clean,
                    _charge: charge,
                }));
        }
        drop(rows);
        *slot = Some(carry);
        Ok(())
    }
}
impl BodyTask {
    pub(super) fn remember_carry(&self) {
        let queries = &self.queries;
        let carry = queries
            .carry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if carry.is_none() {
            return;
        }
        let slots = queries.slots.lock().unwrap_or_else(|e| e.into_inner());
        let Some(slot) = slots.get(&self.input) else {
            return;
        };
        let outcome = match slot {
            outcome => outcome,
        };
        let (reference, summary, diagnostics, lowering) = match &outcome.state {
            FrontState::Failed { .. } => return,
            FrontState::Settled {
                archive,
                publication,
                diagnostics,
                lowering,
                ..
            } => {
                let Some(summary) = publication.live_value::<crate::products::LiveSummaryProduct>()
                else {
                    return;
                };
                (*archive, summary.value_arc(), diagnostics, lowering)
            }
        };
        let dead_arms = outcome.dead_arms.as_ref().map(LintFacts::materialize);
        let facts = FactsRef {
            summary: Some(summary),
            checked: &outcome.checked,
            dead_arms: &dead_arms,
            diagnostics,
            diagnostic_phases: &outcome.diagnostic_phases,
            lowering: Some(lowering),
            table_keys: outcome.table_keys.as_ref(),
        };
        if facts.summary.as_ref().is_some_and(|s| s.storage.is_some()) {
            return;
        }
        if facts.bytes(true) > self.weight.scratch {
            return;
        }
        let facts = Facts {
            summary: facts.summary.clone(),
            checked: facts.checked.clone(),
            dead_arms: facts.dead_arms.clone(),
            diagnostics: facts.diagnostics.clone(),
            diagnostic_phases: facts.diagnostic_phases.clone(),
            lowering: facts.lowering.cloned(),
            table_keys: facts.table_keys.cloned(),
        };
        if let Some(carry) = carry {
            self.remember_live_facts(&carry, reference, outcome.reads.clone(), facts);
        }
    }
    fn remember_live_facts(
        &self,
        carry: &FrontCarry,
        reference: Option<Reference>,
        reads: Arc<Reads>,
        mut facts: Facts,
    ) {
        let queries = &self.queries;
        let mut bindings = BTreeMap::new();
        let mut valid = true;
        facts.visit_spans_mut(|span| {
            if span.anchor == hirsel_span::Anchor(0) {
                return;
            }
            if let Some((owner, occurrence)) = queries.citations.record_owner(span.anchor) {
                bindings.insert(span.anchor, (DefId::intern(&owner), occurrence));
            } else {
                valid = false;
            }
        });
        if !valid {
            return;
        }
        let decode_bytes = facts.view().bytes(true);
        if decode_bytes > self.weight.scratch {
            return;
        }
        let mut roots = Vec::new();
        facts.visit_types_mut(|ty| roots.push(*ty));
        let type_rows = if roots.is_empty() {
            None
        } else {
            let projection = queries.types.project_reachable(roots);
            if projection.retained_bytes() > self.weight.scratch {
                return;
            }
            Some(projection.into_interner())
        };
        let input = &queries.inputs[&self.input];
        carry.remember(
            input.key.0,
            input.definition,
            input.key.1,
            reads,
            reference,
            facts,
            bindings,
            Arc::downgrade(&queries.types),
            decode_bytes,
            type_rows,
        );
    }
    pub(super) fn remember_cached_carry(
        &self,
        reference: Option<Reference>,
        reads: Arc<Reads>,
        bytes: Vec<u8>,
        _decode_bytes: u64,
    ) {
        let Some(carry) = self
            .queries
            .carry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        else {
            return;
        };
        let Some(facts) = self.decode_facts(&bytes, &reads) else {
            return;
        };
        self.remember_live_facts(&carry, reference, reads, facts);
    }
    pub(super) fn load_carry(
        &self,
        cx: &mut dyn QueryContext,
    ) -> Option<Result<ProductRef, Reference>> {
        let queries = &self.queries;
        let carry = queries
            .carry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()?;
        let changes = queries
            .carry_changes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let rows = carry.rows.lock().unwrap_or_else(|e| e.into_inner());
        let row = rows.entries.get(&self.queries.inputs[&self.input].key.0)?;
        let clean = changes.as_ref().is_some_and(|changes| {
            changes.clean.get(&self.queries.inputs[&self.input].key.0) == Some(&row.generation)
        });
        let input = &queries.inputs[&self.input];
        if row.text != input.key.1
            || row.decode_bytes > self.weight.scratch
            || (!clean
                && row
                    .reads
                    .first_changed(|read| queries.observe(input.definition.path(), read))
                    .is_some())
        {
            return None;
        }
        let mut facts = row.facts.clone();
        if !row.types.ptr_eq(&Arc::downgrade(&queries.types)) {
            let mut roots = Vec::new();
            facts.visit_types_mut(|ty| roots.push(*ty));
            if !roots.is_empty() {
                let map = crate::queries::live_record::import_types(
                    row.type_rows.as_ref()?,
                    &queries.types,
                    &roots,
                )?;
                let mut valid = true;
                facts.visit_types_mut(|ty| match map.get(ty) {
                    Some(mapped) => *ty = *mapped,
                    None => valid = false,
                });
                if !valid {
                    return None;
                }
            }
        }
        let mut valid = true;
        facts.visit_spans_mut(|span| {
            if span.anchor == hirsel_span::Anchor(0) {
                return;
            }
            if let Some(anchor) = row
                .bindings
                .get(&span.anchor)
                .and_then(|(owner, occurrence)| {
                    queries
                        .citations
                        .owners
                        .record_source_anchor(owner.path(), *occurrence)
                })
            {
                span.anchor = anchor;
            } else {
                valid = false;
            }
        });
        if !valid {
            return None;
        }
        let reference = row.reference;
        let reads = row.reads.clone();
        // Completion takes the slots lock, while capture takes it before carry.
        drop(rows);
        self.install_facts(cx, reference, reads, facts, clean)
    }
    pub(super) fn decode_facts(&self, bytes: &[u8], reads: &Arc<Reads>) -> Option<Facts> {
        let queries = &self.queries;
        let input = &queries.inputs[&self.input];
        let requirement = self.checkpoint(
            "facts schema",
            queries
                .records
                .record_requirement::<Facts>(bytes.to_vec().into()),
        )?;
        if requirement.required_bytes() > self.weight.scratch {
            return None;
        }
        let facts: Facts = self.checkpoint(
            "facts decode",
            requirement.decode_reserved(
                |bytes| queries.store.try_admit_product_memory(bytes),
                |path, occurrence| {
                    queries
                        .citations
                        .owners
                        .record_source_anchor(path, occurrence)
                },
            ),
        )?;
        let (encoded, roots) = hirsel_types::with_table_collection(|| {
            bincode::serialize_into(std::io::sink(), &facts)
        });
        encoded.ok()?;
        if !roots.iter().all(|id| queries.types.get(*id).is_some())
            || facts.summary.as_ref().is_none_or(|s| {
                s.definition != input.definition
                    || s.storage.is_some()
                    // The publication names source text; a generated query's
                    // input key also binds its execution and checked prefix.
                    || s.text != input.text
                    || s.reads.as_ref() != reads.as_ref()
            })
        {
            return self.checkpoint("facts identity or type roots", None);
        }
        let view = FactsRef {
            summary: facts.summary.clone(),
            checked: &facts.checked,
            dead_arms: &facts.dead_arms,
            diagnostics: &facts.diagnostics,
            diagnostic_phases: &facts.diagnostic_phases,
            lowering: facts.lowering.as_ref(),
            table_keys: facts.table_keys.as_ref(),
        };
        self.checkpoint(
            "facts retained bound",
            (view.bytes(true) <= self.weight.scratch).then_some(facts),
        )
    }
    pub(super) fn install_facts(
        &self,
        cx: &mut dyn QueryContext,
        reference: Option<Reference>,
        reads: Arc<Reads>,
        facts: Facts,
        clean: bool,
    ) -> Option<Result<ProductRef, Reference>> {
        self.install_facts_inner(cx, reference, reads, facts, clean)
    }
    pub(super) fn install_cached_facts(
        &self,
        cx: &mut dyn QueryContext,
        reference: Option<Reference>,
        reads: Arc<Reads>,
        mut facts: Facts,
    ) -> Option<Result<ProductRef, Reference>> {
        // The wire codec gives repeated Arcs independent allocations. Join
        // equal checked facts before selecting the compact retained shape;
        // the complete decoded peak was admitted before this normalization.
        if let Some((checked, checked_reads, _)) = &mut facts.checked {
            if checked_reads.as_ref() == reads.as_ref()
                && checked_reads.observed().eq(reads.observed())
            {
                *checked_reads = reads.clone();
            }
            if let Some(summary) = &mut facts.summary
                && summary.checked.as_ref().is_some_and(|previous| {
                    previous.summary == checked.summary && previous.growth == checked.growth
                })
                && let Some(summary) = Arc::get_mut(summary)
            {
                summary.checked = Some(checked.clone());
            }
        }
        self.install_facts_inner(cx, reference, reads, facts, false)
    }
    fn install_facts_inner(
        &self,
        cx: &mut dyn QueryContext,
        reference: Option<Reference>,
        mut reads: Arc<Reads>,
        mut facts: Facts,
        clean: bool,
    ) -> Option<Result<ProductRef, Reference>> {
        self.checkpoint("shared summary reads", facts.share_summary_reads(&reads))?;
        let queries = &self.queries;
        let input = &queries.inputs[&self.input];
        let mut memo_rows = vec![(
            crate::memo::MemoKey::new(QueryKind::Modes, input.definition),
            reads.clone(),
        )];
        if let Some((_, reads, _)) = &facts.checked {
            memo_rows.push((
                crate::memo::MemoKey::new(QueryKind::Check, input.definition),
                reads.clone(),
            ));
        }
        queries.pool_observed_reads(
            cx,
            &mut reads,
            &mut facts.summary,
            &mut facts.checked,
            &mut memo_rows,
        )?;
        let action = hirsel_engine_store::query_action(&input.key, input.key.1);
        let outcome = queries
            .publish_compact_facts(self.input, reference, action, reads, facts, memo_rows)?;
        let publication = match &outcome.state {
            FrontState::Failed { record, .. } => return Some(Err(*record)),
            FrontState::Settled { publication, .. } => publication.clone(),
        };
        if clean {
            queries
                .clean_memos
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .extend(outcome.memos.iter().map(|(key, _)| *key));
        }
        queries.install_outcome(self.input, outcome, self.weight.scratch);
        queries.store_adoptions.fetch_add(1, Relaxed);
        // Compact facts complete this query independently of body
        // storage. Publishing a body association would fetch its archive just
        // to check existence, even though Check needs only these compact facts.
        // A later actual body demand owns materialization and its association.
        timings::count(Count::StoreHits);
        timings::count(Count::StoreModeHits);
        Some(Ok(publication))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hirsel_span::Anchor;

    #[test]
    fn carry_reservation_covers_rows_refuses_n_minus_one_and_lives_through_last_clone() {
        let owner = DefId::intern(&DefPath::core("carry"));
        for width in [1, 4, 16] {
            let keys: Vec<_> = (0..width).map(|i| ContentHash::of_bytes(&[i])).collect();
            let reference = Domain { kind: 1, schema: 1 }.reference(b"body").unwrap();
            let budget = hirsel_engine_admission::ProcessBudget::new(1 << 20);
            let (carry, allocations) = crate::test_alloc::measured(|| {
                let carry = FrontCarry::new(budget.reserve(1 << 20).unwrap())
                    .ok()
                    .unwrap();
                for key in &keys {
                    carry.remember(
                        *key,
                        owner,
                        *key,
                        Arc::new(Reads::from_vec(vec![(ReadKey::Environment, *key)]).unwrap()),
                        Some(reference),
                        Facts {
                            summary: None,
                            checked: None,
                            dead_arms: None,
                            diagnostics: vec![Diagnostic::error(
                                const { hirsel_diag::RuleId::of("CT-4") },
                                Span::default(),
                                "9".repeat(64),
                            )],
                            diagnostic_phases: vec![],
                            lowering: None,
                            table_keys: None,
                        },
                        BTreeMap::new(),
                        std::sync::Weak::new(),
                        1024,
                        None,
                    );
                }
                carry
            });
            assert_eq!(carry.len(), width as usize);
            let required = carry.retained_bytes();
            assert!(
                allocations.live as u64 <= required,
                "live={} required={required}",
                allocations.live
            );
            let detached = carry.clone();
            drop(carry);
            assert_eq!(budget.reserved(), 1 << 20);
            drop(detached);
            assert_eq!(budget.reserved(), 0);

            let short = FrontCarry::new(budget.reserve(required - 1).unwrap())
                .ok()
                .unwrap();
            for key in &keys {
                short.remember(
                    *key,
                    owner,
                    *key,
                    Arc::new(Reads::from_vec(vec![(ReadKey::Environment, *key)]).unwrap()),
                    Some(reference),
                    Facts {
                        summary: None,
                        checked: None,
                        dead_arms: None,
                        diagnostics: vec![Diagnostic::error(
                            const { hirsel_diag::RuleId::of("CT-4") },
                            Span::default(),
                            "9".repeat(64),
                        )],
                        diagnostic_phases: vec![],
                        lowering: None,
                        table_keys: None,
                    },
                    BTreeMap::new(),
                    std::sync::Weak::new(),
                    1024,
                    None,
                );
            }
            assert!(short.len() < width as usize);
            assert!(short.retained_bytes() <= required - 1);
            drop(short);
            assert_eq!(budget.reserved(), 0);
        }
        let header = (size_of::<FrontCarry>() + 2 * size_of::<usize>()) as u64;
        let budget = hirsel_engine_admission::ProcessBudget::new(header);
        let (refused, allocations) = crate::test_alloc::measured(|| {
            FrontCarry::new(budget.reserve(header - 1).unwrap())
                .err()
                .unwrap()
        });
        assert_eq!(allocations.allocations, 0);
        assert_eq!(budget.reserved(), header - 1);
        drop(refused);
        assert_eq!(budget.reserved(), 0);
    }

    #[test]
    fn adopted_summary_reads_share_one_allocation_without_changing_complete_bytes() {
        let owner = DefId::intern(&DefPath::core("read-alias"));
        let rows = vec![
            (ReadKey::Environment, ContentHash::of_bytes(b"environment")),
            (
                ReadKey::Signature(owner),
                ContentHash::of_bytes(b"signature"),
            ),
        ];
        let reads = Arc::new(Reads::from_vec(rows.clone()).unwrap());
        let duplicate = Arc::new(Reads::from_vec(rows).unwrap());
        let mut facts = Facts {
            summary: Some(Arc::new(PublishedSummary {
                definition: owner,
                text: ContentHash::default(),
                summary: DefinitionSummary {
                    owner,
                    frames: [].into(),
                    calls: [].into(),
                    staged: [].into(),
                },
                growth: ParameterGrowth::default(),
                storage: None,
                checked: None,
                reads: duplicate,
            })),
            checked: None,
            dead_arms: None,
            diagnostics: vec![],
            diagnostic_phases: vec![],
            lowering: None,
            table_keys: None,
        };
        let original = bincode::serialize(&facts).unwrap();
        let mut before = Weight::default();
        before.reads(&facts.summary.as_ref().unwrap().reads);
        before.reads(&reads);
        assert!(!Arc::ptr_eq(&facts.summary.as_ref().unwrap().reads, &reads));
        facts.share_summary_reads(&reads).unwrap();
        assert!(Arc::ptr_eq(&facts.summary.as_ref().unwrap().reads, &reads));
        let mut after = Weight::default();
        after.reads(&facts.summary.as_ref().unwrap().reads);
        after.reads(&reads);
        assert!(after.bytes < before.bytes);
        assert_eq!(bincode::serialize(&facts).unwrap(), original);
        let reversed = Arc::new(
            Reads::from_vec(vec![
                (
                    ReadKey::Signature(owner),
                    ContentHash::of_bytes(b"signature"),
                ),
                (ReadKey::Environment, ContentHash::of_bytes(b"environment")),
            ])
            .unwrap(),
        );
        facts.share_summary_reads(&reversed).unwrap();
        let preserved = &facts.summary.as_ref().unwrap().reads;
        assert!(Arc::ptr_eq(preserved, &reads));
        assert_eq!(
            preserved.observed().copied().collect::<Vec<_>>(),
            vec![
                (ReadKey::Environment, ContentHash::of_bytes(b"environment"),),
                (
                    ReadKey::Signature(owner),
                    ContentHash::of_bytes(b"signature"),
                ),
            ]
        );
        assert_eq!(bincode::serialize(&facts).unwrap(), original);
        let changed = Arc::new(
            Reads::from_vec(vec![(
                ReadKey::Environment,
                ContentHash::of_bytes(b"changed"),
            )])
            .unwrap(),
        );
        assert!(facts.share_summary_reads(&changed).is_none());
        assert_eq!(bincode::serialize(&facts).unwrap(), original);
    }

    #[test]
    fn packed_checked_summary_requires_the_checked_proof_it_references() {
        let reads = Arc::new(Reads::empty());
        let missing = PackedFactsRef {
            summary: Some(PackedSummaryRef::Checked {
                definition: DefId::intern(&DefPath::core("missing-proof")),
                text: Default::default(),
                reads: Some(&reads),
            }),
            checked: &None,
            dead_arms: &None,
            diagnostics: &vec![],
            diagnostic_phases: &vec![],
            lowering: None,
            table_keys: None,
        };
        let bytes = bincode::serialize(&missing).unwrap();
        assert!(bincode::deserialize::<Facts>(&bytes).is_err());
    }

    #[test]
    fn carry_codec_layout_covers_independent_reads_diagnostics_and_summary_decode_peaks() {
        hirsel_intern::jobs::with_compiler_stack(|| {
            for (width, full) in [(1, true), (4, true), (16, true), (64, true), (4096, false)] {
                let anchor = Anchor::fresh();
                let relocated = Anchor::fresh();
                let owner = DefPath::core("portable");
                let definition = DefId::intern(&owner);
                let carry_read = |index: usize| {
                    (
                        ReadKey::Signature(DefId::intern(&DefPath::core(&format!(
                            "carry_read_{index}"
                        )))),
                        ContentHash::of_bytes(&[index as u8]),
                    )
                };
                let separate_summary_order = full && width == 4;
                let environment = (
                    ReadKey::Environment,
                    ContentHash::of_bytes(b"carry-environment"),
                );
                let observations = if separate_summary_order {
                    vec![environment, carry_read(0), carry_read(1), carry_read(2)]
                } else {
                    (0..if full { width } else { 0 }).map(carry_read).collect()
                };
                let reads = Arc::new(Reads::from_vec(observations).unwrap());
                let checked_reads = if separate_summary_order {
                    Arc::new(
                        Reads::from_vec(vec![
                            carry_read(0),
                            environment,
                            carry_read(1),
                            carry_read(2),
                        ])
                        .unwrap(),
                    )
                } else {
                    reads.clone()
                };
                let checked = Arc::new(CheckedSummary {
                    summary: DefinitionSummary {
                        owner: definition,
                        frames: (0..width)
                            .map(|_| hirsel_query_products::Frame {
                                site: hirsel_types::NameSite::Item,
                                parent: None,
                                kind: FrameKind::Item,
                                generic: false,
                                size_bindings: Box::new([]),
                            })
                            .collect(),
                        calls: [].into(),
                        staged: [].into(),
                    },
                    growth: Default::default(),
                });
                let facts = Facts {
                    summary: Some(Arc::new(PublishedSummary {
                        definition,
                        summary: checked.summary.clone(),
                        growth: checked.growth.clone(),
                        checked: full.then(|| checked.clone()),
                        storage: None,
                        reads: reads.clone(),
                        text: Default::default(),
                    })),
                    checked: full.then(|| {
                        (
                            checked,
                            checked_reads,
                            Arc::new(OrderedSignatureReads::new(&[])),
                        )
                    }),
                    dead_arms: None,
                    diagnostics: if full {
                        vec![Diagnostic::error(
                            const { hirsel_diag::RuleId::of("CARRY-1") },
                            Span {
                                anchor,
                                start: 0,
                                end: 1,
                            },
                            "retained warning",
                        )]
                    } else {
                        vec![]
                    },
                    diagnostic_phases: vec![],
                    lowering: None,
                    table_keys: None,
                };
                let view = FactsRef {
                    summary: facts.summary.clone(),
                    checked: &facts.checked,
                    dead_arms: &facts.dead_arms,
                    diagnostics: &facts.diagnostics,
                    diagnostic_phases: &facts.diagnostic_phases,
                    lowering: facts.lowering.as_ref(),
                    table_keys: facts.table_keys.as_ref(),
                };
                let store = Arc::new(persistent::Contents(Arc::new(
                    hirsel_engine_store::MemStore::default(),
                )));
                let encoder = persistent::TypeRecords::new(
                    hirsel_types::TypeInterner::new(None),
                    store.clone(),
                );
                let decoder =
                    persistent::TypeRecords::new(hirsel_types::TypeInterner::new(None), store);
                let (bytes, outer) = encoder
                    .encode_record_with_decode_bytes(&view, |_| Some((owner.clone(), 0)))
                    .unwrap();
                let bound = outer + view.bytes(true);
                let (decoded, allocations) = crate::test_alloc::measured(|| {
                    decoder
                        .decode_record::<Facts>(&bytes, |_, _| Some(relocated))
                        .unwrap()
                });
                assert!(
                    allocations.peak as u64 <= bound,
                    "width={width}, peak={}, bound={bound}",
                    allocations.peak
                );
                if full {
                    assert_eq!(decoded.diagnostics[0].primary.anchor, relocated);
                    assert_eq!(decoded.summary.as_ref().unwrap().reads.len(), width);
                    assert_eq!(
                        decoded.checked.as_ref().unwrap().1,
                        facts.checked.as_ref().unwrap().1
                    );
                }
                if separate_summary_order {
                    assert_eq!(
                        decoded
                            .summary
                            .as_ref()
                            .unwrap()
                            .reads
                            .observed()
                            .copied()
                            .collect::<Vec<_>>(),
                        vec![environment, carry_read(0), carry_read(1), carry_read(2),]
                    );
                    assert_eq!(
                        decoded
                            .checked
                            .as_ref()
                            .unwrap()
                            .1
                            .observed()
                            .copied()
                            .collect::<Vec<_>>(),
                        vec![carry_read(0), environment, carry_read(1), carry_read(2),]
                    );
                }
                assert_eq!(
                    decoded.summary.as_ref().unwrap().reads,
                    facts.summary.as_ref().unwrap().reads
                );
                assert_eq!(
                    decoded.summary.as_ref().unwrap().summary,
                    facts.summary.as_ref().unwrap().summary
                );
                assert_eq!(
                    decoded.summary.as_ref().unwrap().growth,
                    facts.summary.as_ref().unwrap().growth
                );
            }
        });
    }
}

hirsel_product_codec::decode_variants!(PackedSummary {
    Checked { definition, text, reads }, Owned(f0)
});
hirsel_product_codec::decode_structure!(PackedFacts {
    summary,
    checked,
    dead_arms,
    diagnostics,
    diagnostic_phases,
    lowering,
    table_keys
});
impl hirsel_product_codec::decode_schema::Walk for Facts {
    fn walk(scan: &mut hirsel_product_codec::decode_schema::Scan<'_>) -> Option<()> {
        // try_from also creates the shared publication when PackedSummary::Checked is used.
        scan.add(size_of::<PublishedSummary>() + 2 * size_of::<usize>())?;
        PackedFacts::walk(scan)
    }
}
