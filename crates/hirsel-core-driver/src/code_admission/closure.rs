use super::{AdmissionError, BackendSemantics, Body, EngineTag, Limit, SlotContract, charge_rows};
use crate::foreign_header::Abi;
use hirsel_core_ids::ContentHash;
use hirsel_core_ir::Identity;
use hirsel_core_ir::address::{Binding, Group, foreign_address};
use hirsel_core_ir::decode::Debit;
use hirsel_core_ir::normal::{DecodedBody, Slot};
use hirsel_core_ir::release_recipe::{Backend, ForeignFunction, ObjectRef, Recipe};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap};

#[derive(Clone, Copy)]
struct MemberIndex {
    group: usize,
    member: usize,
}

struct Member {
    address: ContentHash,
    body: Option<usize>,
}

struct GroupIndex {
    members: Vec<Vec<Member>>,
    addresses: HashMap<ContentHash, MemberIndex>,
}

#[derive(Clone, Copy, PartialEq)]
enum BoundIdentity {
    Member(ContentHash),
    Foreign(ContentHash),
    Builtin(u32),
}

struct ForeignIndex<'a> {
    addresses: HashMap<ContentHash, usize>,
    functions: Vec<ForeignSlot<'a>>,
}

struct ForeignSlot<'a> {
    address: ContentHash,
    function: &'a ForeignFunction,
    abi: Abi<'a>,
    archive: ObjectRef,
    reached: bool,
}

impl<'a> ForeignIndex<'a> {
    fn new(recipe: &'a Recipe, budget: &Debit<'_>) -> Result<Self, AdmissionError> {
        let mut index = Self {
            addresses: HashMap::new(),
            functions: Vec::new(),
        };
        for leaf in &recipe.foreign_modules {
            if leaf.functions.is_empty() {
                return Err(AdmissionError::Refused(format!(
                    "Extra(foreign module {})",
                    leaf.module
                )));
            }
            budget.check()?;
            let encoded = budget.scratch();
            let len = leaf.targets.iter().map(|row| row.target.len() + 43).sum();
            charge_rows::<u8>(&encoded, len)?;
            let mut rows = Vec::with_capacity(len);
            leaf.encode_archive_rows(&mut rows);
            let archives = ContentHash(Sha256::digest(&rows).into());
            drop((rows, encoded));
            let selected = leaf
                .targets
                .binary_search_by(|row| row.target.cmp(&recipe.launch.target))
                .expect("archive inventory selects a row for each reached module");
            for function in &leaf.functions {
                budget.charge_slots(1)?;
                let abi = Abi::parse(&function.header)
                    .ok_or(AdmissionError::Binding("invalid foreign function header"))?;
                let address = foreign_address(function, &leaf.symbols, archives);
                charge_map::<ContentHash, usize>(budget, 1)?;
                if index
                    .addresses
                    .insert(address, index.functions.len())
                    .is_some()
                {
                    return Err(AdmissionError::Binding("duplicate foreign address"));
                }
                budget.reserve(&mut index.functions, 1)?;
                index.functions.push(ForeignSlot {
                    address,
                    function,
                    abi,
                    archive: leaf.targets[selected].archive,
                    reached: false,
                });
            }
        }
        Ok(index)
    }

    fn reach(&mut self, address: ContentHash) -> Option<&ForeignSlot<'a>> {
        let &index = self.addresses.get(&address)?;
        let row = &mut self.functions[index];
        row.reached = true;
        Some(row)
    }

    fn extra(&self) -> Option<ContentHash> {
        self.functions
            .iter()
            .find_map(|row| (!row.reached).then_some(row.address))
    }
}

pub(super) fn check(
    recipe: &Recipe,
    groups: &[Group],
    bodies: &[Body],
    semantics: &impl BackendSemantics,
    budget: &Debit<'_>,
) -> Result<(), AdmissionError> {
    let GroupIndex { members, addresses } = index(recipe, groups, budget)?;
    let entry = *addresses
        .get(&recipe.address)
        .ok_or(AdmissionError::Missing(recipe.address))?;
    let mut foreign = ForeignIndex::new(recipe, budget)?;

    charge_rows::<bool>(budget, groups.len() + bodies.len())?;
    charge_rows::<usize>(budget, groups.len() * 3)?;
    charge_rows::<Vec<usize>>(budget, groups.len())?;
    let mut reached_groups = vec![false; groups.len()];
    let mut reached_bodies = vec![false; bodies.len()];
    let mut pending = Vec::with_capacity(groups.len());
    let mut out = vec![Vec::new(); groups.len()];
    let mut indegree = vec![0; groups.len()];
    let member_count = addresses.len();
    charge_map::<&Identity, BoundIdentity>(budget, member_count)?;
    let mut identities = HashMap::with_capacity(member_count);
    reached_groups[entry.group] = true;
    pending.push(entry.group);
    let mut binding = None;
    let mut conflict = None;
    let mut unsupported = None;

    if let Some(body) = members[entry.group][entry.member].body
        && bodies[body].identity != recipe.entry
    {
        binding = Some("entry identity differs from its body");
    }

    while let Some(group) = pending.pop() {
        let rows = &groups[group].members;
        let mut graph = InternalEdges::new(rows.len(), budget)?;
        let mut previous = None;
        for (member, row) in rows.iter().enumerate() {
            budget.check()?;
            let indexed = &members[group][member];
            let body = indexed.body.ok_or(AdmissionError::Missing(row.body))?;
            let decoded = &bodies[body];
            reached_bodies[body] = true;
            let identity = &decoded.identity;
            if previous.is_some_and(|prev| prev >= identity) {
                binding.get_or_insert("group members are not in ascending identity order");
            }
            previous = Some(identity);
            record_identity(
                &mut identities,
                identity,
                BoundIdentity::Member(indexed.address),
                &mut conflict,
                budget,
            )?;
            if row.slots.len() != decoded.slots.len() {
                binding.get_or_insert("group binding count differs from the body slots");
            }

            for (slot_index, target) in row.slots.iter().enumerate() {
                budget.check()?;
                let slot = decoded.slots.get(slot_index);
                let target = match *target {
                    Binding::Internal(target) => {
                        let target = target as usize;
                        if target >= rows.len() {
                            binding.get_or_insert("internal member index is out of range");
                            continue;
                        }
                        graph.push(member, target)?;
                        MemberIndex {
                            group,
                            member: target,
                        }
                    }
                    Binding::External(address) => {
                        let Some(&target) = addresses.get(&address) else {
                            let function = foreign
                                .reach(address)
                                .ok_or(AdmissionError::Missing(address))?;
                            if let Some(slot) = slot {
                                if slot.identity != function.function.identity
                                    || slot.signature.is_none()
                                {
                                    binding.get_or_insert(
                                        "slot identity differs from its bound foreign function",
                                    );
                                } else if let Slot::External(id) = decoded.decoded.slots[slot_index]
                                {
                                    let module = &decoded.decoded.module;
                                    let signature = &module.signatures
                                        [module.functions[id.index()].signature.index()];
                                    if !function.abi.matches_slot(signature, module) {
                                        binding.get_or_insert(
                                            "slot signature differs from its bound foreign header",
                                        );
                                    }
                                }
                                record_identity(
                                    &mut identities,
                                    &slot.identity,
                                    BoundIdentity::Foreign(address),
                                    &mut conflict,
                                    budget,
                                )?;
                            }
                            continue;
                        };
                        if target.group == group {
                            binding.get_or_insert("external binding stays within the group");
                        } else {
                            budget.reserve(&mut out[group], 1)?;
                            out[group].push(target.group);
                            indegree[target.group] += 1;
                        }
                        if !std::mem::replace(&mut reached_groups[target.group], true) {
                            pending.push(target.group);
                        }
                        target
                    }
                    Binding::Builtin(version) => {
                        if let Some(slot) = slot {
                            record_identity(
                                &mut identities,
                                &slot.identity,
                                BoundIdentity::Builtin(version),
                                &mut conflict,
                                budget,
                            )?;
                            if slot.signature.is_some() {
                                binding.get_or_insert("callable slot must bind to a member");
                            } else if !semantics.supports_builtin(
                                recipe.launch.backend,
                                &slot.identity,
                                version,
                            ) {
                                unsupported.get_or_insert("builtin identity and version");
                            }
                        }
                        continue;
                    }
                };
                if let (Some(slot), Some(body)) = (slot, members[target.group][target.member].body)
                {
                    check_member(slot, &bodies[body], &mut binding, &mut conflict);
                }
            }
        }
        if !graph.strongly_connected()? {
            binding.get_or_insert("group is not strongly connected");
        }
    }

    if let Some(n) = reached_groups.iter().position(|&seen| !seen) {
        return Err(AdmissionError::Extra(recipe.groups[n].hash));
    }
    if let Some(n) = reached_bodies.iter().position(|&seen| !seen) {
        return Err(AdmissionError::Extra(recipe.bodies[n].body.hash));
    }
    if let Some(address) = foreign.extra() {
        return Err(AdmissionError::Extra(address));
    }
    if let Some(reason) = binding {
        return Err(AdmissionError::Binding(reason));
    }
    if !acyclic(&out, indegree, pending, budget, |_| {})? {
        return Err(AdmissionError::Binding(
            "external group bindings form a cycle",
        ));
    }
    if let Some(identity) = conflict {
        return Err(AdmissionError::Conflict(Box::new(identity.clone())));
    }
    let mut prefixes = BTreeSet::new();
    for leaf in &recipe.foreign_modules {
        charge_rows::<&str>(budget, 1)?;
        if !prefixes.insert(leaf.symbols.as_str()) {
            return Err(AdmissionError::Conflict(Box::new(Identity::plain(
                leaf.module.clone(),
            ))));
        }
    }
    let mut grants = BTreeMap::new();
    for grant in &recipe.launch.grants {
        if let Some(archive) = grant.archive {
            charge_rows::<(&str, ObjectRef)>(budget, 1)?;
            let interface = grant.values[0].as_str();
            if let Some(previous) = grants.insert(interface, archive)
                && previous != archive
            {
                return Err(AdmissionError::Binding(
                    "one foreign interface has different archive grants",
                ));
            }
        }
    }
    for row in &foreign.functions {
        budget.check()?;
        // A function can be in the closure even when the program has no way
        // to make its required capability. If it has the capability, the
        // operator's pin must match the archive whose code will run.
        if let Some(archive) = grants.get(row.abi.interface)
            && archive != &row.archive
        {
            return Err(AdmissionError::Binding(
                "foreign function has no matching archive grant",
            ));
        }
    }
    let entry_body = members[entry.group][entry.member]
        .body
        .expect("walked entry body");
    check_launch(recipe, entry_body, bodies, budget)?;
    let backend = recipe.launch.backend;
    if !semantics.supports_target(backend, &recipe.launch.target) {
        return Err(AdmissionError::Unsupported("native backend target"));
    }
    if let Some(reason) = unsupported {
        return Err(AdmissionError::Unsupported(reason));
    }
    for body in bodies {
        check_engine_tags(&body.decoded, backend, semantics, budget)?;
    }
    Ok(())
}

fn index(
    recipe: &Recipe,
    groups: &[Group],
    budget: &Debit<'_>,
) -> Result<GroupIndex, AdmissionError> {
    charge_rows::<Vec<Member>>(budget, groups.len())?;
    let count = groups.iter().try_fold(0_usize, |count, group| {
        count
            .checked_add(group.members.len())
            .ok_or(AdmissionError::Limit(Limit::Members))
    })?;
    charge_rows::<Member>(budget, count)?;
    charge_map::<ContentHash, MemberIndex>(budget, count)?;
    let mut addresses = HashMap::with_capacity(count);
    let mut members = Vec::with_capacity(groups.len());
    for (group, rows) in groups.iter().enumerate() {
        let group_hash = recipe.groups[group].hash;
        let mut indexed = Vec::with_capacity(rows.members.len());
        for (member, row) in rows.members.iter().enumerate() {
            budget.check()?;
            let ordinal =
                u32::try_from(member).map_err(|_| AdmissionError::Limit(Limit::Members))?;
            let address = Group::member_address(group_hash, ordinal);
            if addresses
                .insert(address, MemberIndex { group, member })
                .is_some()
            {
                return Err(AdmissionError::Binding("duplicate member address"));
            }
            let body = recipe
                .bodies
                .binary_search_by_key(&row.body, |body| body.body.hash)
                .ok();
            indexed.push(Member { address, body });
        }
        members.push(indexed);
    }
    Ok(GroupIndex { members, addresses })
}

fn record_identity<'a>(
    identities: &mut HashMap<&'a Identity, BoundIdentity>,
    identity: &'a Identity,
    target: BoundIdentity,
    conflict: &mut Option<&'a Identity>,
    budget: &Debit<'_>,
) -> Result<(), AdmissionError> {
    if !matches!(target, BoundIdentity::Member(_)) {
        charge_map::<&Identity, BoundIdentity>(budget, 1)?;
    }
    if let Some(previous) = identities.insert(identity, target)
        && previous != target
    {
        conflict.get_or_insert(identity);
    }
    Ok(())
}

fn check_member<'a>(
    slot: &'a SlotContract,
    target: &Body,
    binding: &mut Option<&'static str>,
    conflict: &mut Option<&'a Identity>,
) {
    if slot.identity != target.identity || slot.signature.is_none() {
        binding.get_or_insert("slot identity differs from its bound member");
    } else if slot.signature != Some(target.signature) {
        conflict.get_or_insert(&slot.identity);
    }
}

fn check_launch(
    recipe: &Recipe,
    entry: usize,
    bodies: &[Body],
    budget: &Debit<'_>,
) -> Result<(), AdmissionError> {
    for (i, body) in bodies.iter().enumerate() {
        budget.check()?;
        if body.annex.1.is_some() != (i == entry) {
            return Err(AdmissionError::Binding(
                "startup parameter names belong to the entry annex",
            ));
        }
    }
    let body = &bodies[entry];
    let module = &body.decoded.module;
    let signature = &module.signatures[module.functions[0].signature.index()];
    if recipe.launch.grants.len() != signature.params.len()
        || recipe
            .launch
            .grants
            .iter()
            .enumerate()
            .any(|(i, grant)| grant.ordinal as usize != i)
    {
        return Err(AdmissionError::Binding(
            "launch grant ordinals differ from entry parameters",
        ));
    }
    let exit = signature.dest.is_some_and(|dest| {
        use hirsel_core_layout::{Body, FieldTy};
        let Body::Variant(cases) = &module.types[dest.index()].body else {
            return false;
        };
        let Some(error) = cases.get(1) else {
            return false;
        };
        matches!(
            error.fields.as_slice(),
            [field] if matches!(field.ty, FieldTy::Inline(row)
                if module.types[row.index()].name == "core@1/core/Exit")
        )
    });
    if recipe.launch.exit_status != exit {
        return Err(AdmissionError::Binding(
            "entry exit status differs from its result shape",
        ));
    }
    Ok(())
}

fn check_engine_tags(
    body: &DecodedBody,
    backend: Backend,
    semantics: &impl BackendSemantics,
    budget: &Debit<'_>,
) -> Result<(), AdmissionError> {
    use hirsel_core_ir::normal;
    use hirsel_core_ir::{Callee, RtOp, Wait};

    let check = |tag| {
        budget.check()?;
        if semantics.supports_tag(backend, tag) {
            Ok(())
        } else {
            Err(AdmissionError::Unsupported("engine operation tag"))
        }
    };
    for body in body.module.functions.iter().filter_map(|f| f.body.as_ref()) {
        for inst in &body.insts {
            check(EngineTag::Instruction(normal::instruction_tag(inst)))?;
        }
        for term in &body.terminators {
            check(EngineTag::Terminator(normal::terminator_tag(term)))?;
        }
        for call in &body.calls {
            if let Callee::Runtime(operation) = &call.callee {
                check(EngineTag::Runtime(normal::runtime_tag(operation)))?;
                match operation {
                    RtOp::Returnable(Wait::Host(host)) if host.returnable_symbol().is_none() => {
                        return Err(AdmissionError::Unsupported("returnable host operation"));
                    }
                    RtOp::Host(host) | RtOp::Returnable(Wait::Host(host)) => {
                        check(EngineTag::Host(normal::host_tag(host)))?;
                    }
                    RtOp::Flow(flow) => check(EngineTag::Flow(normal::flow_tag(flow)))?,
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

fn charge_map<K, V>(budget: &Debit<'_>, count: usize) -> Result<(), AdmissionError> {
    // Bound hash-table growth and its control bytes as well as the entries.
    charge_rows::<(K, V, u8)>(budget, count.saturating_mul(4))
}

// Record each internal edge when resolving the member's slots. The forward and
// reverse searches then prove strong connectivity without decoding an edge again.
// The edges of one group are given back when the next group is walked.
struct InternalEdges<'b> {
    forward: Vec<Vec<usize>>,
    reverse: Vec<Vec<usize>>,
    debit: Debit<'b>,
}

impl<'b> InternalEdges<'b> {
    fn new(members: usize, budget: &Debit<'b>) -> Result<Self, AdmissionError> {
        let debit = budget.scratch();
        charge_rows::<Vec<usize>>(&debit, members.saturating_mul(2))?;
        Ok(Self {
            forward: vec![Vec::new(); members],
            reverse: vec![Vec::new(); members],
            debit,
        })
    }

    fn push(&mut self, source: usize, target: usize) -> Result<(), AdmissionError> {
        self.debit.reserve(&mut self.forward[source], 1)?;
        self.forward[source].push(target);
        self.debit.reserve(&mut self.reverse[target], 1)?;
        self.reverse[target].push(source);
        Ok(())
    }

    fn strongly_connected(&self) -> Result<bool, AdmissionError> {
        let n = self.forward.len();
        if n <= 1 {
            return Ok(n == 1);
        }
        let budget = self.debit.scratch();
        charge_rows::<bool>(&budget, n)?;
        charge_rows::<usize>(&budget, n)?;
        let mut seen = vec![false; n];
        let mut pending = Vec::with_capacity(n);
        for graph in [&self.forward, &self.reverse] {
            seen.fill(false);
            seen[0] = true;
            pending.push(0);
            let mut reached = 0;
            while let Some(node) = pending.pop() {
                reached += 1;
                for &target in &graph[node] {
                    budget.check()?;
                    if !std::mem::replace(&mut seen[target], true) {
                        pending.push(target);
                    }
                }
            }
            if reached != n {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

/// Only requested groups need to be ordered. An external address outside
/// this subset either already lives with the receiver or is its refusal.
pub(super) fn send_order(
    recipe: &Recipe,
    groups: &[Group],
    budget: &Debit<'_>,
) -> Result<Vec<usize>, AdmissionError> {
    // Only the order outlives the walk.
    let work = budget.scratch();
    let GroupIndex { addresses, .. } = index(recipe, groups, &work)?;
    let count = groups.len();
    charge_rows::<Vec<usize>>(&work, count)?;
    charge_rows::<usize>(&work, count.saturating_mul(2))?;
    charge_rows::<usize>(budget, count)?;
    let mut graph = vec![Vec::new(); count];
    let mut indegree = vec![0; count];
    for (group, rows) in groups.iter().enumerate() {
        for binding in rows.members.iter().flat_map(|row| &row.slots) {
            budget.check()?;
            if let Binding::External(address) = binding
                && let Some(target) = addresses.get(address)
            {
                work.reserve(&mut graph[target.group], 1)?;
                graph[target.group].push(group);
                indegree[group] += 1;
            }
        }
    }
    let mut order = Vec::with_capacity(count);
    if !acyclic(&graph, indegree, Vec::with_capacity(count), &work, |i| {
        order.push(i)
    })? {
        return Err(AdmissionError::Binding(
            "external group bindings form a cycle",
        ));
    }
    Ok(order)
}

fn acyclic(
    graph: &[Vec<usize>],
    mut indegree: Vec<usize>,
    mut pending: Vec<usize>,
    budget: &Debit<'_>,
    mut visit: impl FnMut(usize),
) -> Result<bool, AdmissionError> {
    pending.extend(
        indegree
            .iter()
            .enumerate()
            .filter_map(|(i, &n)| (n == 0).then_some(i)),
    );
    let mut removed = 0;
    while let Some(node) = pending.pop() {
        budget.check()?;
        removed += 1;
        visit(node);
        for &target in &graph[node] {
            budget.check()?;
            indegree[target] -= 1;
            if indegree[target] == 0 {
                pending.push(target);
            }
        }
    }
    Ok(removed == graph.len())
}
