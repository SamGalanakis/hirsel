//! The publication cursor's bounded speculative window. Effectful roots read
//! only after their source predecessors have been accepted by that cursor.
use super::super::scheduled_lower::check::CheckFacts;
use super::*;

pub(super) enum RootRead {
    Front,
    Lower {
        front: Result<ProductRef, Reference>,
    },
    Settled {
        front: Option<Result<ProductRef, Reference>>,
        lower: Option<CheckFacts>,
    },
}
impl RootRead {
    pub(super) fn settled(&self) -> bool {
        matches!(self, Self::Settled { .. })
    }
    pub(super) fn poll(
        &mut self,
        queries: &FrontCheckQueries,
        cx: &mut dyn QueryContext,
        input: &FileInput,
        root: usize,
        predecessor: bool,
        stopped: bool,
    ) -> Result<Option<QueryId>, Reference> {
        let work = &input.paths[root];
        if matches!(self, Self::Front) {
            if work.ordered_front {
                if stopped {
                    *self = Self::Settled {
                        front: None,
                        lower: None,
                    };
                    return Ok(None);
                }
                if !predecessor {
                    return Ok(None);
                }
            }
            let id = queries
                .plan
                .front
                .query(&work.path)
                .ok_or(queries.errors.protocol)?;
            let selected = match cx.read(id) {
                Poll::Pending => return Ok(Some(id)),
                Poll::Ready(selected) => selected,
            };
            let facts = queries
                .plan
                .front
                .check_facts(selected.clone())
                .ok_or_else(|| selected.clone().err().unwrap_or_else(|| {
                    crate::requests::source_graph::protocol_failure(
                        &*queries.store,
                        queries.errors,
                        Some(id),
                        &format!("Front checked facts for {} are unavailable for the selected product", work.path),
                    )
                }))?;
            let lower = !stopped
                && work.executable
                && facts.result.as_ref().is_ok_and(|d| !has_errors(d))
                && (input.lowering == LoweringPolicy::Reachable
                    || facts.lowering.as_ref().is_some_and(|lowering| {
                        lowering.needed_with(|id| queries.constants.contains(&id))
                    }));
            *self = if lower {
                Self::Lower { front: selected }
            } else {
                Self::Settled {
                    front: Some(selected),
                    lower: None,
                }
            };
        }
        if let Self::Lower { front } = self {
            if stopped {
                *self = Self::Settled {
                    front: Some(front.clone()),
                    lower: None,
                };
                return Ok(None);
            }
            if !predecessor && !can_overlap(queries, &work.path)? {
                return Ok(None);
            }
            let id = queries
                .plan
                .lower
                .query(&work.path)
                .ok_or(queries.errors.protocol)?;
            let lower = match queries.plan.lower.poll_check(cx, id) {
                Poll::Pending => return Ok(Some(id)),
                Poll::Ready(facts) => facts?,
            };
            *self = Self::Settled {
                front: Some(front.clone()),
                lower: Some(lower),
            };
        }
        Ok(None)
    }
}

// Unknown or declaration-producing dependencies require ordered admission.
// Completed compact P classifications suffice; this walk executes no producer.
fn can_overlap(queries: &FrontCheckQueries, root: &DefPath) -> Result<bool, Reference> {
    let mut pending = vec![DefId::intern(root)];
    let mut seen = BTreeSet::new();
    while let Some(id) = pending.pop() {
        if !seen.insert(id) {
            continue;
        }
        let Some(work) = queries
            .files
            .iter()
            .flat_map(|file| &file.paths)
            .find(|work| work.path == *id.path())
        else {
            // Immutable installed declarations without a source producer
            // cannot publish a new declaration in this request.
            if queries.plan.front.query(id.path()).is_some() {
                return Ok(false);
            }
            continue;
        };
        if work.ordered_lower {
            return Ok(false);
        }
        let Some(lowering) = queries.plan.front.lowering_inputs(id.path()) else {
            return Ok(false);
        };
        // Metadata is bounded by root scratch, never a traversal cutoff.
        let bytes = (seen.len() + pending.len() + lowering.references().len())
            .checked_mul(4 * size_of::<DefId>())
            .ok_or(queries.errors.resource)?;
        if bytes as u64 > queries.weight.scratch {
            return Err(queries.errors.resource);
        }
        pending.extend(lowering.references().iter().copied());
    }
    Ok(true)
}

pub(super) fn width(cx: &dyn QueryContext, weight: MemoryWeight, task_bytes: usize) -> usize {
    let slots = usize::try_from(
        weight.retained.saturating_sub(task_bytes as u64)
            / (size_of::<RootRead>() + size_of::<QueryId>()) as u64,
    )
    .unwrap_or(usize::MAX);
    cx.workers().max(1).min(slots)
}
