//! Dependency-ready source products and universe summaries. The fold retains
//! only its cursor and digest; a separate Check join owns durable publication.
use super::*;
mod acceptance;
use hirsel_syntax::PackageId;
use hirsel_validate::PackageUniverse;
const OUTPUT: Domain = Domain {
    kind: 0x4600_0036,
    schema: 2,
};
// 0x4600_0037 is the compact Check store's domain.
pub(crate) const UNIVERSE: Domain = Domain {
    kind: 0x4600_0038,
    schema: 2,
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
            .map(|(path, query)| {
                (
                    *query,
                    this.operation
                        .capture()
                        .bodies
                        .contains_key(&DefId::intern(path)),
                )
            })
            .collect::<Vec<_>>();
        let bytes = roots
            .capacity()
            .checked_mul(std::mem::size_of::<(QueryId, bool)>())
            .ok_or(QueryError::NotImplemented)? as u64;
        this.retain(bytes)
            .map_err(|_| operational("precompute inventory exceeds admission"))?;
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
            .filter(|path| {
                this.operation
                    .capture()
                    .bodies
                    .contains_key(&DefId::intern(path))
            })
            .map(|path| path.0.package.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .filter_map(|package| {
                let sources = this.front.signatures().package_sources(&package)?;
                let query = universe_query(this.environment, &package, sources);
                Some((query.address.0, (query, package)))
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
        let sources = self.front.signatures().package_sources(package)?;
        let query = universe_query(self.environment, package, sources);
        let product = self
            .precompute
            .as_ref()?
            .universe_completed
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&query.address.0)?
            .clone();
        let archive = product.archive(ArchiveAdmission::Retained).ok()?;
        Some(crate::snapshot::InstalledUniverse::decode(archive.bytes())?.own)
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
/// The address of `package`'s universe summaries under one engine
/// environment and the exact sources of its dependency closure. An importer
/// computes it from the sources it loaded, without reading any row, and so
/// never reads a summary published for other sources.
pub fn universe_query(
    environment: ContentHash,
    package: &PackageId,
    sources: ContentHash,
) -> QueryId {
    QueryId {
        kind: FRONT_RUN_KIND,
        address: hirsel_artifact_store::Address(ContentHash::of_parts(
            "hirsel.front.universe.address.v3",
            [
                environment.0.as_slice(),
                package.name.as_bytes(),
                &[0],
                package.version.as_bytes(),
                sources.0.as_slice(),
            ],
        )),
    }
}
/// The action that produced `record` as the summary `query` addresses.
pub(crate) fn universe_action(query: QueryId, record: Reference) -> hirsel_artifact_store::Action {
    hirsel_artifact_store::Action(ContentHash::of_parts(
        "hirsel.front.universe.action.v1",
        [query.address.0.0.as_slice(), record.output.0.0.as_slice()],
    ))
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
        let universe = queries
            .front
            .signatures()
            .installed_universe(&self.package)?;
        let bytes = universe.encode().ok_or_else(|| {
            operational("a package universe with signature errors is not published")
        })?;
        let record = queries
            .store
            .put_content(UNIVERSE, &bytes)
            .ok_or_else(|| operational("universe summary exceeds transport admission"))?;
        let action = universe_action(self.query, record);
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
fn preparation_query(query: QueryId) -> QueryId {
    QueryId {
        kind: query.kind,
        address: hirsel_artifact_store::Address(ContentHash::of_parts(
            "hirsel.front.precompute-preparation.address.v1",
            [query.address.0.0.as_slice()],
        )),
    }
}
pub(super) fn create(queries: &Arc<FrontRunQueries>, query: QueryId) -> Option<Box<dyn QueryTask>> {
    let root = queries.precompute_query()?;
    if query == root {
        return Some(Box::new(acceptance::Task::new(queries.clone(), query)));
    }
    if query != preparation_query(root) {
        return None;
    }
    Some(Box::new(Task {
        queries: queries.clone(),
        query,
        index: 0,
        digest: ContentHash::default(),
        waiting: None,
        terminal: None,
    }))
}
struct Task {
    queries: Arc<FrontRunQueries>,
    query: QueryId,
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
                self.digest = ContentHash::of_parts(
                    "hirsel.front.precompute.fold.v2",
                    [self.digest.0.as_slice(), product.action.0.0.as_slice()],
                );
            }
            drop(product);
            cx.consume(query);
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
            drop(product);
            cx.consume(query);
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
            queries
                .store
                .associate_product(product.address, product.action, record);
            Ok(product)
        })())
    }
}
