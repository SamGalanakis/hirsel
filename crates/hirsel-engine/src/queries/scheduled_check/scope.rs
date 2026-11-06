//! Completed ordinary-check obligations. A failed body is an obligation with
//! reads and diagnostics, never permission to demand its unavailable TIR.
use super::*;

pub struct FrontCheckScope {
    reads: BTreeMap<DefPath, Arc<Reads>>,
    lowered: BTreeSet<DefPath>,
    failed_lowered: BTreeSet<DefPath>,
    _owner: Arc<FrontCheckQueries>,
    _allowance: hirsel_engine_admission::MemoryReservation,
}
impl FrontCheckScope {
    pub(super) fn owner(&self) -> &Arc<FrontCheckQueries> {
        &self._owner
    }

    pub fn reads(&self) -> &BTreeMap<DefPath, Arc<Reads>> {
        &self.reads
    }
    pub fn lowered(&self) -> &BTreeSet<DefPath> {
        &self.lowered
    }
    pub fn failed_lowered(&self) -> &BTreeSet<DefPath> {
        &self.failed_lowered
    }
    /// Only successful obligations already selected by this completed Check.
    /// The failure guard precedes the immutable owner's body decoder.
    pub fn definition(&self, path: &DefPath) -> Result<hirsel_tir::DefinitionRef, QueryError> {
        if !self.lowered.contains(path) || self.failed_lowered.contains(path) {
            return Err(QueryError::NotImplemented);
        }
        let query = self
            ._owner
            .plan
            .lower
            .query(path)
            .ok_or(QueryError::NotImplemented)?;
        let owner = self
            ._owner
            .completed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(|(_, data)| data.observations.lower_owner(query))
            .cloned()
            .ok_or(QueryError::NotImplemented)?;
        owner.settled_definition(path)
    }
}
impl FrontCheckQueries {
    /// Admission for the exact completed scope, without decoding a body or
    /// copying read rows. It includes transient union buffers as well as hold.
    pub fn completed_scope_bytes(&self, product: ProductRef) -> Result<u64, QueryError> {
        let completed = self.completed.lock().unwrap_or_else(|e| e.into_inner());
        let (selected, data) = completed.as_ref().ok_or(QueryError::NotImplemented)?;
        if *selected != product || data.checked.completion != CheckCompletion::Complete {
            return Err(QueryError::NotImplemented);
        }
        self.scope_bytes(data)
    }
    fn scope_bytes(&self, data: &Data) -> Result<u64, QueryError> {
        let missing = || QueryError::NotImplemented;
        // Pre-admit every prospective map/set row and copied read buffer.
        // Front reads remain shared, but charging them as well is conservative.
        let mut weight = hirsel_query_products::retained::MetadataWeight::default();
        weight.add(size_of::<FrontCheckScope>());
        let front_rows = data.front.len() + data.failed_front.len();
        let lower_rows = data.lowered.len() + data.failed_lowered.len();
        weight.tree::<DefPath, Arc<Reads>>(front_rows + lower_rows);
        weight.tree::<DefPath, ()>(lower_rows);
        weight.tree::<DefPath, ()>(data.failed_lowered.len());
        for (path, observed) in &data.front {
            weight.path(path);
            weight.add(
                usize::try_from(observed.reads.decoding_bytes().ok_or_else(missing)?)
                    .map_err(|_| missing())?,
            );
            weight.add(hirsel_engine_admission::retained_arc_bytes(&Reads::empty()) as usize);
            weight.add(
                observed
                    .reads
                    .len()
                    .checked_mul(2 * size_of::<(ReadKey, ContentHash)>())
                    .ok_or_else(missing)?,
            );
        }
        for (path, _) in &data.failed_front {
            weight.path(path);
            let reads = self.completed_front_reads(data, path).ok_or_else(missing)?;
            weight.add(hirsel_engine_admission::retained_arc_bytes(&*reads) as usize);
            weight.add(
                reads
                    .len()
                    .checked_mul(2 * size_of::<(ReadKey, ContentHash)>())
                    .ok_or_else(missing)?,
            );
        }
        for (path, failed) in data
            .lowered
            .iter()
            .map(|(p, _)| (p, false))
            .chain(data.failed_lowered.iter().map(|(p, _)| (p, true)))
        {
            weight.path(path);
            weight.path(path);
            if failed {
                weight.path(path);
            }
            let query = self.plan.lower.query(path).ok_or_else(missing)?;
            let reader = data.observations.lower_reader(query).ok_or_else(missing)?;
            let rows = reader.len();
            weight.add(
                usize::try_from(reader.decoding_bytes().ok_or_else(missing)?)
                    .map_err(|_| missing())?,
            );
            weight.add(2 * hirsel_engine_admission::retained_arc_bytes(&Reads::empty()) as usize);
            // The Lower read clone, linear union and a possible boxed-slice
            // shrink overlap temporarily. Front rows have two union buffers
            // above; producer-owned originals stay under their own grants.
            weight.add(
                rows.checked_mul(3 * size_of::<(ReadKey, ContentHash)>())
                    .ok_or_else(missing)?,
            );
        }
        Ok(weight.bytes())
    }
    fn completed_front_reads(&self, data: &Data, path: &DefPath) -> Option<Arc<Reads>> {
        data.observations.front_reader(path)?.load(&*self.store)
    }
    /// Scope is minted only for this root's exact completed descriptor. The
    /// caller reserves its inspection buffers separately from producer memory.
    pub fn completed_scope(
        self: &Arc<Self>,
        product: ProductRef,
        allowance: hirsel_engine_admission::MemoryReservation,
    ) -> Result<FrontCheckScope, QueryError> {
        let completed = self.completed.lock().unwrap_or_else(|e| e.into_inner());
        let (selected, data) = completed.as_ref().ok_or(QueryError::NotImplemented)?;
        if *selected != product || data.checked.completion != CheckCompletion::Complete {
            return Err(QueryError::NotImplemented);
        }
        let missing = || {
            QueryError::Operational(hirsel_types::OperationalFailure {
                operation: "completed check scope".into(),
                message: "missing complete settled read evidence or scope allowance".into(),
            })
        };
        if self.scope_bytes(data)? > allowance.bytes() {
            return Err(missing());
        }
        let mut scope = FrontCheckScope {
            reads: BTreeMap::new(),
            lowered: BTreeSet::new(),
            failed_lowered: BTreeSet::new(),
            _owner: self.clone(),
            _allowance: allowance,
        };
        for (path, observed) in &data.front {
            let reads = observed.reads.load(&*self.store).ok_or_else(missing)?;
            scope.reads.insert(path.clone(), reads);
        }
        for (path, _) in &data.failed_front {
            let reads = self.completed_front_reads(data, path).ok_or_else(missing)?;
            scope.reads.insert(path.clone(), reads);
        }
        for (path, failed) in data
            .lowered
            .iter()
            .map(|(p, _)| (p, false))
            .chain(data.failed_lowered.iter().map(|(p, _)| (p, true)))
        {
            let query = self.plan.lower.query(path).ok_or_else(missing)?;
            let reads = data
                .observations
                .lower_reader(query)
                .and_then(|reader| reader.load(&*self.store))
                .ok_or_else(missing)?;
            let complete = match scope.reads.get(path) {
                Some(front) => Arc::new(front.union(&reads).map_err(|_| missing())?),
                None => reads,
            };
            scope.reads.insert(path.clone(), complete);
            scope.lowered.insert(path.clone());
            if failed {
                scope.failed_lowered.insert(path.clone());
            }
        }
        Ok(scope)
    }
}
