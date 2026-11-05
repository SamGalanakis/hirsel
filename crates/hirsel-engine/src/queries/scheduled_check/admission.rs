//! One bounded body window, independent of source-wide acceptance reductions.
//! Front/Lower owners retain observations for the ordered Check publisher.
use super::*;

enum RootRead {
    Front,
    Lower,
}
enum AdmissionState {
    Roots {
        file: usize,
        root: usize,
        read: RootRead,
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
                    read: RootRead::Front,
                };
            }
            file += 1;
        }
        Self::Extra { next: 0 }
    }
    fn front_done(self, lower: bool) -> Self {
        if lower {
            let Self::Roots { file, root, .. } = self else {
                unreachable!()
            };
            Self::Roots {
                file,
                root,
                read: RootRead::Lower,
            }
        } else {
            self.root_done()
        }
    }
    fn root_done(self) -> Self {
        let Self::Roots { file, root, .. } = self else {
            unreachable!()
        };
        Self::Roots {
            file,
            root: root + 1,
            read: RootRead::Front,
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
        loop {
            let queries = self.queries.clone();
            match &self.state {
                AdmissionState::Roots { file, root, read } => {
                    let input = &queries.files[*file];
                    let Some((path, executable)) = input.paths.get(*root) else {
                        let paths = queries
                            .plan
                            .lower
                            .selected_instances(
                                input.paths.iter().map(|(path, _)| path.clone()),
                                queries.weight.scratch,
                            )
                            .map(|paths| paths.into_iter().collect())
                            .unwrap_or_default();
                        self.transition(|state| state.instances_selected(paths));
                        continue;
                    };
                    let front = matches!(read, RootRead::Front);
                    let id = if front {
                        queries.plan.front.query(path)
                    } else {
                        queries.plan.lower.query(path)
                    }
                    .ok_or(queries.errors.protocol)?;
                    let executable = *executable;
                    let lowering = input.lowering;
                    let selected = match self.read(cx, id) {
                        Poll::Pending => return Ok(Some(id)),
                        Poll::Ready(selected) => selected,
                    };
                    if front {
                        let facts = queries
                            .plan
                            .front
                            .check_facts(selected.clone())
                            .ok_or_else(|| selected.err().unwrap_or(queries.errors.protocol))?;
                        let lower = executable
                            && facts.result.as_ref().is_ok_and(|d| !has_errors(d))
                            && (lowering == LoweringPolicy::Reachable
                                || facts.lowering.as_ref().is_some_and(|lowering| {
                                    lowering.needed_with(|id| queries.constants.contains(&id))
                                }));
                        self.transition(|state| state.front_done(lower));
                    } else {
                        self.transition(AdmissionState::root_done);
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
                        return Ok(Some(id));
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
                        return Ok(Some(id));
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
                        return Ok(Some(id));
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
        self.transition(|state| state.finish(result.clone()));
        finish(result)
    }
}
