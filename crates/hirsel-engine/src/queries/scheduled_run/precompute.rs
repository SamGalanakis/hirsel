//! Check first, then demand every source-owned semantic/body product and each
//! source package's universe summary. The continuation retains only its
//! position and fold, never a launch or revision.
use super::*;
use hirsel_syntax::PackageId;
use hirsel_validate::PackageUniverse;
const OUTPUT: Domain = Domain {
    kind: 0x4600_0036,
    schema: 2,
};
const UNIVERSE: Domain = Domain {
    kind: 0x4600_0037,
    schema: 1,
};
#[derive(Clone, Copy)]
pub struct FrontPrecomputeOutput {
    pub definitions: u64,
    pub digest: ContentHash,
}
pub(super) struct Selection {
    query: QueryId,
    roots: Vec<(QueryId, bool)>,
    definitions: u64,
    /// Each source package's universe summary query, by address.
    universes: BTreeMap<ContentHash, (QueryId, PackageId)>,
    universe_completed: Mutex<BTreeMap<ContentHash, ProductRef>>,
}
impl FrontRunQueries {
    pub fn with_precompute(
        mut self: Arc<Self>,
        check: Arc<FrontCheckQueries>,
    ) -> Result<Arc<Self>, QueryError> {
        if !self.source_products || !check.matches_inputs(&self.front, &self.lower) {
            return Err(operational(
                "precompute requires matching source-only producers",
            ));
        }
        let this = Arc::get_mut(&mut self)
            .ok_or_else(|| operational("precompute requires an unshared source view"))?;
        let roots = this
            .semantic_ids
            .iter()
            .map(|(path, query)| (*query, this.inputs[path].source))
            .collect::<Vec<_>>();
        let bytes = roots
            .capacity()
            .checked_mul(std::mem::size_of::<(QueryId, bool)>())
            .ok_or(QueryError::NotImplemented)? as u64;
        let mut remaining = this
            .retention
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        *remaining = remaining
            .checked_sub(bytes)
            .ok_or_else(|| operational("precompute inventory exceeds admission"))?;
        drop(remaining);
        let query = QueryId {
            kind: FRONT_RUN_KIND,
            address: hirsel_artifact_store::Address(ContentHash::of_parts(
                "hirsel.front.precompute.address.v1",
                [check.query().address.0.0.as_slice()],
            )),
        };
        let universes = this
            .inputs
            .iter()
            .filter(|(_, input)| input.source)
            .map(|(path, _)| path.0.package.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(|package| {
                let query = universe_query(this.environment, &package);
                (query.address.0, (query, package))
            })
            .collect();
        this.check = Some(check);
        let definitions = roots.iter().filter(|(_, source)| *source).count() as u64;
        this.precompute = Some(Selection {
            query,
            roots,
            definitions,
            universes,
            universe_completed: Mutex::new(BTreeMap::new()),
        });
        Ok(self)
    }
    /// `package`'s published universe summary, decoded from its archived
    /// bytes, once this precomputation has completed it.
    pub fn published_universe(&self, package: &PackageId) -> Option<PackageUniverse> {
        let query = universe_query(self.environment, package);
        let product = self
            .precompute
            .as_ref()?
            .universe_completed
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&query.address.0)?
            .clone();
        let archive = product.archive(ArchiveAdmission::Retained).ok()?;
        PackageUniverse::decode(archive.bytes())
    }
    pub fn precompute_query(&self) -> Option<QueryId> {
        self.precompute.as_ref().map(|selection| selection.query)
    }
    pub fn precompute_outcome(&self, product: ProductRef) -> Option<FrontPrecomputeOutput> {
        self.precomputed
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .and_then(|(selected, output)| (*selected == product).then_some(*output))
    }
}
/// The address of `package`'s universe summary under one engine environment.
/// An installed package's products share the environment their importer
/// reads semantic products under, so an importer names it without its rows.
pub fn universe_query(environment: ContentHash, package: &PackageId) -> QueryId {
    QueryId {
        kind: FRONT_RUN_KIND,
        address: hirsel_artifact_store::Address(ContentHash::of_parts(
            "hirsel.front.universe.address.v1",
            [
                environment.0.as_slice(),
                package.name.as_bytes(),
                &[0],
                package.version.as_bytes(),
            ],
        )),
    }
}
pub(super) fn create_universe(
    queries: &Arc<FrontRunQueries>,
    query: QueryId,
) -> Option<Box<dyn QueryTask>> {
    let selection = queries.precompute.as_ref()?;
    let (selected, package) = selection.universes.get(&query.address.0)?;
    if *selected != query {
        return None;
    }
    let terminal = selection
        .universe_completed
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get(&query.address.0)
        .map(|product| Ok(product.clone()));
    Some(Box::new(UniverseTask {
        queries: queries.clone(),
        query,
        package: package.clone(),
        terminal,
    }))
}
/// One source package's V universe summary: its input identity, errors and
/// first heap role, computed from the same snapshot the Check root read.
struct UniverseTask {
    queries: Arc<FrontRunQueries>,
    query: QueryId,
    package: PackageId,
    terminal: Option<Result<ProductRef, Reference>>,
}
impl QueryTask for UniverseTask {
    fn id(&self) -> QueryId {
        self.query
    }
    fn weight(&self) -> MemoryWeight {
        MemoryWeight {
            retained: self.queries.weight.retained / 2,
            ..self.queries.weight
        }
    }
    fn continuation_bytes(&self) -> u64 {
        self.weight().retained
    }
    fn poll(&mut self, _cx: &mut dyn QueryContext) -> QueryPoll {
        let result = match self.terminal.clone() {
            Some(result) => result,
            None => self
                .complete()
                .map_err(|error| self.queries.failure_record(error)),
        };
        self.terminal = Some(result.clone());
        match result {
            Ok(product) => QueryPoll::Complete(product),
            Err(error) => QueryPoll::Failed(error),
        }
    }
}
impl UniverseTask {
    fn complete(&self) -> Result<ProductRef, QueryError> {
        let queries = &self.queries;
        let selection = queries
            .precompute
            .as_ref()
            .ok_or_else(|| operational("universe summary requires precomputation"))?;
        let universe = queries.front.signatures().package_universe(&self.package);
        let bytes = universe.encode().ok_or_else(|| {
            operational("a package universe with signature errors is not published")
        })?;
        let record = queries
            .store
            .put_content(UNIVERSE, &bytes)
            .ok_or_else(|| operational("universe summary exceeds transport admission"))?;
        let action = hirsel_artifact_store::Action(ContentHash::of_parts(
            "hirsel.front.universe.action.v1",
            [
                self.query.address.0.0.as_slice(),
                record.output.0.0.as_slice(),
            ],
        ));
        let product = ProductRef::import_archive(
            self.query.address,
            action,
            queries
                .admit_archive(record, bytes)
                .map_err(|_| operational("universe summary exceeds admission"))?,
        );
        selection
            .universe_completed
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(product.address.0, product.clone());
        queries
            .store
            .associate_product(product.address, product.action, record);
        Ok(product)
    }
}
pub(super) fn create(queries: &Arc<FrontRunQueries>, query: QueryId) -> Option<Box<dyn QueryTask>> {
    if queries.precompute_query() != Some(query) {
        return None;
    }
    let terminal = queries
        .precomputed
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .as_ref()
        .map(|(product, _)| Ok(product.clone()));
    Some(Box::new(Task {
        queries: queries.clone(),
        query,
        checked: false,
        index: 0,
        digest: ContentHash::default(),
        waiting: None,
        terminal,
    }))
}
struct Task {
    queries: Arc<FrontRunQueries>,
    query: QueryId,
    checked: bool,
    index: usize,
    digest: ContentHash,
    waiting: Option<QueryId>,
    terminal: Option<Result<ProductRef, Reference>>,
}
impl QueryTask for Task {
    fn id(&self) -> QueryId {
        self.query
    }
    fn weight(&self) -> MemoryWeight {
        MemoryWeight {
            retained: self.queries.weight.retained / 2,
            ..self.queries.weight
        }
    }
    fn continuation_bytes(&self) -> u64 {
        self.weight().retained
    }
    fn poll(&mut self, cx: &mut dyn QueryContext) -> QueryPoll {
        let result = if let Some(result) = self.terminal.clone() {
            result
        } else {
            match self.advance(cx) {
                Poll::Pending => return QueryPoll::Waiting(self.waiting.into_iter().collect()),
                Poll::Ready(result) => result,
            }
        };
        self.terminal = Some(result.clone());
        match result {
            Ok(product) => QueryPoll::Complete(product),
            Err(error) => QueryPoll::Failed(error),
        }
    }
}
impl Task {
    fn read(
        &mut self,
        cx: &mut dyn QueryContext,
        query: QueryId,
    ) -> Poll<Result<ProductRef, Reference>> {
        match cx.read(query) {
            Poll::Pending => {
                self.waiting = Some(query);
                Poll::Pending
            }
            Poll::Ready(result) => {
                self.waiting = None;
                Poll::Ready(result)
            }
        }
    }
    fn advance(&mut self, cx: &mut dyn QueryContext) -> Poll<Result<ProductRef, Reference>> {
        if !self.checked {
            let Some(check) = self.queries.check.clone() else {
                return Poll::Ready(Err(self.queries.errors.protocol));
            };
            let product = match self.read(cx, check.query()) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
                Poll::Ready(Ok(product)) => product,
            };
            let Some(output) = check.outcome(product) else {
                return Poll::Ready(Err(self.queries.errors.protocol));
            };
            if has_errors(&output.checked.diagnostics) {
                return Poll::Ready(Err(self
                    .queries
                    .failure_record(QueryError::Diagnostics(output.checked.diagnostics))));
            }
            if output.checked.completion != CheckCompletion::Complete {
                return Poll::Ready(Err(self.queries.failure_record(QueryError::NotImplemented)));
            }
            self.checked = true;
        }
        let queries = self.queries.clone();
        let Some(selection) = &queries.precompute else {
            return Poll::Ready(Err(queries.errors.protocol));
        };
        while let Some((query, source)) = selection.roots.get(self.index).copied() {
            let product = match self.read(cx, query) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
                Poll::Ready(Ok(product)) => product,
            };
            if source {
                let archive = match product.archive(ArchiveAdmission::Retained) {
                    Ok(archive) => archive,
                    Err(_) => return Poll::Ready(Err(queries.errors.resource)),
                };
                self.digest = ContentHash::of_parts(
                    "hirsel.front.precompute.fold.v1",
                    [
                        self.digest.0.as_slice(),
                        archive.reference().output.0.0.as_slice(),
                    ],
                );
            }
            self.index += 1;
        }
        // Installed roots publish each source package's universe summary.
        while let Some((query, _)) = selection
            .universes
            .values()
            .nth(self.index - selection.roots.len())
            .cloned()
        {
            let product = match self.read(cx, query) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
                Poll::Ready(Ok(product)) => product,
            };
            let archive = match product.archive(ArchiveAdmission::Retained) {
                Ok(archive) => archive,
                Err(_) => return Poll::Ready(Err(queries.errors.resource)),
            };
            self.digest = ContentHash::of_parts(
                "hirsel.front.precompute.universe-fold.v1",
                [
                    self.digest.0.as_slice(),
                    archive.reference().output.0.0.as_slice(),
                ],
            );
            self.index += 1;
        }
        Poll::Ready((|| {
            let output = FrontPrecomputeOutput {
                definitions: selection.definitions,
                digest: self.digest,
            };
            let bytes = bincode::serialize(&(output.definitions, output.digest))
                .map_err(|_| queries.errors.resource)?;
            let record = queries
                .store
                .put_content(OUTPUT, &bytes)
                .ok_or(queries.errors.resource)?;
            let action = hirsel_artifact_store::Action(ContentHash::of_parts(
                "hirsel.front.precompute.action.v1",
                [
                    self.query.address.0.0.as_slice(),
                    record.output.0.0.as_slice(),
                ],
            ));
            let product = ProductRef::import_archive(
                self.query.address,
                action,
                queries
                    .admit_archive(record, bytes)
                    .map_err(|_| queries.errors.resource)?,
            );
            // The precompute output is the request's root: under Complete it
            // becomes visible only after every component it folds is durable.
            if !queries
                .store
                .publish_root(product.address, product.action, record)
                && queries.store.persistence() == crate::store::Persistence::Complete
            {
                return Err(queries.failure_record(operational(
                    "precompute closure could not be stored durably",
                )));
            }
            *queries
                .precomputed
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some((product.clone(), output));
            Ok(product)
        })())
    }
}
