//! PROG-10 code addresses, independent of reuse and native artifacts.

use crate::Identity;
use hirsel_core_ids::ContentHash;
use std::collections::BTreeMap;

/// The hash and ordered executable slots of a checked instance.
pub struct CodeRow {
    pub body: ContentHash,
    pub slots: Vec<Identity>,
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

/// Whether the definition has a versioned compiler implementation.
pub fn is_versioned_builtin(definition: &hirsel_core_ids::DefPath) -> bool {
    builtin_semantics::number(definition).is_some()
}

fn hash_builtin(identity: &Identity, semantics: u32) -> ContentHash {
    let mut bytes = crate::normal::encode_identity(identity);
    bytes.extend_from_slice(&semantics.to_le_bytes());
    ContentHash::of_parts("hirsel.code-builtin.v1", [&bytes[..]])
}

/// Addresses of all instances reached by these roots. A missing row means an
/// incomplete reach, unless the definition has an assigned builtin number.
pub fn code_addresses(
    roots: impl IntoIterator<Item = Identity>,
    row: impl FnMut(&Identity) -> Option<CodeRow>,
) -> Option<BTreeMap<Identity, ContentHash>> {
    addresses(roots, row, |identity| {
        builtin_semantics::number(&identity.definition)
    })
}

fn addresses(
    roots: impl IntoIterator<Item = Identity>,
    mut row: impl FnMut(&Identity) -> Option<CodeRow>,
    builtin: impl Fn(&Identity) -> Option<u32>,
) -> Option<BTreeMap<Identity, ContentHash>> {
    let mut checked = BTreeMap::new();
    let mut builtins = BTreeMap::new();
    let mut pending: Vec<_> = roots.into_iter().collect();
    while let Some(identity) = pending.pop() {
        if checked.contains_key(&identity) || builtins.contains_key(&identity) {
            continue;
        }
        if let Some(code) = row(&identity) {
            pending.extend(code.slots.iter().cloned());
            checked.insert(identity, code);
        } else {
            let number = builtin(&identity)?;
            builtins.insert(identity, number);
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
                    None => Slot::Builtin(builtins[slot]),
                })
                .collect(),
        })
        .collect();
    let hashes = group_addresses(&nodes);
    let mut result: BTreeMap<_, _> = checked.into_keys().zip(hashes).collect();
    result.extend(builtins.into_iter().map(|(identity, number)| {
        let address = hash_builtin(&identity, number);
        (identity, address)
    }));
    Some(result)
}

struct Node {
    body: ContentHash,
    slots: Vec<Slot>,
}

#[derive(Clone, Copy)]
enum Slot {
    Instance(usize),
    Builtin(u32),
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

fn group_addresses(nodes: &[Node]) -> Vec<ContentHash> {
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
                hash_group(nodes, &mut members, &mut group_numbers, &mut addresses);
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
) {
    members.sort_unstable();
    for (k, &member) in members.iter().enumerate() {
        numbers[member] = Some(k as u32);
    }
    let mut bytes = b"HIRGROUP\0\x01\x00".to_vec();
    crate::normal::uleb(&mut bytes, members.len() as u64);
    for &member in members.iter() {
        let node = &nodes[member];
        bytes.extend_from_slice(&node.body.0);
        crate::normal::uleb(&mut bytes, node.slots.len() as u64);
        for slot in &node.slots {
            match *slot {
                Slot::Instance(child) => match numbers[child] {
                    Some(k) => {
                        bytes.push(0);
                        crate::normal::uleb(&mut bytes, u64::from(k));
                    }
                    None => {
                        bytes.push(1);
                        bytes.extend_from_slice(&addresses[child].0);
                    }
                },
                Slot::Builtin(number) => {
                    bytes.push(2);
                    bytes.extend_from_slice(&number.to_le_bytes());
                }
            }
        }
    }
    let group = ContentHash::of_parts("hirsel.code-group.v1", [&bytes[..]]);
    let mut member_bytes = [0; 36];
    member_bytes[..32].copy_from_slice(&group.0);
    for &member in members.iter() {
        let k = numbers[member].take().unwrap();
        member_bytes[32..].copy_from_slice(&k.to_le_bytes());
        addresses[member] = ContentHash::of_parts("hirsel.code-address.v1", [&member_bytes[..]]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hirsel_core_ids::DefPath;

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
                    })
                } else {
                    Some(CodeRow {
                        body: ContentHash([b_body; 32]),
                        slots: vec![if back { a.clone() } else { b.clone() }],
                    })
                }
            })
            .unwrap()
        };
        let normal = graph(vec![a.clone()], 7, 9, true);
        assert_eq!(normal, graph(vec![b.clone()], 7, 9, true));
        assert_eq!(normal, graph(vec![b.clone(), a.clone()], 7, 9, true));
        let hex = |hash: ContentHash| {
            hash.0
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        };
        // Independent group of two members: a's slot is Internal(1),
        // b's Internal(0). SHA-256 framing was computed with Python hashlib.
        assert_eq!(
            hex(normal[&a]),
            "5491ad36f94b799078d3d5912302f491df5ac026a97d661cfe92d94a765aaa4d"
        );
        assert_eq!(
            hex(normal[&b]),
            "db3dd3efab15c8e6d1212ee69f83f1441d6fb56811388eefe35dc8f678cc9105"
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
