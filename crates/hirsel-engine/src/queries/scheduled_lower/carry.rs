//! Admitted typed Lower/Evaluate outcomes across fresh source graphs. Each row
//! retains complete reads and source bindings; adoption replays generation
//! demands and rebinds spans before publishing a current live product.
use super::*;

#[derive(Clone)]
struct Row {
    generation: u64,
    owner: DefId,
    text: ContentHash,
    reads: Reads,
    outcome: Outcome,
    raw: Option<RawLowerOutput>,
    demands: Vec<namespace::reopen::ManifestDemand>,
    bindings: BTreeMap<hirsel_span::Anchor, (DefId, u32)>,
    types: std::sync::Weak<hirsel_types::TypeInterner>,
    type_rows: Option<Arc<hirsel_types::TypeInterner>>,
    charge: u64,
}
#[derive(Default)]
struct Rows {
    entries: BTreeMap<(u32, ContentHash), Row>,
    used: u64,
    generation: u64,
    subscriptions: BTreeSet<(ReadKey, DefId, ContentHash, (u32, ContentHash))>,
}
pub(super) struct CarryChanges {
    clean: BTreeMap<(u32, ContentHash), u64>,
}
fn index_bytes(entries: usize, subscriptions: usize) -> u64 {
    let mut weight = hirsel_query_products::retained::MetadataWeight::default();
    weight.tree::<(u32, ContentHash), Row>(entries);
    weight.tree::<(ReadKey, DefId, ContentHash, (u32, ContentHash)), ()>(subscriptions);
    weight.bytes()
}
/// Optional host-owned carry, independently bounded from ordinary storage.
pub struct LowerCarry {
    rows: Mutex<Rows>,
    allowance: hirsel_engine_admission::MemoryReservation,
}
impl LowerCarry {
    pub fn new(
        allowance: hirsel_engine_admission::MemoryReservation,
    ) -> Result<Arc<Self>, hirsel_engine_admission::MemoryReservation> {
        let header = size_of::<Self>() as u64 + 2 * size_of::<usize>() as u64;
        if allowance.bytes() < header {
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
    fn remember(&self, key: (u32, ContentHash), mut row: Row) {
        let mut rows = self.rows.lock().unwrap_or_else(|e| e.into_inner());
        let old = rows.entries.get(&key).map_or(0, |r| r.charge);
        let Some(used) = rows
            .used
            .checked_sub(old)
            .and_then(|n| n.checked_add(row.charge))
        else {
            return;
        };
        let old_reads = rows.entries.get(&key).map_or(0, |old| old.reads.len());
        let Some(subscriptions) = rows
            .subscriptions
            .len()
            .checked_sub(old_reads)
            .and_then(|count| count.checked_add(row.reads.len()))
        else {
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
            for (read, value) in old.reads.iter() {
                rows.subscriptions.remove(&(*read, old.owner, *value, key));
            }
        }
        row.generation = generation;
        rows.generation = generation;
        for (read, value) in row.reads.iter() {
            rows.subscriptions.insert((*read, row.owner, *value, key));
        }
        rows.entries.insert(key, row);
        rows.used = used;
    }
}
impl FrontLowerQueries {
    /// Install before submitting tasks. The caller keeps the same carry for
    /// later fresh graphs; it holds neither this producer nor its sources.
    pub fn install_carry(&self, carry: Arc<LowerCarry>) -> Result<(), Arc<LowerCarry>> {
        let mut slot = self.carry.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_some()
            || !self
                .outcomes
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_empty()
        {
            return Err(carry);
        }
        let rows = carry.rows.lock().unwrap_or_else(|e| e.into_inner());
        let mut weight = hirsel_query_products::retained::MetadataWeight::default();
        weight.add(size_of::<CarryChanges>() + 2 * size_of::<usize>());
        weight.tree::<(u32, ContentHash), u64>(rows.entries.len());
        let mut retention = self.retention.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(remaining) = retention.checked_sub(weight.bytes()) {
            *retention = remaining;
            drop(retention);
            let mut clean: BTreeMap<_, _> = rows
                .entries
                .iter()
                .map(|(key, row)| (*key, row.generation))
                .collect();
            let mut previous = None;
            for (read, owner, value, key) in &rows.subscriptions {
                let current = match previous {
                    Some((last_read, last_owner, observed))
                        if last_read == *read && last_owner == *owner =>
                    {
                        observed
                    }
                    _ => {
                        let observed = self.observe(owner.path(), read);
                        previous = Some((*read, *owner, observed));
                        observed
                    }
                };
                if current != Some(*value) {
                    clean.remove(key);
                }
            }
            *self.carry_changes.lock().unwrap_or_else(|e| e.into_inner()) =
                Some(Arc::new(CarryChanges { clean }));
        }
        drop(rows);
        *slot = Some(carry);
        Ok(())
    }
    fn carry(&self) -> Option<Arc<LowerCarry>> {
        self.carry.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    /// Inspect a current source/Lower input without executing a query. A missing
    /// observation remains absent; a read key conveys no validation authority.
    pub fn observed_read_value(&self, owner: &DefPath, read: &ReadKey) -> Option<ContentHash> {
        match *read {
            ReadKey::ExecutedBody(id) if self.inputs.get_id(id).is_none() => None,
            ReadKey::LayoutBody(id) if !self.layouts.contains_key(&id) => None,
            _ => self.observe(owner, read),
        }
    }

    /// A read's value in this graph. Body and layout texts come from this
    /// producer's own inputs; every other read is the front's source fact.
    pub(super) fn observe(&self, owner: &DefPath, read: &ReadKey) -> Option<ContentHash> {
        match *read {
            ReadKey::ExecutedBody(id) => Some(memos::optional(
                self.inputs.get_id(id).map(|input| input.implementation),
            )),
            ReadKey::LayoutBody(id) => Some(memos::optional(self.layouts.get(&id).copied())),
            _ => self.front.observe(owner, read),
        }
    }
    pub(super) fn remember_imported_carry(&self, id: QueryId, key: &EntryKey, outcome: &Outcome) {
        if self.carry().is_none() {
            return;
        }
        let raw = match &outcome.result {
            Ok(Value::Lower(lower)) => {
                let Some(output) = lower.decode(self) else {
                    return;
                };
                let Some(materialized) = lower
                    .materialized
                    .get()
                    .and_then(|reference| self.store.get_content(*reference))
                    .and_then(|bytes| self.records.decode(&*self.namespace.anchors, &bytes))
                else {
                    return;
                };
                Some((output, materialized))
            }
            Ok(Value::Constant(_)) => None,
            Err(_) => return,
        };
        self.remember_live_carry(id, key, outcome, &raw);
    }
    pub(super) fn remember_live_carry(
        &self,
        id: QueryId,
        key: &EntryKey,
        outcome: &Outcome,
        raw: &Option<RawLowerOutput>,
    ) {
        let Some(carry) = self.carry() else {
            return;
        };
        let Some(owner) = self
            .paths
            .get(&(id.kind, id.address.0), &self.inputs, &self.namespace)
        else {
            return;
        };
        let Some(mut demands) = self.namespace.live_demands_for_reads(owner, &outcome.reads) else {
            return;
        };
        let mut estimate = outcome.retained_bytes().unwrap_or(u64::MAX);
        if let Some((output, materialized)) = raw {
            estimate = estimate
                .saturating_add(hirsel_query_products::retained::retained_tir_bytes(
                    &output.value,
                ))
                .saturating_add(
                    hirsel_query_products::retained::retained_lowering_output_bytes(
                        &output.diagnostics,
                        &output.dependencies,
                    ),
                );
            let mut weight = hirsel_query_products::retained::MetadataWeight::default();
            weight.vec(materialized, |weight, (_, value)| {
                weight.materialized(value)
            });
            estimate = estimate.saturating_add(weight.bytes());
        }
        // Cloning and collecting source bindings uses the active task's scratch.
        if estimate.saturating_mul(4) > self.weight.scratch {
            return;
        }
        let mut outcome = outcome.clone();
        outcome.observations = None;
        outcome.diagnostic_sources = None;
        if let Ok(Value::Lower(lower)) = &mut outcome.result {
            let Some(verified) = lower.verified(self) else {
                return;
            };
            lower.local_verified = verified;
            lower.verified = None;
            lower.live = None;
        }
        if let Ok(Value::Constant(constants)) = &mut outcome.result {
            let Some((value, substitutions)) = constants.decoded(
                &self.records,
                &self.namespace.anchors,
                &*self.store,
                self.weight.scratch,
            ) else {
                return;
            };
            **constants = Constants::new(value, substitutions);
        }
        let mut raw = raw.clone();
        let mut roots = Vec::new();
        if visit_types(&mut outcome, &mut raw, &mut demands, |ty| roots.push(*ty)).is_none() {
            return;
        }
        let type_rows = if roots.is_empty() {
            None
        } else {
            let projection = self.namespace.snapshot.types().project_reachable(roots);
            if estimate
                .saturating_add(projection.retained_bytes())
                .saturating_mul(4)
                > self.weight.scratch
            {
                return;
            }
            Some(projection.into_interner())
        };
        let mut bindings = BTreeMap::new();
        let mut valid = true;
        visit(&mut outcome, &mut raw, |span| {
            if span.anchor == hirsel_span::Anchor(0) {
                return;
            }
            if let Some((path, occurrence)) = self.namespace.anchors.record_owner(span.anchor) {
                bindings.insert(span.anchor, (DefId::intern(&path), occurrence));
            } else {
                valid = false;
            }
        });
        if !valid {
            return;
        }
        let Some(mut charge) = outcome.retained_bytes() else {
            return;
        };
        let mut weight = hirsel_query_products::retained::MetadataWeight::default();
        weight.tree::<hirsel_span::Anchor, (DefId, u32)>(bindings.len());
        weight.add(demands.capacity() * size_of::<namespace::reopen::ManifestDemand>());
        for demand in &demands {
            weight.path(&demand.declaration);
            weight.flat_vec(&demand.arguments);
        }
        if let Some((output, materialized)) = &raw {
            let Some(bytes) = hirsel_query_products::retained::retained_tir_bytes(&output.value)
                .checked_add(
                    hirsel_query_products::retained::retained_lowering_output_bytes(
                        &output.diagnostics,
                        &output.dependencies,
                    ),
                )
            else {
                return;
            };
            charge = charge.saturating_add(bytes);
            weight.vec(materialized, |weight, (_, value)| {
                weight.materialized(value)
            });
        }
        charge = charge.saturating_add(weight.bytes()).saturating_add(
            type_rows
                .as_ref()
                .map_or(0, |types| types.retained_local_bytes()),
        );
        if charge > carry.allowance.bytes() || charge > self.weight.scratch {
            return;
        }
        carry.remember(
            (id.kind, key.0),
            Row {
                generation: 0,
                owner,
                text: key.1,
                reads: outcome.reads.clone(),
                outcome,
                raw,
                demands,
                bindings,
                types: Arc::downgrade(self.namespace.snapshot.types()),
                type_rows,
                charge,
            },
        );
    }
    pub(super) async fn load_carry(
        self: &Arc<Self>,
        id: QueryId,
        path: &DefPath,
        key: &EntryKey,
        query_reads: &QueryReads,
    ) -> Option<Result<ProductRef, Reference>> {
        let carry = self.carry()?;
        let mut row = {
            let rows = carry.rows.lock().unwrap_or_else(|e| e.into_inner());
            let row = rows.entries.get(&(id.kind, key.0))?;
            if row.text != key.1 || row.charge > self.weight.scratch {
                return None;
            }
            row.clone()
        };
        if !row
            .types
            .ptr_eq(&Arc::downgrade(self.namespace.snapshot.types()))
        {
            let mut roots = Vec::new();
            visit_types(&mut row.outcome, &mut row.raw, &mut row.demands, |ty| {
                roots.push(*ty)
            })?;
            if !roots.is_empty() {
                let map = live_record::import_types(
                    row.type_rows.as_ref()?,
                    self.namespace.snapshot.types(),
                    &roots,
                )?;
                let mut valid = true;
                visit_types(
                    &mut row.outcome,
                    &mut row.raw,
                    &mut row.demands,
                    |ty| match map.get(ty) {
                        Some(mapped) => *ty = *mapped,
                        None => valid = false,
                    },
                )?;
                if !valid {
                    return None;
                }
            }
        }
        if let Some(producer) = self.demand_producer() {
            let mut capture = self.namespace.clone();
            let mut successor = None;
            for demand in &row.demands {
                let site = capture
                    .bodies
                    .get(&DefId::intern(&demand.declaration))
                    .and_then(|body| body.queries.as_ref())
                    .map(|keys| keys.site)
                    .unwrap_or_default();
                let product = producer
                    .demand(
                        &capture,
                        &demand.declaration,
                        &demand.arguments,
                        site,
                        query_reads.clone(),
                    )
                    .await
                    .ok()?;
                product.result.ok()?;
                if let Some(next) = product.successor {
                    capture = next.namespace().clone();
                    successor = Some(next);
                }
            }
            if let Some(successor) = successor
                && capture.identity() != self.namespace.identity()
            {
                let owner = successor.owner().clone();
                let actual = owner.id(path, id.kind)?;
                return Some(self.forward_capture(id, actual, owner, query_reads).await);
            }
        } else if !row.demands.is_empty() {
            return None;
        }
        let clean = self
            .carry_changes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .is_some_and(|changes| changes.clean.get(&(id.kind, key.0)) == Some(&row.generation));
        if !clean
            && row
                .reads
                .first_changed(|read| self.observe(path, read))
                .is_some()
        {
            return None;
        }
        let mut outcome = row.outcome;
        let mut raw = row.raw;
        let mut valid = true;
        visit(&mut outcome, &mut raw, |span| {
            if span.anchor == hirsel_span::Anchor(0) {
                return;
            }
            if let Some(anchor) = row
                .bindings
                .get(&span.anchor)
                .and_then(|(path, occurrence)| {
                    self.namespace
                        .anchors
                        .record_source_anchor(path.path(), *occurrence)
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
        if let Ok(Value::Lower(lower)) = &mut outcome.result {
            lower.body = Arc::new(std::sync::OnceLock::new());
            lower.materialized = Arc::new(std::sync::OnceLock::new());
            lower.archived_metadata = None;
        }
        if let Err(error) = self
            .restore_selections(&outcome.selections, query_reads)
            .await
        {
            return Some(Err(error));
        }
        Some(self.publish_live(id, key, outcome, raw, query_reads).await)
    }
}
fn visit(
    outcome: &mut Outcome,
    raw: &mut Option<RawLowerOutput>,
    mut visit: impl FnMut(&mut Span),
) {
    let mut depth = outcome.depth.evidence();
    for span in depth.first.iter_mut().flatten() {
        visit(span);
    }
    outcome.depth = depth.into();
    if let Ok(Value::Lower(lower)) = &mut outcome.result {
        for diagnostic in &mut lower.diagnostics {
            namespace::diagnostic_spans(diagnostic, &mut visit);
        }
    }
    if let Some((output, _)) = raw {
        for diagnostic in &mut output.diagnostics {
            namespace::diagnostic_spans(diagnostic, &mut visit);
        }
        for body in &mut Arc::make_mut(&mut output.value).bodies {
            for local in &mut body.locals {
                visit(&mut local.span);
            }
            for scope in &mut body.scopes {
                visit(&mut scope.span);
            }
            for block in &mut body.blocks {
                visit(&mut block.span);
                for instruction in &mut block.instructions {
                    visit(&mut instruction.span);
                }
            }
        }
    }
}

fn arguments_types(
    arguments: &mut [hirsel_types::GenericArgument],
    visit: &mut impl FnMut(&mut hirsel_types::TypeId),
) {
    for argument in arguments {
        if let hirsel_types::GenericArgument::Type(ty) = argument {
            visit(ty);
        }
    }
}
pub(super) fn materialized_types(
    value: &mut Materialized,
    visit: &mut impl FnMut(&mut hirsel_types::TypeId),
) {
    let mut pending = vec![&mut value.root];
    while let Some(value) = pending.pop() {
        visit(&mut value.ty);
        match &mut value.kind {
            hirsel_tir::ValueKind::FunctionReference { arguments, .. } => {
                arguments_types(arguments, visit)
            }
            hirsel_tir::ValueKind::Construct { fields, .. } => pending.extend(fields),
            hirsel_tir::ValueKind::Constant(_) => {}
        }
    }
}
fn visit_types(
    outcome: &mut Outcome,
    raw: &mut Option<RawLowerOutput>,
    demands: &mut [namespace::reopen::ManifestDemand],
    mut visit: impl FnMut(&mut hirsel_types::TypeId),
) -> Option<()> {
    for demand in demands {
        arguments_types(&mut demand.arguments, &mut visit);
    }
    if let Ok(Value::Constant(constants)) = &mut outcome.result {
        constants.visit_types_mut(&mut visit)?;
    }
    if let Some((output, materialized)) = raw {
        let definition = Arc::make_mut(&mut output.value);
        for ty in Arc::make_mut(&mut definition.types) {
            visit(ty);
        }
        for body in &mut definition.bodies {
            visit(&mut body.result);
            for local in &mut body.locals {
                visit(&mut local.ty);
            }
            for place in &mut body.places {
                visit(&mut place.ty);
            }
            for rvalue in &mut body.rvalues {
                match rvalue {
                    hirsel_tir::Rvalue::Shape(ty) => visit(ty),
                    hirsel_tir::Rvalue::FunctionReference { arguments, .. } => {
                        arguments_types(arguments, &mut visit)
                    }
                    _ => {}
                }
            }
            for block in &mut body.blocks {
                if let hirsel_tir::Terminator::Call { callee, .. } = &mut block.terminator {
                    match callee {
                        hirsel_tir::Callee::Definition { arguments, .. }
                        | hirsel_tir::Callee::Indirect { arguments, .. } => {
                            arguments_types(arguments, &mut visit)
                        }
                    }
                }
            }
        }
        for (_, value) in materialized {
            materialized_types(value, &mut visit);
        }
    }
    Some(())
}
