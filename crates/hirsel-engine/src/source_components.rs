//! Source-only call SCCs and independently addressed settled GEN-5 facets.
//! Membership uses hirsel-graph's iterative Tarjan walk, also used by the
//! scalar source oracle. Native instance/layout graphs never select roots.
use crate::CheckedSummary;
use crate::NativeRecordContext;
use crate::NativeRecordSource;
use crate::store::{Action, Address, Reference};
use hirsel_engine_runtime::ProductRef;
use hirsel_query_products::reads::Reads;

use hirsel_span::Span;
use hirsel_syntax::{ContentHash, DefId};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

/// Content-derived identity of a live source facet. Its output observes only
/// the settled value; changed actions with equal outputs stop propagation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(
    from = "(ContentHash, ContentHash, ContentHash)",
    into = "(ContentHash, ContentHash, ContentHash)"
)]
pub struct SourceGrowthIdentity {
    pub address: Address,
    pub action: Action,
    pub output: ContentHash,
}

impl From<(ContentHash, ContentHash, ContentHash)> for SourceGrowthIdentity {
    fn from((address, action, output): (ContentHash, ContentHash, ContentHash)) -> Self {
        Self {
            address: Address(address),
            action: Action(action),
            output,
        }
    }
}
impl From<SourceGrowthIdentity> for (ContentHash, ContentHash, ContentHash) {
    fn from(value: SourceGrowthIdentity) -> Self {
        (value.address.0, value.action.0, value.output)
    }
}

#[derive(serde::Serialize)]
pub struct SourceGrowthComponent {
    pub product: SourceGrowthIdentity,
    pub members: Box<[DefId]>,
    pub growing: Box<[Span]>,
    #[serde(skip)]
    settled_growing: Box<[(DefId, u32, u32, u32)]>,
    #[serde(skip)]
    pub(crate) publication: Option<crate::products::SourceGrowthPublication>,
    #[serde(skip)]
    _reservation: hirsel_engine_admission::MemoryReservation,
}

pub struct SourceGrowthFacets {
    pub membership: SourceGrowthIdentity,
    pub components: SourceGrowthComponents,
    _reservation: hirsel_engine_admission::MemoryReservation,
}
/// Admitted pages retain SCC values independently of scheduler execution slots.
/// Each page owns its backing and directory charge before allocation.
#[derive(Default)]
pub struct SourceGrowthComponents {
    pages: Vec<SourceGrowthPage>,
    len: usize,
    _directory: Option<hirsel_engine_admission::MemoryReservation>,
}
struct SourceGrowthPage {
    values: Vec<Arc<SourceGrowthComponent>>,
    _reservation: hirsel_engine_admission::MemoryReservation,
}
impl SourceGrowthComponents {
    const PAGE: usize = 128;
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn get(&self, index: usize) -> Option<&Arc<SourceGrowthComponent>> {
        if index >= self.len {
            return None;
        }
        self.pages
            .get(index / Self::PAGE)?
            .values
            .get(index % Self::PAGE)
    }
    pub fn iter(&self) -> SourceGrowthIter<'_> {
        SourceGrowthIter {
            pages: self.pages.iter(),
            current: [].iter(),
            remaining: self.len,
        }
    }
    pub(crate) fn push(
        &mut self,
        value: Arc<SourceGrowthComponent>,
        pool: &hirsel_engine_admission::MemoryReservationPool,
        remaining: usize,
    ) -> Option<()> {
        if self
            .pages
            .last()
            .is_none_or(|page| page.values.len() == page.values.capacity())
        {
            let count = remaining.clamp(1, Self::PAGE);
            let bytes = count.checked_mul(size_of::<Arc<SourceGrowthComponent>>())?;
            let reservation = pool.take(bytes as u64)?;
            if self.pages.len() == self.pages.capacity() {
                let capacity = self.pages.capacity().saturating_mul(2).max(4);
                let bytes =
                    (capacity - self.pages.capacity()).checked_mul(size_of::<SourceGrowthPage>())?;
                let directory = pool.take(bytes as u64)?;
                if let Some(held) = &mut self._directory {
                    held.join(directory).ok()?;
                } else {
                    self._directory = Some(directory);
                }
                self.pages.reserve_exact(capacity - self.pages.len());
            }
            self.pages.push(SourceGrowthPage {
                values: Vec::with_capacity(count),
                _reservation: reservation,
            });
        }
        self.pages.last_mut()?.values.push(value);
        self.len += 1;
        Some(())
    }
}
pub struct SourceGrowthIter<'a> {
    pages: std::slice::Iter<'a, SourceGrowthPage>,
    current: std::slice::Iter<'a, Arc<SourceGrowthComponent>>,
    remaining: usize,
}
impl<'a> Iterator for SourceGrowthIter<'a> {
    type Item = &'a Arc<SourceGrowthComponent>;
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(value) = self.current.next() {
                self.remaining -= 1;
                return Some(value);
            }
            self.current = self.pages.next()?.values.iter();
        }
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}
impl ExactSizeIterator for SourceGrowthIter<'_> {}
impl<'a> IntoIterator for &'a SourceGrowthComponents {
    type Item = &'a Arc<SourceGrowthComponent>;
    type IntoIter = SourceGrowthIter<'a>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}
impl std::ops::Index<usize> for SourceGrowthComponents {
    type Output = Arc<SourceGrowthComponent>;
    fn index(&self, index: usize) -> &Self::Output {
        self.get(index).expect("source component index")
    }
}
impl serde::Serialize for SourceGrowthFacets {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut seq = serializer.serialize_seq(Some(self.components.len()))?;
        for component in &self.components {
            seq.serialize_element(&(&component.members, &component.growing))?;
        }
        seq.end()
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct SourceComponentMembers {
    pub address: ContentHash,
    pub members: Box<[DefId]>,
    pub inputs: Box<[ContentHash]>,
}
#[derive(serde::Serialize)]
pub(crate) struct SourceMembership {
    #[serde(skip)]
    pub(crate) publication: Option<crate::products::SourceGrowthPublication>,
    pub product: SourceGrowthIdentity,
    pub components: Box<[SourceComponentMembers]>,
    #[serde(skip)]
    address_order: Box<[u32]>,
    #[serde(skip)]
    _reservation: hirsel_engine_admission::MemoryReservation,
}

impl SourceMembership {
    pub(crate) fn component_index(&self, address: ContentHash) -> Option<usize> {
        let index = self
            .address_order
            .binary_search_by_key(&address, |&i| self.components[i as usize].address)
            .ok()?;
        Some(self.address_order[index] as usize)
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) enum SourceGrowthQueryRecord {
    Membership {
        product: SourceGrowthIdentity,
        components: Box<[SourceComponentMembers]>,
        reads: Arc<Reads>,
    },
    Component {
        product: SourceGrowthIdentity,
        members: Box<[DefId]>,
        growing: Box<[Span]>,
        reads: Arc<Reads>,
    },
}

#[derive(serde::Serialize)]
pub(crate) struct SourceGrowthData {
    pub facets: Arc<SourceGrowthFacets>,
    pub diagnostics: Arc<Vec<hirsel_diag::Diagnostic>>,
    pub reads: Arc<crate::queries::ArchivedReader>,
}

/// Accepted typed call/growth facts retained by the admitted source task.
/// Component actions bind their complete observations without an archive.
pub struct SourceGrowthFact {
    pub(crate) owner: DefId,
    pub(crate) calls: Box<[DefId]>,
    growth: crate::ParameterGrowth,
    pub(crate) definition: Address,
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
        let mut weight = hirsel_query_products::retained::Weight::default();
        weight.add(std::mem::size_of::<Self>() + std::mem::size_of_val(&*self.calls));
        weight.parameter_growth(&self.growth);
        weight.bytes
    }
}

/// Bounded producer of source growth products. Supply the exact accepted source
/// context and an environment/root namespace. Every required root must contribute
/// a settled checked summary before the final acceptance reduction.
pub struct SourceGrowthProducer<'a> {
    pub source: &'a NativeRecordContext,
    pub output_pool: &'a hirsel_engine_admission::MemoryReservationPool,
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
            growth: summary.growth.clone(),
            definition,
            selected,
            observed,
        })
    }

    /// Membership is a query of call topology only. Tarjan's output has no
    /// settled growth verdict and cannot accept a source program.
    pub(crate) fn membership_parts(
        &self,
        facts: &[SourceGrowthFact],
    ) -> Option<(SourceGrowthIdentity, Box<[SourceComponentMembers]>)> {
        let mut nodes = BTreeSet::new();
        for node in facts
            .iter()
            .map(|fact| fact.owner)
            .chain(facts.iter().flat_map(|fact| fact.calls.iter().copied()))
        {
            if !nodes.contains(&node) {
                let mut weight = hirsel_query_products::retained::MetadataWeight::default();
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
        for fact in facts {
            graph[numbers[&fact.owner]].extend(fact.calls.iter().map(|id| numbers[id]));
        }
        // hirsel-graph lib.rs:164: the scalar oracle's iterative Tarjan walk.
        let mut components: Vec<Vec<DefId>> = hirsel_graph::components(&graph)
            .into_iter()
            .map(|members| members.into_iter().map(|i| nodes[i]).collect())
            .collect();
        components.sort();
        let product = SourceGrowthIdentity {
            address: self.membership_address(),
            action: self.membership_action(facts)?,
            output: hash_value("hirsel.source-growth.membership.value.v1", &components)?,
        };
        // Index facts once. Component construction visits only its members'
        // ranges, retaining the original input order even for unordered callers.
        let index_bytes = facts.len().checked_mul(3 * size_of::<usize>())? as u64;
        if graph_bytes(nodes.len(), edges, facts.len())?.checked_add(index_bytes)? > self.scratch {
            return None;
        }
        let mut by_owner: Vec<_> = (0..facts.len()).collect();
        by_owner.sort_unstable_by_key(|&i| (facts[i].owner, i));
        let components = components
            .into_iter()
            .map(|members| {
                let mut positions = Vec::new();
                for member in &members {
                    let start = by_owner.partition_point(|&i| facts[i].owner < *member);
                    let end = by_owner.partition_point(|&i| facts[i].owner <= *member);
                    positions.extend_from_slice(&by_owner[start..end]);
                }
                positions.sort_unstable();
                let inputs = positions
                    .into_iter()
                    .map(|i| facts[i].definition.0)
                    .collect();
                Some(SourceComponentMembers {
                    address: self.component_address(&members)?.0,
                    members: members.into_boxed_slice(),
                    inputs,
                })
            })
            .collect::<Option<Vec<_>>>()?
            .into_boxed_slice();
        Some((product, components))
    }
    pub(crate) fn membership_value(
        &self,
        product: SourceGrowthIdentity,
        components: Box<[SourceComponentMembers]>,
    ) -> Option<SourceMembership> {
        let mut weight = hirsel_query_products::retained::MetadataWeight::default();
        weight.add(std::mem::size_of::<SourceMembership>() + 2 * std::mem::size_of::<usize>());
        weight.add(std::mem::size_of_val(&*components));
        for component in &components {
            weight.add(std::mem::size_of_val(&*component.members));
            weight.add(std::mem::size_of_val(&*component.inputs));
        }
        let count = u32::try_from(components.len()).ok()?;
        weight.add(components.len().checked_mul(size_of::<u32>())?);
        let reservation = self.output_pool.take(weight.bytes())?;
        let mut address_order: Vec<_> = (0..count).collect();
        address_order.sort_unstable_by_key(|&i| components[i as usize].address);
        Some(SourceMembership {
            publication: None,
            product,
            components,
            address_order: address_order.into_boxed_slice(),
            _reservation: reservation,
        })
    }
    pub(crate) fn membership_action(&self, facts: &[SourceGrowthFact]) -> Option<Action> {
        let mut observations: Vec<_> = facts
            .iter()
            .map(|fact| {
                (
                    fact.owner,
                    &fact.calls,
                    fact.definition.0,
                    fact.selected,
                    fact.observed,
                )
            })
            .collect();
        observations.sort_by_key(|row| (row.0, row.2));
        Some(Action(hash_value(
            "hirsel.source-growth.membership.action.v3",
            &observations,
        )?))
    }
    pub(crate) fn membership_address(&self) -> Address {
        Address(ContentHash::of_parts(
            "hirsel.source-growth.membership.address.v1",
            [self.namespace.0.0.as_slice()],
        ))
    }
    fn component_address(&self, members: &[DefId]) -> Option<Address> {
        let members = hash_value("hirsel.source-growth.members.v1", &members)?;
        Some(Address(ContentHash::of_parts(
            "hirsel.source-growth.component.address.v1",
            [self.namespace.0.0.as_slice(), members.0.as_slice()],
        )))
    }
    pub(crate) fn component_action(
        &self,
        members: &[DefId],
        facts: &[SourceGrowthFact],
    ) -> Option<Action> {
        let observations: Vec<_> = facts
            .iter()
            .map(|fact| {
                (
                    fact.definition.0,
                    fact.selected,
                    fact.observed,
                    fact.owner,
                    &fact.calls,
                    &fact.growth,
                )
            })
            .collect();
        Some(Action(hash_source_value(
            self.source,
            "hirsel.source-growth.component.action.v5",
            &(members, observations),
        )?))
    }
    /// Settle exactly one SCC, never any predecessor or unrelated component.
    pub(crate) fn component(
        &self,
        members: &[DefId],
        facts: &[SourceGrowthFact],
    ) -> Option<Arc<SourceGrowthComponent>> {
        let selected: BTreeSet<_> = members.iter().copied().collect();
        let edges: Vec<_> = facts
            .iter()
            .flat_map(|fact| fact.growth.edges.iter())
            .filter(|edge| selected.contains(&edge.1.0))
            .copied()
            .collect();
        let mut growing = crate::growing_sites(&edges);
        growing.sort_by_cached_key(|site| {
            (self.source.record_owner(site.anchor), site.start, site.end)
        });
        self.component_value(
            members.to_vec().into_boxed_slice(),
            growing.into_boxed_slice(),
            self.component_action(members, facts)?,
        )
    }
    pub(crate) fn component_value(
        &self,
        members: Box<[DefId]>,
        growing: Box<[Span]>,
        action: Action,
    ) -> Option<Arc<SourceGrowthComponent>> {
        let product = SourceGrowthIdentity {
            address: self.component_address(&members)?,
            action,
            output: hash_source_value(
                self.source,
                "hirsel.source-growth.component.value.v1",
                &(&members, &growing),
            )?,
        };
        let settled_growing = growing
            .iter()
            .map(|site| {
                let (owner, occurrence) = self.source.record_owner(site.anchor)?;
                Some((DefId::intern(&owner), occurrence, site.start, site.end))
            })
            .collect::<Option<Vec<_>>>()?
            .into_boxed_slice();
        let bytes = (std::mem::size_of::<SourceGrowthComponent>()
            + 2 * std::mem::size_of::<usize>())
        .checked_add(std::mem::size_of_val(&*members))?
        .checked_add(std::mem::size_of_val(&*growing))?
        .checked_add(std::mem::size_of_val(&*settled_growing))?;
        Some(Arc::new(SourceGrowthComponent {
            product,
            members,
            growing,
            settled_growing,
            publication: None,
            _reservation: self.output_pool.take(bytes as u64)?,
        }))
    }
    /// The root reduces already settled component products. It never solves.
    pub(crate) fn accept(
        &self,
        membership: SourceGrowthIdentity,
        components: SourceGrowthComponents,
    ) -> Option<SourceGrowthFacets> {
        let bytes = std::mem::size_of::<SourceGrowthFacets>() + 2 * std::mem::size_of::<usize>();
        Some(SourceGrowthFacets {
            membership,
            components,
            _reservation: self.output_pool.take(bytes as u64)?,
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
// lib.rs:164, including successor clones and Vec geometric spare capacity.
// Calls are edges, not separately allocated graph vertices. Equations stay
// in the front owner's existing reservation; graph backing is dropped before
// component solving and record encoding.
fn graph_bytes(nodes: usize, edges: usize, facts: usize) -> Option<u64> {
    let twice = nodes.checked_mul(2)?.checked_add(4)?;
    let members = nodes.checked_mul(6)?.checked_add(4)?;
    let adjacency = edges.checked_mul(2)?.checked_add(facts.checked_mul(4)?)?;
    let mut weight = hirsel_query_products::retained::MetadataWeight::default();
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

impl std::fmt::Debug for SourceGrowthComponent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourceGrowthComponent")
            .field("product", &self.product)
            .field("members", &self.members)
            .field("growing", &self.growing)
            .finish()
    }
}

impl SourceGrowthComponent {
    pub(crate) fn equivalent(&self, other: &Self) -> bool {
        self.members == other.members && self.settled_growing == other.settled_growing
    }
}
