//! One bounded body window, independent of source-wide acceptance reductions.
//! This producer owns only cursors. The existing Front/Lower owners charge
//! outputs and retain complete observations for the ordered Check publisher.
use super::*;

pub(super) struct Task {
    queries: Arc<FrontCheckQueries>,
    file: usize,
    root: usize,
    ready: Option<bool>,
    instances: Option<Vec<DefPath>>,
    instance: usize,
    staged: Option<Vec<super::super::scheduled_lower::staged_runtime::Selection>>,
    staged_root: usize,
    extra: usize,
    digest: ContentHash,
    terminal: Option<Result<ProductRef, Reference>>,
}
impl Task {
    pub(super) fn new(queries: Arc<FrontCheckQueries>) -> Self {
        Self {
            queries,
            file: 0,
            root: 0,
            ready: None,
            instances: None,
            instance: 0,
            staged: None,
            staged_root: 0,
            extra: 0,
            digest: ContentHash::of_bytes(b"source admission"),
            terminal: None,
        }
    }
    fn read(
        &mut self,
        cx: &mut dyn QueryContext,
        id: QueryId,
    ) -> Poll<Result<ProductRef, Reference>> {
        let result = cx.read(id);
        if let Poll::Ready(selected) = &result {
            let hash = match selected {
                Ok(product) => product.action.0,
                Err(reference) => reference.output.0,
            };
            self.digest = ContentHash::of_parts(
                "hirsel.front.check-admission.fold.v1",
                [
                    self.digest.0.as_slice(),
                    id.address.0.0.as_slice(),
                    hash.0.as_slice(),
                ],
            );
            cx.consume(id);
        }
        result
    }
    fn advance(&mut self, cx: &mut dyn QueryContext) -> Result<Option<QueryId>, Reference> {
        while self.file < self.queries.files.len() {
            let queries = self.queries.clone();
            let file = &queries.files[self.file];
            if file.error.is_some() || has_errors(&file.diagnostics) {
                self.next_file();
                continue;
            }
            while let Some((path, executable)) = file.paths.get(self.root) {
                let lower = if let Some(lower) = self.ready {
                    lower
                } else {
                    let id = queries
                        .plan
                        .front
                        .query(path)
                        .ok_or(queries.errors.protocol)?;
                    let selected = match self.read(cx, id) {
                        Poll::Pending => return Ok(Some(id)),
                        Poll::Ready(selected) => selected,
                    };
                    let facts = queries
                        .plan
                        .front
                        .check_facts(selected.clone())
                        .ok_or_else(|| selected.err().unwrap_or(queries.errors.protocol))?;
                    let lower = *executable
                        && facts.result.as_ref().is_ok_and(|d| !has_errors(d))
                        && (file.lowering == LoweringPolicy::Reachable
                            || facts.lowering.as_ref().is_some_and(|lowering| {
                                lowering.needed_with(|id| queries.constants.contains(&id))
                            }));
                    self.ready = Some(lower);
                    lower
                };
                if lower {
                    let id = queries
                        .plan
                        .lower
                        .query(path)
                        .ok_or(queries.errors.protocol)?;
                    if self.read(cx, id).is_pending() {
                        return Ok(Some(id));
                    }
                }
                self.root += 1;
                self.ready = None;
            }
            if self.instances.is_none() {
                // The selection is a consequence of these roots' completed
                // Lower work. Another file's Check cannot delay it.
                self.instances = Some(
                    queries
                        .plan
                        .lower
                        .selected_instances(
                            file.paths.iter().map(|(path, _)| path.clone()),
                            queries.weight.scratch,
                        )
                        .map(|paths| paths.into_iter().collect())
                        .unwrap_or_default(),
                );
            }
            while let Some(path) = self
                .instances
                .as_ref()
                .and_then(|rows| rows.get(self.instance))
            {
                let id = queries
                    .plan
                    .lower
                    .query(path)
                    .ok_or(queries.errors.protocol)?;
                if self.read(cx, id).is_pending() {
                    return Ok(Some(id));
                }
                self.instance += 1;
            }
            self.instances = Some(Vec::new());
            if self.staged.is_none() {
                self.staged = Some(
                    queries
                        .plan
                        .lower
                        .runtime_staged_queries(
                            file.paths.iter().map(|(path, _)| path.clone()),
                            queries.weight.scratch,
                            queries.weight.retained,
                        )
                        .unwrap_or_default(),
                );
            }
            while let Some((id, _)) = self
                .staged
                .as_ref()
                .and_then(|rows| rows.get(self.staged_root))
                .copied()
            {
                if self.read(cx, id).is_pending() {
                    return Ok(Some(id));
                }
                self.staged_root += 1;
            }
            self.next_file();
        }
        // Generated instance Front obligations are independent as well. Their
        // diagnostics remain selected by the ordered source owner.
        while let Some(path) = self.queries.instances.get(self.extra) {
            let id = self
                .queries
                .plan
                .front
                .query(path)
                .ok_or(self.queries.errors.protocol)?;
            if self.read(cx, id).is_pending() {
                return Ok(Some(id));
            }
            self.extra += 1;
        }
        Ok(None)
    }
    fn next_file(&mut self) {
        self.file += 1;
        self.root = 0;
        self.ready = None;
        self.instances = None;
        self.instance = 0;
        self.staged = None;
        self.staged_root = 0;
    }
}
impl QueryTask for Task {
    fn id(&self) -> QueryId {
        self.queries.admission_query()
    }
    fn weight(&self) -> MemoryWeight {
        self.queries.weight
    }
    fn continuation_bytes(&self) -> u64 {
        self.weight().retained
    }
    fn poll(&mut self, cx: &mut dyn QueryContext) -> QueryPoll {
        if let Some(result) = &self.terminal {
            return finish(result.clone());
        }
        let result = match self.advance(cx) {
            Ok(Some(id)) => return QueryPoll::Waiting(vec![id]),
            Err(error) => Err(error),
            Ok(None) => Ok(ProductRef::live(
                self.id().address,
                hirsel_artifact_store::Action(self.digest),
                Arc::new(super::super::live_record::LiveRecord {
                    value: self.digest,
                    domain: Domain {
                        kind: 0x4600_003a,
                        schema: 2,
                    },
                    records: self.queries.records.clone(),
                    source: self.queries.anchors.clone(),
                    serialization_working: 0,
                    _admission: self.queries._allowance.clone(),
                }),
            )),
        };
        self.terminal = Some(result.clone());
        finish(result)
    }
}
