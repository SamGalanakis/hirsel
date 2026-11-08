//! One source root's reads, in source order: Front, then Lower when the root
//! lowers.
use super::super::scheduled_lower::check::CheckFacts;
use super::*;

/// One root's Front read and, when it lowers, its Lower check facts.
pub(super) struct RootRead {
    pub(super) front: Option<Result<ProductRef, Reference>>,
    pub(super) lower: Option<CheckFacts>,
}
pub(super) fn read_root(
    queries: &FrontCheckQueries,
    cx: &mut dyn QueryContext,
    input: &FileInput,
    root: usize,
    stopped: bool,
) -> Result<RootRead, Reference> {
    let work = &input.paths[root];
    if work.ordered_front && stopped {
        return Ok(RootRead {
            front: None,
            lower: None,
        });
    }
    let id = queries
        .plan
        .front
        .query(&work.path)
        .ok_or(queries.errors.protocol)?;
    let selected = cx.read(id);
    let facts = queries
        .plan
        .front
        .check_facts(selected.clone())
        .ok_or_else(|| {
            selected.clone().err().unwrap_or_else(|| {
                crate::requests::source_graph::protocol_failure(
                    &*queries.store,
                    queries.errors,
                    Some(id),
                    &format!(
                        "Front checked facts for {} are unavailable for the selected product",
                        work.path
                    ),
                )
            })
        })?;
    let lower = !stopped
        && work.executable
        && facts.result.as_ref().is_ok_and(|d| !has_errors(d))
        && (input.lowering == LoweringPolicy::Reachable
            || facts.lowering.as_ref().is_some_and(|lowering| {
                lowering.needed_with(|id| queries.constants.contains(&id))
            }));
    let lower = if lower {
        let id = queries
            .plan
            .lower
            .query(&work.path)
            .ok_or(queries.errors.protocol)?;
        Some(queries.plan.lower.poll_check(cx, id)?)
    } else {
        None
    };
    Ok(RootRead {
        front: Some(selected),
        lower,
    })
}
