//! Source-only call SCCs and independently addressed settled GEN-5 facets.
//! Membership uses hirsel-graph's iterative Tarjan walk, also used by the
//! scalar source oracle. Native instance/layout graphs never select roots.
use crate::store::{Action, Address, Reference};
use crate::{
    CheckedSummary, NativeRecordContext, NativeRecordSource,
    memo::Reads,
    runtime::{MemoryReservation, ProductRef},
};
use hirsel_span::Span;
use hirsel_syntax::{ContentHash, DefId};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

/// Content-derived identity of a live source facet. Its output observes only
/// the settled value; changed actions with equal outputs stop propagation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceGrowthIdentity {
    pub address: Address,
    pub action: Action,
    pub output: ContentHash,
}

#[derive(Debug, serde::Serialize)]
pub struct SourceGrowthComponent {
    #[serde(skip)]
    pub product: SourceGrowthIdentity,
    pub members: Box<[DefId]>,
    pub growing: Box<[Span]>,
}

#[derive(serde::Serialize)]
pub struct SourceGrowthFacets {
    #[serde(skip)]
    pub membership: SourceGrowthIdentity,
    pub components: Vec<SourceGrowthComponent>,
    #[serde(skip)]
    _reservation: MemoryReservation,
}

#[derive(serde::Serialize)]
pub(crate) struct SourceGrowthData {
    pub facets: Arc<SourceGrowthFacets>,
    pub diagnostics: Arc<Vec<hirsel_diag::Diagnostic>>,
    pub reads: Arc<Reads>,
}

/// Accepted typed call/growth facts retained by the admitted source task.
/// Component actions bind their complete observations without an archive.
pub struct SourceGrowthFact {
    owner: DefId,
    calls: Box<[DefId]>,
    summary: Arc<CheckedSummary>,
    definition: Address,
    selected: SourceGrowthSelection,
    observed: ContentHash,
}

#[derive(Clone, Copy, serde::Serialize)]
pub(crate) enum SourceGrowthSelection {
    Live(ContentHash),
    Archived(Reference),
    Failed(Reference),
}
impl From<Result<ProductRef, Reference>> for SourceGrowthSelection {
    fn from(selected: Result<ProductRef, Reference>) -> Self {
        match selected {
            Ok(product) => Self::Live(product.action.0),
            Err(reference) => Self::Failed(reference),
        }
    }
}

impl SourceGrowthFact {
    /// Physical fact backing retained by an admitted source-growth task.
    pub fn input_bytes(&self) -> u64 {
        (std::mem::size_of::<Self>() + std::mem::size_of_val(&*self.calls)) as u64
    }
}

/// Bounded producer of source growth products. Supply the exact accepted source
/// context and an environment/root namespace. Every required root must contribute
/// a settled checked summary before `settle` is used as a program verdict.
pub struct SourceGrowthProducer<'a> {
    pub source: &'a NativeRecordContext,
    pub output_pool: &'a crate::runtime::MemoryReservationPool,
    pub namespace: Address,
    pub scratch: u64,
}

impl SourceGrowthProducer<'_> {
    /// Called only with a completed checked-stage output, never with a
    /// provisional recursive summary. All observations remain with the root.
    pub fn fact(
        &self,
        definition: Address,
        summary: Arc<CheckedSummary>,
        reads: &Reads,
        selected: Reference,
    ) -> Option<SourceGrowthFact> {
        let read_bytes = reads.iter().try_fold(8u64, |bytes, (key, _)| {
            bytes
                .checked_add(key.encoded_bytes()? as u64)?
                .checked_add(32)
        })?;
        let indices = reads
            .len()
            .checked_mul(std::mem::size_of::<std::ops::Range<usize>>())?
            as u64;
        if read_bytes.checked_mul(2)?.checked_add(indices)? > self.scratch {
            return None;
        }
        self.fact_observed(
            definition,
            summary,
            SourceGrowthSelection::Archived(selected),
            crate::store::payload_reads_hash(reads),
        )
    }

    /// P actions already bind the complete recorded reads through query_action;
    /// failed P records embed those reads. Their accepted witnesses avoid
    /// re-encoding a shared read universe for every source declaration.
    pub(crate) fn fact_observed(
        &self,
        definition: Address,
        summary: Arc<CheckedSummary>,
        selected: SourceGrowthSelection,
        observed: ContentHash,
    ) -> Option<SourceGrowthFact> {
        let count = summary
            .summary
            .calls
            .len()
            .checked_add(summary.growth.edges.len())?
            .checked_add(1)?;
        if (count as u64).checked_mul(512)? > self.scratch {
            return None;
        }
        let owner = summary.summary.owner;
        let calls: Box<[_]> = summary
            .summary
            .calls
            .iter()
            .map(|call| call.callee)
            .chain(summary.growth.edges.iter().map(|edge| edge.1.0))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        Some(SourceGrowthFact {
            owner,
            calls,
            summary,
            definition,
            selected,
            observed,
        })
    }

    /// Consume the accepted facts and settle source call SCCs into one admitted
    /// live facet owner. No store or archive participates in local acceptance.
    pub fn settle(&self, mut facts: Vec<SourceGrowthFact>) -> Option<SourceGrowthFacets> {
        let spare = facts
            .capacity()
            .checked_sub(facts.len())?
            .checked_mul(std::mem::size_of::<SourceGrowthFact>())?;
        let fact_bytes = facts
            .iter()
            .try_fold(spare as u64, |n, f| n.checked_add(f.input_bytes()))?;
        let mut fact_reservation = self.output_pool.take(fact_bytes)?;
        facts.sort_by_key(|fact| (fact.owner, fact.definition.0.0));
        let mut nodes = BTreeSet::new();
        for node in facts
            .iter()
            .map(|fact| fact.owner)
            .chain(facts.iter().flat_map(|fact| fact.calls.iter().copied()))
        {
            if !nodes.contains(&node) {
                let mut weight = crate::NativeMetadataWeight::default();
                weight.tree::<DefId, ()>(nodes.len().checked_add(1)?);
                if weight.bytes() > self.scratch {
                    return None;
                }
                nodes.insert(node);
            }
        }
        let edges = facts
            .iter()
            .try_fold(0usize, |n, fact| n.checked_add(fact.calls.len()))?;
        if graph_bytes(nodes.len(), edges, facts.len())? > self.scratch {
            return None;
        }
        let nodes: Vec<_> = nodes.into_iter().collect();
        let numbers: BTreeMap<_, _> = nodes.iter().enumerate().map(|(i, id)| (*id, i)).collect();
        let mut graph = vec![vec![]; nodes.len()];
        for fact in &facts {
            graph[numbers[&fact.owner]].extend(fact.calls.iter().map(|id| numbers[id]));
        }
        // hirsel-graph lib.rs:170, the existing iterative Tarjan implementation.
        let mut membership: Vec<Vec<DefId>> = hirsel_graph::components(&graph)
            .into_iter()
            .map(|members| members.into_iter().map(|i| nodes[i]).collect())
            .collect();
        membership.sort();
        drop(graph);
        drop(numbers);
        drop(nodes);
        let topology: Vec<_> = facts.iter().map(|fact| (fact.owner, &fact.calls)).collect();
        let membership_product = SourceGrowthIdentity {
            address: Address(ContentHash::of_parts(
                "hirsel.source-growth.membership.address.v1",
                [self.namespace.0.0.as_slice()],
            )),
            action: Action(hash_value(
                "hirsel.source-growth.membership.action.v1",
                &topology,
            )?),
            output: hash_value("hirsel.source-growth.membership.value.v1", &membership)?,
        };
        drop(topology);
        // Move off the all-facts vector before solving any components, so its
        // allocation is actually freed. Only compact SCC membership remains.
        let mut counts = BTreeMap::<DefId, usize>::new();
        for fact in &facts {
            *counts.entry(fact.owner).or_default() += 1;
        }
        let mut by_owner: BTreeMap<DefId, Vec<SourceGrowthFact>> = counts
            .into_iter()
            .map(|(owner, count)| (owner, Vec::with_capacity(count)))
            .collect();
        for fact in facts {
            by_owner.get_mut(&fact.owner)?.push(fact);
        }
        let mut by_owner: BTreeMap<_, _> = by_owner
            .into_iter()
            .map(|(id, facts)| {
                let facts = facts.into_boxed_slice();
                let bytes = facts.iter().map(SourceGrowthFact::input_bytes).sum();
                Some((id, (facts, fact_reservation.split(bytes)?)))
            })
            .collect::<Option<_>>()?;
        drop(fact_reservation);
        let component_count = membership.len();
        let base = std::mem::size_of::<SourceGrowthFacets>() + 2 * std::mem::size_of::<usize>();
        let mut reservation = self.output_pool.take(base as u64)?;
        let mut components: Vec<SourceGrowthComponent> = Vec::new();
        for members in membership {
            let member_hash = hash_value("hirsel.source-growth.members.v1", &members)?;
            let address = Address(ContentHash::of_parts(
                "hirsel.source-growth.component.address.v1",
                [self.namespace.0.0.as_slice(), member_hash.0.as_slice()],
            ));
            let selected_facts: Vec<_> = members
                .iter()
                .filter_map(|id| by_owner.remove(id))
                .collect();
            let observations: Vec<_> = selected_facts
                .iter()
                .flat_map(|(facts, _)| facts.iter())
                .map(|fact| {
                    (
                        fact.definition.0,
                        fact.selected,
                        fact.observed,
                        fact.owner,
                        &fact.calls,
                        &fact.summary.growth,
                    )
                })
                .collect();
            let observed = hash_source_value(
                self.source,
                "hirsel.source-growth.observations.v1",
                &observations,
            )?;
            let action = Action(ContentHash::of_parts(
                "hirsel.source-growth.component.action.v4",
                [member_hash.0.as_slice(), observed.0.as_slice()],
            ));
            drop(observations);
            let selected: BTreeSet<_> = members.iter().copied().collect();
            let edges: Vec<_> = selected_facts
                .iter()
                .flat_map(|(facts, _)| facts.iter())
                .flat_map(|fact| fact.summary.growth.edges.iter())
                .filter(|edge| selected.contains(&edge.1.0))
                .copied()
                .collect();
            let mut growing = crate::growing_sites(&edges);
            growing.sort_by_cached_key(|site| {
                (self.source.record_owner(site.anchor), site.start, site.end)
            });
            drop(edges);
            // Drop the physical facts and their matching charge before admitting
            // this component's owned members and growing sites.
            drop(selected_facts);
            let members = members.into_boxed_slice();
            let growing = growing.into_boxed_slice();
            let output = hash_source_value(
                self.source,
                "hirsel.source-growth.component.value.v1",
                &(&members, &growing),
            )?;
            let next_capacity = if components.len() == components.capacity() {
                components
                    .capacity()
                    .max(2)
                    .checked_mul(2)?
                    .min(component_count)
            } else {
                components.capacity()
            };
            let slots = next_capacity.checked_sub(components.capacity())?;
            let bytes = std::mem::size_of_val(&*members)
                .checked_add(std::mem::size_of_val(&*growing))?
                .checked_add(slots.checked_mul(std::mem::size_of::<SourceGrowthComponent>())?)?;
            reservation
                .join(self.output_pool.take(bytes as u64)?)
                .ok()?;
            // Admit new vector backing only as consumed facts make room. Bound
            // its final capacity by the known SCC count rather than doubling
            // the retained allocation for a non-power-of-two root set.
            components.try_reserve_exact(slots).ok()?;
            components.push(SourceGrowthComponent {
                product: SourceGrowthIdentity {
                    address,
                    action,
                    output,
                },
                members,
                growing,
            });
        }
        Some(SourceGrowthFacets {
            membership: membership_product,
            components,
            _reservation: reservation,
        })
    }
}

/// Bincode's ordinary fixed-width serialization streams straight into SHA-256.
/// Source spans below name lexical owners, never process-local anchor numbers.
pub(crate) fn hash_value(domain: &str, value: &impl serde::Serialize) -> Option<ContentHash> {
    let size = bincode::serialized_size(value).ok()?;
    ContentHash::of_stream(domain, size, |writer| {
        bincode::serialize_into(writer, value).map_err(std::io::Error::other)
    })
    .ok()
}
pub(crate) fn hash_source_value(
    source: &NativeRecordContext,
    domain: &str,
    value: &impl serde::Serialize,
) -> Option<ContentHash> {
    let mut codec = hirsel_span::AnchorCodec::Collect(Vec::new());
    hirsel_span::with_anchor_codec(&mut codec, || bincode::serialized_size(value)).ok()?;
    let hirsel_span::AnchorCodec::Collect(anchors) = codec else {
        unreachable!()
    };
    let owners = anchors
        .iter()
        .map(|anchor| source.record_owner(*anchor))
        .collect::<Option<Vec<_>>>()?;
    let mut codec = hirsel_span::AnchorCodec::Encode(
        anchors
            .into_iter()
            .enumerate()
            .map(|(i, anchor)| (anchor, i as u32 + 1))
            .collect(),
    );
    hirsel_span::with_anchor_codec(&mut codec, || hash_value(domain, &(&owners, value)))
}

// Bounds the live containers of hirsel-graph's iterative Tarjan walk at
// lib.rs:170, including successor clones and Vec geometric spare capacity.
// Calls are edges, not separately allocated graph vertices. Equations stay
// in the front owner's existing reservation; graph backing is dropped before
// component solving and record encoding.
fn graph_bytes(nodes: usize, edges: usize, facts: usize) -> Option<u64> {
    let twice = nodes.checked_mul(2)?.checked_add(4)?;
    let members = nodes.checked_mul(6)?.checked_add(4)?;
    let adjacency = edges.checked_mul(2)?.checked_add(facts.checked_mul(4)?)?;
    let mut weight = crate::NativeMetadataWeight::default();
    weight.tree::<DefId, ()>(nodes);
    weight.tree::<DefId, usize>(nodes);
    weight.tree::<usize, (usize, usize)>(nodes);
    weight.tree::<usize, ()>(nodes);
    weight.tree::<DefId, Vec<SourceGrowthFact>>(facts);
    weight.add(facts.checked_mul(std::mem::size_of::<SourceGrowthFact>())?);
    for (count, size) in [
        (nodes, std::mem::size_of::<DefId>()),
        (nodes, std::mem::size_of::<Vec<usize>>()),
        (adjacency, std::mem::size_of::<usize>()),
        (edges, std::mem::size_of::<usize>()),
        (facts, std::mem::size_of::<&SourceGrowthFact>()),
        (twice, std::mem::size_of::<(usize, Vec<usize>, usize)>()),
        (twice, std::mem::size_of::<usize>()),
        (nodes, std::mem::size_of::<usize>()),
        (twice, std::mem::size_of::<Vec<usize>>()),
        (members, std::mem::size_of::<usize>()),
        (nodes, std::mem::size_of::<Vec<DefId>>()),
        (
            nodes.checked_mul(8)?.checked_add(4)?,
            std::mem::size_of::<DefId>(),
        ),
    ] {
        weight.add(count.checked_mul(size)?);
    }
    Some(weight.bytes())
}

#[cfg(test)]
mod tests;
