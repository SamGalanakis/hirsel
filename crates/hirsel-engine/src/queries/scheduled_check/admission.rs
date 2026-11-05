//! One bounded body window, independent of source-wide acceptance reductions.
//! Front/Lower owners retain observations for the ordered Check publisher.
use super::*;
use std::collections::VecDeque;

enum RootRead {
    Front,
    Lower {
        front: ContentHash,
    },
    Settled {
        front: ContentHash,
        lower: Option<ContentHash>,
    },
}
impl RootRead {
    fn front_done(self, front: ContentHash, lower: bool) -> Self {
        if lower {
            Self::Lower { front }
        } else {
            Self::Settled { front, lower: None }
        }
    }
    fn poll(
        self,
        queries: &FrontCheckQueries,
        cx: &mut dyn QueryContext,
        file: usize,
        root: usize,
    ) -> Result<(Self, Option<QueryId>), Reference> {
        let input = &queries.files[file];
        let (path, executable) = &input.paths[root];
        match self {
            Self::Front => {
                let id = queries
                    .plan
                    .front
                    .query(path)
                    .ok_or(queries.errors.protocol)?;
                let selected = match cx.read(id) {
                    Poll::Pending => return Ok((self, Some(id))),
                    Poll::Ready(selected) => selected,
                };
                let hash = selected_hash(&selected);
                let facts = queries
                    .plan
                    .front
                    .check_facts(selected.clone())
                    .ok_or_else(|| selected.err().unwrap_or(queries.errors.protocol))?;
                let lower = *executable
                    && facts.result.as_ref().is_ok_and(|d| !has_errors(d))
                    && (input.lowering == LoweringPolicy::Reachable
                        || facts.lowering.as_ref().is_some_and(|lowering| {
                            lowering.needed_with(|id| queries.constants.contains(&id))
                        }));
                cx.consume(id);
                let next = self.front_done(hash, lower);
                if lower {
                    next.poll(queries, cx, file, root)
                } else {
                    Ok((next, None))
                }
            }
            Self::Lower { front } => {
                let id = queries
                    .plan
                    .lower
                    .query(path)
                    .ok_or(queries.errors.protocol)?;
                let selected = match cx.read(id) {
                    Poll::Pending => return Ok((self, Some(id))),
                    Poll::Ready(selected) => selected,
                };
                let lower = selected_hash(&selected);
                cx.consume(id);
                Ok((
                    Self::Settled {
                        front,
                        lower: Some(lower),
                    },
                    None,
                ))
            }
            Self::Settled { .. } => Ok((self, None)),
        }
    }
}
fn selected_hash(selected: &Result<ProductRef, Reference>) -> ContentHash {
    match selected {
        Ok(product) => product.action.0,
        Err(reference) => reference.output.0,
    }
}
enum AdmissionState {
    Roots {
        file: usize,
        root: usize,
        read: VecDeque<RootRead>,
    },
    Instances {
        file: usize,
        paths: Vec<DefPath>,
        next: usize,
    },
    Staged {
        file: usize,
        selections: Vec<super::super::scheduled_lower::staged_runtime::Selection>,
        next: usize,
    },
    Extra {
        next: usize,
    },
    Finished(Result<ProductRef, Reference>),
}
impl AdmissionState {
    fn enter_file(queries: &FrontCheckQueries, mut file: usize) -> Self {
        while let Some(input) = queries.files.get(file) {
            if input.error.is_none() && !has_errors(&input.diagnostics) {
                return Self::Roots {
                    file,
                    root: 0,
                    read: VecDeque::new(),
                };
            }
            file += 1;
        }
        Self::Extra { next: 0 }
    }
    fn root_done(self) -> Self {
        let Self::Roots {
            file,
            root,
            mut read,
        } = self
        else {
            unreachable!()
        };
        read.pop_front();
        Self::Roots {
            file,
            root: root + 1,
            read,
        }
    }
    fn instances_selected(self, paths: Vec<DefPath>) -> Self {
        let Self::Roots { file, .. } = self else {
            unreachable!()
        };
        Self::Instances {
            file,
            paths,
            next: 0,
        }
    }
    fn instances_done(self, queries: &FrontCheckQueries) -> Self {
        let Self::Instances { file, .. } = self else {
            unreachable!()
        };
        let input = &queries.files[file];
        let selections = queries
            .plan
            .lower
            .runtime_staged_queries(
                input.paths.iter().map(|(path, _)| path.clone()),
                queries.weight.scratch,
                queries.weight.retained,
            )
            .unwrap_or_default();
        Self::staged_selected(file, selections)
    }
    fn staged_selected(
        file: usize,
        selections: Vec<super::super::scheduled_lower::staged_runtime::Selection>,
    ) -> Self {
        Self::Staged {
            file,
            selections,
            next: 0,
        }
    }
    fn staged_done(self, queries: &FrontCheckQueries) -> Self {
        let Self::Staged { file, .. } = self else {
            unreachable!()
        };
        Self::enter_file(queries, file + 1)
    }
    fn finish(self, result: Result<ProductRef, Reference>) -> Self {
        Self::Finished(result)
    }
}
pub(super) struct Task {
    queries: Arc<FrontCheckQueries>,
    state: AdmissionState,
    digest: ContentHash,
}
impl Task {
    pub(super) fn new(queries: Arc<FrontCheckQueries>) -> Self {
        Self {
            state: AdmissionState::enter_file(&queries, 0),
            queries,
            digest: ContentHash::of_bytes(b"source admission"),
        }
    }
    fn read(
        &mut self,
        cx: &mut dyn QueryContext,
        id: QueryId,
    ) -> Poll<Result<ProductRef, Reference>> {
        let result = cx.read(id);
        if let Poll::Ready(selected) = &result {
            self.fold(id, selected_hash(selected));
            cx.consume(id);
        }
        result
    }
    fn fold(&mut self, id: QueryId, hash: ContentHash) {
        self.digest = ContentHash::of_parts(
            "hirsel.front.check-admission.fold.v1",
            [
                self.digest.0.as_slice(),
                id.address.0.0.as_slice(),
                hash.0.as_slice(),
            ],
        );
    }
    fn advance(&mut self, cx: &mut dyn QueryContext) -> Result<Option<Vec<QueryId>>, Reference> {
        loop {
            let queries = self.queries.clone();
            match &self.state {
                AdmissionState::Roots { .. } => {
                    let AdmissionState::Roots {
                        file,
                        root,
                        mut read,
                    } = std::mem::replace(
                        &mut self.state,
                        AdmissionState::Finished(Err(queries.errors.protocol)),
                    )
                    else {
                        unreachable!()
                    };
                    let input = &queries.files[file];
                    if root == input.paths.len() && read.is_empty() {
                        let paths = queries
                            .plan
                            .lower
                            .selected_instances(
                                input.paths.iter().map(|(path, _)| path.clone()),
                                queries.weight.scratch,
                            )
                            .map(|paths| paths.into_iter().collect())
                            .unwrap_or_default();
                        self.state =
                            AdmissionState::Roots { file, root, read }.instances_selected(paths);
                        continue;
                    }
                    // Hashes, not bodies, survive out-of-order completion. The
                    // window fits the existing continuation grant and worker count.
                    let slots = usize::try_from(
                        queries.weight.retained.saturating_sub(std::mem::size_of::<Self>() as u64)
                            / (std::mem::size_of::<RootRead>() + std::mem::size_of::<QueryId>()) as u64,
                    )
                    .unwrap_or(usize::MAX);
                    let width = cx.workers().max(1).min(slots);
                    if width == 0 {
                        return Err(queries.errors.resource);
                    }
                    if read.capacity() < width {
                        read.reserve_exact(width - read.len());
                    }
                    while read.len() < width && root + read.len() < input.paths.len() {
                        read.push_back(RootRead::Front);
                    }
                    let mut pending = Vec::with_capacity(width);
                    for (offset, phase) in read.iter_mut().enumerate() {
                        let current = std::mem::replace(phase, RootRead::Front);
                        let (next, waiting) = current.poll(&queries, cx, file, root + offset)?;
                        *phase = next;
                        if let Some(id) = waiting {
                            pending.push(id);
                        }
                    }
                    self.state = AdmissionState::Roots { file, root, read };
                    // Fold in the original Front/Lower source order, even when
                    // later roots return first. Release each body when read.
                    loop {
                        let next = match &self.state {
                            AdmissionState::Roots { file, root, read } => match read.front() {
                                Some(RootRead::Settled { front, lower }) => {
                                    Some((*file, *root, *front, *lower))
                                }
                                _ => None,
                            },
                            _ => None,
                        };
                        let Some((file, root, front, lower)) = next else {
                            break;
                        };
                        let path = &queries.files[file].paths[root].0;
                        self.fold(
                            queries
                                .plan
                                .front
                                .query(path)
                                .ok_or(queries.errors.protocol)?,
                            front,
                        );
                        if let Some(lower) = lower {
                            self.fold(
                                queries
                                    .plan
                                    .lower
                                    .query(path)
                                    .ok_or(queries.errors.protocol)?,
                                lower,
                            );
                        }
                        self.transition(AdmissionState::root_done);
                    }
                    if !pending.is_empty() {
                        return Ok(Some(pending));
                    }
                }
                AdmissionState::Instances { paths, next, .. } => {
                    let Some(path) = paths.get(*next) else {
                        self.transition(|state| state.instances_done(&queries));
                        continue;
                    };
                    let id = queries
                        .plan
                        .lower
                        .query(path)
                        .ok_or(queries.errors.protocol)?;
                    if self.read(cx, id).is_pending() {
                        return Ok(Some(vec![id]));
                    }
                    let AdmissionState::Instances { next, .. } = &mut self.state else {
                        unreachable!()
                    };
                    *next += 1;
                }
                AdmissionState::Staged {
                    selections, next, ..
                } => {
                    let Some((id, _)) = selections.get(*next).copied() else {
                        self.transition(|state| state.staged_done(&queries));
                        continue;
                    };
                    if self.read(cx, id).is_pending() {
                        return Ok(Some(vec![id]));
                    }
                    let AdmissionState::Staged { next, .. } = &mut self.state else {
                        unreachable!()
                    };
                    *next += 1;
                }
                AdmissionState::Extra { next } => {
                    let Some(path) = queries.instances.get(*next) else {
                        return Ok(None);
                    };
                    let id = queries
                        .plan
                        .front
                        .query(path)
                        .ok_or(queries.errors.protocol)?;
                    if self.read(cx, id).is_pending() {
                        return Ok(Some(vec![id]));
                    }
                    let AdmissionState::Extra { next } = &mut self.state else {
                        unreachable!()
                    };
                    *next += 1;
                }
                AdmissionState::Finished(_) => return Ok(None),
            }
        }
    }
    fn transition(&mut self, change: impl FnOnce(AdmissionState) -> AdmissionState) {
        let state = std::mem::replace(
            &mut self.state,
            AdmissionState::Finished(Err(self.queries.errors.protocol)),
        );
        self.state = change(state);
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
        if let AdmissionState::Finished(result) = &self.state {
            return finish(result.clone());
        }
        let result = match self.advance(cx) {
            Ok(Some(ids)) => return QueryPoll::WaitingAny(ids),
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
        self.transition(|state| state.finish(result.clone()));
        finish(result)
    }
}
