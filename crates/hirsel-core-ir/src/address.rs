//! PROG-10 code addresses, independent of reuse and native artifacts.

use crate::Identity;
use crate::decode::{Debit, DecodeResult};
use crate::normal::{
    uleb,
    wire::{Read, Wire, invalid},
};
use crate::release_recipe::{ForeignFunction, text};
use hirsel_core_ids::ContentHash;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// The hash and ordered executable slots of a checked instance, and the value
/// hash of each of its constant slots in order (PROG-10). A constant slot is
/// bound to a value, never walked as a callee.
pub struct CodeRow {
    pub body: ContentHash,
    pub slots: Vec<Identity>,
    pub values: Vec<ContentHash>,
}

const GROUP_MAGIC: &[u8] = b"HIRGROUP\0\x02\x00";
/// The group hash domain; version 2 adds the `Value` binding.
pub const GROUP_HASH_DOMAIN: &str = "hirsel.code-group.v2";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Binding {
    Internal(u32),
    External(ContentHash),
    Builtin(u32),
    /// A constant slot, bound to the computed constant's value hash.
    Value(ContentHash),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Member {
    pub body: ContentHash,
    pub slots: Vec<Binding>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Group {
    pub members: Vec<Member>,
}

impl Binding {
    fn encode(self, bytes: &mut Vec<u8>) {
        match self {
            Self::Internal(member) => {
                bytes.push(0);
                uleb(bytes, u64::from(member));
            }
            Self::External(address) => {
                bytes.push(1);
                bytes.extend_from_slice(&address.0);
            }
            Self::Builtin(number) => {
                bytes.push(2);
                bytes.extend_from_slice(&number.to_le_bytes());
            }
            Self::Value(hash) => {
                bytes.push(3);
                bytes.extend_from_slice(&hash.0);
            }
        }
    }

    fn decode(input: &mut Read<'_, '_>) -> DecodeResult<Self> {
        match u8::read(input)? {
            0 => Ok(Self::Internal(input.index()?)),
            1 => Ok(Self::External(Wire::read(input)?)),
            2 => Ok(Self::Builtin(Wire::read(input)?)),
            3 => Ok(Self::Value(Wire::read(input)?)),
            _ => Err(invalid("unknown group binding tag")),
        }
    }
}

impl Group {
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = Self::header(self.members.len());
        for member in &self.members {
            Self::write_member_header(&mut bytes, member.body, member.slots.len());
            for &slot in &member.slots {
                slot.encode(&mut bytes);
            }
        }
        bytes
    }

    fn header(count: usize) -> Vec<u8> {
        let mut bytes = GROUP_MAGIC.to_vec();
        uleb(&mut bytes, count as u64);
        bytes
    }

    fn write_member_header(bytes: &mut Vec<u8>, body: ContentHash, slots: usize) {
        bytes.extend_from_slice(&body.0);
        uleb(bytes, slots as u64);
    }

    /// Read the frame. Admission checks member order against the identities in
    /// the bodies and validates the bindings once it has the whole closure.
    pub fn decode(bytes: &[u8], budget: &Debit<'_>) -> DecodeResult<Self> {
        let mut input = Read::new(bytes, GROUP_MAGIC, budget)?;
        let count = input.count()?;
        if count == 0 || u32::try_from(count - 1).is_err() {
            return Err(invalid("invalid group member count"));
        }
        input.charge_members(count)?;
        let members = input.read_vec(count, |input| {
            let body = Wire::read(input)?;
            let count = input.count()?;
            input.charge_slots(count)?;
            let slots = input.read_vec(count, Binding::decode)?;
            Ok(Member { body, slots })
        })?;
        input.end()?;
        Ok(Self { members })
    }

    pub fn hash(bytes: &[u8]) -> ContentHash {
        ContentHash::of_parts(GROUP_HASH_DOMAIN, [bytes])
    }

    pub fn member_address(hash: ContentHash, ordinal: u32) -> ContentHash {
        let mut bytes = [0; 36];
        bytes[..32].copy_from_slice(&hash.0);
        bytes[32..].copy_from_slice(&ordinal.to_le_bytes());
        ContentHash::of_parts("hirsel.code-address.v1", [&bytes[..]])
    }
}

/// The bytes carried by a PROG-9 descriptor and the entry link.
pub fn code_revision(address: ContentHash) -> ContentHash {
    ContentHash::of_parts("hirsel.code-revision.v2", [&address.0[..]])
}

mod builtin_semantics;

/// An intrinsic's PROG-10 address. The table is keyed by definition, while the
/// hash includes the full identity, including closed arguments if there are any.
pub fn builtin_address(identity: &Identity) -> Option<ContentHash> {
    builtin_semantics::number(&identity.definition).map(|n| hash_builtin(identity, n))
}

/// The exact semantics number implemented for a compiler builtin.
pub fn builtin_semantics_number(definition: &hirsel_core_ids::DefPath) -> Option<u32> {
    builtin_semantics::number(definition)
}

/// Whether the definition has a versioned compiler implementation.
pub fn is_versioned_builtin(definition: &hirsel_core_ids::DefPath) -> bool {
    builtin_semantics::number(definition).is_some()
}

fn hash_builtin(identity: &Identity, semantics: u32) -> ContentHash {
    let mut bytes = crate::normal::encode_identity(identity);
    bytes.extend_from_slice(&semantics.to_le_bytes());
    ContentHash::of_parts("hirsel.code-builtin.v1", [&bytes[..]])
}

/// FFI-9 binds a foreign function's identity, header, C symbol and full archive set.
pub fn foreign_address(
    function: &ForeignFunction,
    prefix: &str,
    archives: ContentHash,
) -> ContentHash {
    foreign_code("hirsel.code-foreign.v1", function, prefix, &archives.0)
}

/// FFI-16: a contained function's address names the module's pin `m` where a
/// native one names its archive-set digest, under its own domain.
pub fn contained_address(function: &ForeignFunction, prefix: &str, pin: [u8; 32]) -> ContentHash {
    foreign_code("hirsel.code-contained.v1", function, prefix, &pin)
}

fn foreign_code(
    domain: &str,
    function: &ForeignFunction,
    prefix: &str,
    code: &[u8; 32],
) -> ContentHash {
    let mut bytes = crate::normal::encode_identity(&function.identity);
    text(&mut bytes, &function.header);
    let name = &function.identity.definition.0.name;
    uleb(&mut bytes, (prefix.len() + name.len()) as u64);
    bytes.extend_from_slice(prefix.as_bytes());
    bytes.extend_from_slice(name.as_bytes());
    bytes.extend_from_slice(code);
    ContentHash::of_parts(domain, [&bytes[..]])
}

/// Addresses of all instances reached by these roots. A missing row means an
/// incomplete reach, unless the definition has an assigned builtin number.
pub fn code_addresses(
    roots: impl IntoIterator<Item = Identity>,
    row: impl FnMut(&Identity) -> Option<CodeRow>,
) -> Option<BTreeMap<Identity, ContentHash>> {
    code_groups(roots, row, |_, _| {})
}

/// Compute the addresses and hand each canonical group frame to its producer.
pub fn code_groups(
    roots: impl IntoIterator<Item = Identity>,
    row: impl FnMut(&Identity) -> Option<CodeRow>,
    group: impl FnMut(ContentHash, Vec<u8>),
) -> Option<BTreeMap<Identity, ContentHash>> {
    code_groups_with_external(roots, row, |_| None, group)
}

/// A bodyless foreign function supplies a verified FFI-9 external address.
pub fn code_groups_with_external(
    roots: impl IntoIterator<Item = Identity>,
    row: impl FnMut(&Identity) -> Option<CodeRow>,
    external: impl Fn(&Identity) -> Option<ContentHash>,
    group: impl FnMut(ContentHash, Vec<u8>),
) -> Option<BTreeMap<Identity, ContentHash>> {
    let graph = CodeGraph::discover(roots, row, external)?;
    let mut group = group;
    let hashes = group_addresses(&graph.nodes, |_, hash, bytes| group(hash, bytes));
    let mut addresses = graph.external_addresses;
    addresses.extend(graph.members.into_iter().zip(hashes));
    Some(addresses)
}

struct CodeGraph {
    members: Vec<Identity>,
    nodes: Vec<Node>,
    external_addresses: BTreeMap<Identity, ContentHash>,
}

impl CodeGraph {
    fn discover(
        roots: impl IntoIterator<Item = Identity>,
        mut row: impl FnMut(&Identity) -> Option<CodeRow>,
        external: impl Fn(&Identity) -> Option<ContentHash>,
    ) -> Option<Self> {
        let mut checked = BTreeMap::new();
        let mut builtins = BTreeMap::new();
        let mut externals = BTreeMap::new();
        let mut pending: Vec<_> = roots.into_iter().collect();
        while let Some(identity) = pending.pop() {
            if checked.contains_key(&identity)
                || builtins.contains_key(&identity)
                || externals.contains_key(&identity)
            {
                continue;
            }
            if let Some(code) = row(&identity) {
                pending.extend(code.slots.iter().cloned());
                checked.insert(identity, code);
            } else if let Some(number) = builtin_semantics::number(&identity.definition) {
                builtins.insert(identity, number);
            } else {
                let address = external(&identity)?;
                externals.insert(identity, address);
            }
        }

        // These positions are in ascending Identity order and belong to this walk.
        let indices: BTreeMap<_, _> = checked.keys().enumerate().map(|(i, id)| (id, i)).collect();
        let nodes: Vec<_> = checked
            .values()
            .map(|code| Node {
                body: code.body,
                slots: code
                    .slots
                    .iter()
                    .map(|slot| match indices.get(slot) {
                        Some(&i) => Slot::Instance(i),
                        None => match builtins.get(slot) {
                            Some(&number) => Slot::Builtin(number),
                            None => Slot::External(externals[slot]),
                        },
                    })
                    .collect(),
                values: code.values.clone(),
            })
            .collect();
        externals.extend(builtins.into_iter().map(|(identity, number)| {
            let address = hash_builtin(&identity, number);
            (identity, address)
        }));
        Some(Self {
            members: checked.into_keys().collect(),
            nodes,
            external_addresses: externals,
        })
    }

    fn addresses(&self, hashes: &[ContentHash]) -> BTreeMap<Identity, ContentHash> {
        self.members
            .iter()
            .cloned()
            .zip(hashes.iter().copied())
            .chain(
                self.external_addresses
                    .iter()
                    .map(|(identity, hash)| (identity.clone(), *hash)),
            )
            .collect()
    }
}

/// The groups of one fixed slot graph, retained in callee-first order. A
/// body-only edit re-encodes its component and the callers whose bindings
/// change. The caller must build a new plan when the slot graph changes.
pub struct GroupPlan {
    graph: CodeGraph,
    frames: Vec<GroupFrame>,
    component: Vec<usize>,
    hashes: Vec<ContentHash>,
    addresses: Arc<BTreeMap<Identity, ContentHash>>,
    numbers: Vec<Option<u32>>,
}

struct GroupFrame {
    members: Vec<usize>,
    callers: Vec<usize>,
    hash: ContentHash,
    bytes: Vec<u8>,
}

impl GroupPlan {
    pub fn new(
        roots: impl IntoIterator<Item = Identity>,
        row: impl FnMut(&Identity) -> Option<CodeRow>,
        external: impl Fn(&Identity) -> Option<ContentHash>,
    ) -> Option<Self> {
        let graph = CodeGraph::discover(roots, row, external)?;
        let mut frames = Vec::new();
        let mut component = vec![0; graph.nodes.len()];
        let hashes = group_addresses(&graph.nodes, |members, hash, bytes| {
            for &node in members {
                component[node] = frames.len();
            }
            frames.push(GroupFrame {
                members: members.to_vec(),
                callers: Vec::new(),
                hash,
                bytes,
            });
        });
        for (i, node) in graph.nodes.iter().enumerate() {
            let caller = component[i];
            for slot in &node.slots {
                if let Slot::Instance(callee) = *slot
                    && component[callee] != caller
                {
                    frames[component[callee]].callers.push(caller);
                }
            }
        }
        for frame in &mut frames {
            frame.callers.sort_unstable();
            frame.callers.dedup();
        }
        let addresses = Arc::new(graph.addresses(&hashes));
        Some(Self {
            numbers: vec![None; graph.nodes.len()],
            graph,
            frames,
            component,
            hashes,
            addresses,
        })
    }

    /// Update hashes for changed bodies and constant values. Identities
    /// outside this plan's root closure do not contribute to its addresses.
    pub fn update<'a>(
        &mut self,
        rows: impl IntoIterator<Item = (&'a Identity, ContentHash, &'a [ContentHash])>,
    ) {
        let mut pending = BTreeSet::new();
        for (identity, body, values) in rows {
            if let Ok(node) = self.graph.members.binary_search(identity) {
                let held = &mut self.graph.nodes[node];
                if held.values != values {
                    held.values.clear();
                    held.values.extend_from_slice(values);
                } else if held.body == body {
                    continue;
                }
                held.body = body;
                pending.insert(self.component[node]);
            }
        }
        while let Some(i) = pending.pop_first() {
            let frame = &mut self.frames[i];
            let (hash, bytes) = hash_group(
                &self.graph.nodes,
                &mut frame.members,
                &mut self.numbers,
                &mut self.hashes,
            );
            if hash != frame.hash {
                frame.hash = hash;
                frame.bytes = bytes;
                let addresses = Arc::make_mut(&mut self.addresses);
                for &node in &frame.members {
                    *addresses.get_mut(&self.graph.members[node]).unwrap() = self.hashes[node];
                }
                pending.extend(frame.callers.iter().copied());
            }
        }
    }

    pub fn addresses(&self) -> Arc<BTreeMap<Identity, ContentHash>> {
        Arc::clone(&self.addresses)
    }

    pub fn frames(&self) -> impl Iterator<Item = (ContentHash, &[u8])> {
        self.frames
            .iter()
            .map(|frame| (frame.hash, frame.bytes.as_slice()))
    }
}

struct Node {
    body: ContentHash,
    slots: Vec<Slot>,
    values: Vec<ContentHash>,
}

#[derive(Clone, Copy)]
enum Slot {
    Instance(usize),
    Builtin(u32),
    External(ContentHash),
}

#[derive(Default)]
struct Visit {
    index: Option<usize>,
    low: usize,
    on_stack: bool,
}

struct Frame {
    node: usize,
    next: usize,
}

fn group_addresses(
    nodes: &[Node],
    mut group: impl FnMut(&[usize], ContentHash, Vec<u8>),
) -> Vec<ContentHash> {
    let mut visits: Vec<_> = (0..nodes.len()).map(|_| Visit::default()).collect();
    let mut frames: Vec<Frame> = Vec::new();
    let mut stack = Vec::new();
    let mut next_index = 0;
    let mut addresses = vec![ContentHash::default(); nodes.len()];
    let mut members = Vec::new();
    let mut group_numbers = vec![None; nodes.len()];
    let enter = |node: usize,
                 visits: &mut [Visit],
                 stack: &mut Vec<usize>,
                 frames: &mut Vec<Frame>,
                 next: &mut usize| {
        visits[node] = Visit {
            index: Some(*next),
            low: *next,
            on_stack: true,
        };
        *next += 1;
        stack.push(node);
        frames.push(Frame { node, next: 0 });
    };
    for root in 0..nodes.len() {
        if visits[root].index.is_some() {
            continue;
        }
        enter(root, &mut visits, &mut stack, &mut frames, &mut next_index);
        while let Some(frame) = frames.last_mut() {
            let node = frame.node;
            if let Some(slot) = nodes[node].slots.get(frame.next) {
                frame.next += 1;
                if let Slot::Instance(child) = *slot {
                    match visits[child].index {
                        None => enter(child, &mut visits, &mut stack, &mut frames, &mut next_index),
                        Some(index) if visits[child].on_stack => {
                            visits[node].low = visits[node].low.min(index);
                        }
                        Some(_) => {}
                    }
                }
                continue;
            }
            frames.pop();
            let low = visits[node].low;
            if visits[node].index == Some(low) {
                loop {
                    let member = stack.pop().unwrap();
                    visits[member].on_stack = false;
                    members.push(member);
                    if member == node {
                        break;
                    }
                }
                let (hash, bytes) =
                    hash_group(nodes, &mut members, &mut group_numbers, &mut addresses);
                group(&members, hash, bytes);
                members.clear();
            }
            if let Some(parent) = frames.last() {
                visits[parent.node].low = visits[parent.node].low.min(low);
            }
        }
    }
    addresses
}

fn hash_group(
    nodes: &[Node],
    members: &mut [usize],
    numbers: &mut [Option<u32>],
    addresses: &mut [ContentHash],
) -> (ContentHash, Vec<u8>) {
    members.sort_unstable();
    for (k, &member) in members.iter().enumerate() {
        numbers[member] = Some(k as u32);
    }
    let mut bytes = Group::header(members.len());
    for &member in members.iter() {
        let node = &nodes[member];
        Group::write_member_header(&mut bytes, node.body, node.slots.len() + node.values.len());
        for &slot in &node.slots {
            let binding = match slot {
                Slot::Instance(child) => match numbers[child] {
                    Some(k) => Binding::Internal(k),
                    None => Binding::External(addresses[child]),
                },
                Slot::Builtin(number) => Binding::Builtin(number),
                Slot::External(address) => Binding::External(address),
            };
            binding.encode(&mut bytes);
        }
        for &value in &node.values {
            Binding::Value(value).encode(&mut bytes);
        }
    }
    let group = Group::hash(&bytes);
    for &member in members.iter() {
        let k = numbers[member].take().unwrap();
        addresses[member] = Group::member_address(group, k);
    }
    (group, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hirsel_core_ids::DefPath;

    #[test]
    fn body_edits_rehash_only_their_groups_and_callers() {
        let [root, a, b, leaf] = ["root", "a", "b", "leaf"].map(|name| {
            Identity::plain(DefPath::from_full(&format!("app@1/main/{name}")).unwrap())
        });
        let mut bodies = BTreeMap::from([
            (root.clone(), ContentHash([1; 32])),
            (a.clone(), ContentHash([2; 32])),
            (b.clone(), ContentHash([3; 32])),
            (leaf.clone(), ContentHash([4; 32])),
        ]);
        let slots = BTreeMap::from([
            (root.clone(), vec![a.clone(), leaf.clone()]),
            (a.clone(), vec![b.clone()]),
            (b.clone(), vec![a.clone()]),
            (leaf.clone(), vec![]),
        ]);
        // `a` reads one computed constant.
        let mut value = vec![ContentHash([6; 32])];
        let row = |bodies: &BTreeMap<Identity, ContentHash>,
                   value: &Vec<ContentHash>,
                   identity: &Identity| {
            Some(CodeRow {
                body: *bodies.get(identity)?,
                slots: slots.get(identity)?.clone(),
                values: if *identity == a {
                    value.clone()
                } else {
                    vec![]
                },
            })
        };
        let mut plan =
            GroupPlan::new([root.clone()], |id| row(&bodies, &value, id), |_| None).unwrap();
        let before = plan.addresses();
        let hash = ContentHash([5; 32]);
        bodies.insert(b.clone(), hash);
        plan.update([(&b, hash, &[][..])]);
        let mut expected_frames = BTreeMap::new();
        let expected = code_groups(
            [root.clone()],
            |id| row(&bodies, &value, id),
            |hash, bytes| {
                expected_frames.insert(hash, bytes);
            },
        )
        .unwrap();
        let after = plan.addresses();
        assert_eq!(*after, expected);
        assert_eq!(
            plan.frames()
                .map(|(hash, bytes)| (hash, bytes.to_vec()))
                .collect::<BTreeMap<_, _>>(),
            expected_frames,
        );
        assert_eq!(after[&leaf], before[&leaf]);
        for changed in [&root, &a, &b] {
            assert_ne!(after[changed], before[changed]);
        }
        // A value edit with an unchanged body rehashes the reader's group and
        // its callers, and nothing else (G7 row 1).
        value = vec![ContentHash([7; 32])];
        plan.update([(&a, bodies[&a], &value[..])]);
        let expected = code_groups([root.clone()], |id| row(&bodies, &value, id), |_, _| {});
        let changed = plan.addresses();
        assert_eq!(*changed, expected.unwrap());
        assert_eq!(changed[&leaf], after[&leaf]);
        for reader in [&root, &a, &b] {
            assert_ne!(changed[reader], after[reader]);
        }
    }

    #[test]
    fn a_constant_slot_binds_its_value_hash() {
        // One member, body [3; 32], no function slot, one constant slot bound to
        // value hash [6; 32]: SHA-256 framing computed with Python hashlib.
        let c = Identity::plain(DefPath::from_full("app@1/main/c").unwrap());
        let addresses = code_addresses([c.clone()], |_| {
            Some(CodeRow {
                body: ContentHash([3; 32]),
                slots: vec![],
                values: vec![ContentHash([6; 32])],
            })
        })
        .unwrap();
        let hex = addresses[&c].to_string();
        assert_eq!(
            hex,
            "1d2b613082f724ec5e16b0b5fc7ce9576c101d86baab23e806755be92bc95384"
        );
    }

    #[test]
    fn recursive_generic_group_uses_full_identity_order_and_every_edge() {
        let key = |byte| Identity {
            definition: DefPath::from_full("app@1/main/f").unwrap(),
            dictionaries: vec![],
            closures: vec![ContentHash([byte; 32])],
        };
        let (a, b) = (key(1), key(2));
        let graph = |roots, a_body, b_body, back| {
            code_addresses(roots, |key| {
                if *key == a {
                    Some(CodeRow {
                        body: ContentHash([a_body; 32]),
                        slots: vec![b.clone()],
                        values: vec![],
                    })
                } else {
                    Some(CodeRow {
                        body: ContentHash([b_body; 32]),
                        slots: vec![if back { a.clone() } else { b.clone() }],
                        values: vec![],
                    })
                }
            })
            .unwrap()
        };
        let normal = graph(vec![a.clone()], 7, 9, true);
        assert_eq!(normal, graph(vec![b.clone()], 7, 9, true));
        assert_eq!(normal, graph(vec![b.clone(), a.clone()], 7, 9, true));
        let hex = |hash: ContentHash| hash.to_string();
        // Independent group of two members: a's slot is Internal(1),
        // b's Internal(0). SHA-256 framing was computed with Python hashlib.
        assert_eq!(
            hex(normal[&a]),
            "4993888bf25276d9c52375686ffbddacd5f551ee919ae6617ae3f3a6f0439de7"
        );
        assert_eq!(
            hex(normal[&b]),
            "e2772927965c4c1b4098db379fe7de8ede69383979185f1be4aa30c20082d6ec"
        );
        for edited in [
            graph(vec![a.clone()], 8, 9, true),
            graph(vec![a.clone()], 7, 10, true),
        ] {
            assert_ne!(normal[&a], edited[&a]);
            assert_ne!(normal[&b], edited[&b]);
        }
        let split = graph(vec![a.clone()], 7, 9, false);
        assert_ne!(normal[&a], split[&a]);
        assert_ne!(normal[&b], split[&b]);
        let edit_after_split = graph(vec![a.clone()], 8, 9, false);
        assert_eq!(
            split[&b], edit_after_split[&b],
            "self-recursive b no longer reaches a"
        );
    }
}
