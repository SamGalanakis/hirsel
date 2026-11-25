//! A granted or narrowed capability that crosses units inside a value: an
//! item, a helper's entry or its result (docs/recording.md, "Handle
//! snapshots", unit tapes U3). Its grant is read from the cell that
//! recorded it before any cell is locked, since the sending or receiving
//! unit's own cell is locked while the item's value is written. Where it
//! arrives it takes a new object ordinal and the receiving unit's registry
//! designates its word on the receiver's own cell, so its operations there
//! reach the receiver's stream.
use super::super::schedule::Group;
use super::super::*;
use crate::host_file::{DYNAMIC_GRANT, Registry};
use hirsel_core_net::exec::ExecRules;
use std::sync::Arc;

/// One capability word's grant as the cell that recorded it holds it.
pub(crate) struct Granted {
    word: u64,
    grant: Result<Grant, &'static str>,
}

struct Grant {
    kind: u8,
    stored: Vec<u8>,
    inner: Option<Inner>,
    exec: Option<Arc<ExecRules>>,
}

const UNGRANTED: &str = "a value holds a capability not granted through the recording";
const VIRTUAL: &str =
    "a value holds a capability of virtual construction, whose snapshot this build does not write";
const NARROWED: &str =
    "a value holds a narrowed Exec capability, whose profile snapshot this build does not write";
const KIND: &str =
    "a value holds an Env or Cred capability, whose snapshot this build does not write";
const NET_RULES: &str = "a value holds a Net capability whose rules a snapshot cannot hold";
const UNRESOLVED: &str = "a value holds a capability the recording did not resolve";

/// Each of `words`, resolved through `registry`: the unit's own designation,
/// or the run's.
pub(crate) fn resolve(registry: &Registry, words: &[u64]) -> Vec<Granted> {
    words
        .iter()
        .map(|&word| Granted {
            word,
            grant: grant(registry, word),
        })
        .collect()
}

fn grant(registry: &Registry, word: u64) -> Result<Grant, &'static str> {
    if word & DYNAMIC_GRANT == 0 {
        return Err(UNGRANTED);
    }
    // SAFETY: a registry's grant provider is pinned for the run.
    let provider = unsafe { &*registry.grant_provider(word) };
    if !dispatch::owns(provider) {
        return Err(VIRTUAL);
    }
    let h = registry.grant_handle(word);
    let state = cell_of(provider.state).state.lock().unwrap();
    let live = state
        .live
        .as_ref()
        .filter(|_| state.generation == generation_of(h))
        .ok_or(UNGRANTED)?;
    let o = ordinal_of(h);
    let (kind, stored) = live.grants.get(&o).cloned().ok_or(NARROWED)?;
    let inner = live.objects[o - 1].inner;
    let stored = match kind {
        0 | 2..=6 | 8 | 13 => stored,
        // A Net's rules, granted or narrowed, are its source's handle.
        1 => net_rules(inner.ok_or(NET_RULES)?.handle)?,
        _ => return Err(KIND),
    };
    Ok(Grant {
        kind,
        stored,
        inner,
        exec: live.exec_rules.get(&o).cloned(),
    })
}

/// A Net's rules as its snapshot holds them: the rule count, then each rule
/// as the header's grant snapshot writes it.
fn net_rules(rules: u64) -> Result<Vec<u8>, &'static str> {
    // SAFETY: a recorded Net's source handle is its run-pinned rule list,
    // whose rows the run keeps live.
    let rules: Option<Vec<_>> = unsafe {
        (*(rules as *const crate::net::Rules))
            .as_slice()
            .iter()
            .map(|r| r.decode())
            .collect()
    };
    format::net_snapshot(&rules.ok_or(NET_RULES)?).map_err(|_| NET_RULES)
}

/// A capability snapshot: tag 5, `ordinal`, then the kind and its stored
/// bytes as the grant snapshot encodes them.
fn snapshot(ordinal: u64, g: &Grant) -> Vec<u8> {
    let mut out = vec![5];
    put_varint(&mut out, ordinal);
    out.push(g.kind);
    put_varint(&mut out, g.stored.len() as u64);
    out.extend_from_slice(&g.stored);
    out
}

fn find(grants: &[Granted], word: u64) -> Result<&Grant, &'static str> {
    let g = grants.iter().find(|g| g.word == word).ok_or(UNRESOLVED)?;
    g.grant.as_ref().map_err(|why| *why)
}

/// A sent capability's snapshot; the canonical form leaves its ordinal out.
pub(in crate::tape) fn sent(grants: &[Granted], word: u64) -> Result<Vec<u8>, &'static str> {
    find(grants, word).map(|g| snapshot(0, g))
}

/// An arriving capability: a new object ordinal in the receiving unit, its
/// snapshot, and its word's designation there, applied once the whole item
/// is written.
pub(in crate::tape) fn arrived(
    live: &mut Live,
    generation: u32,
    grants: &[Granted],
    word: u64,
    designations: &mut Vec<(u64, u64)>,
) -> Result<Vec<u8>, &'static str> {
    let g = find(grants, word)?;
    live.objects.push(Object {
        inner: g.inner,
        open_resource: false,
    });
    let ordinal = live.objects.len();
    if let Some(rules) = &g.exec {
        live.exec_rules.insert(ordinal, rules.clone());
    }
    live.grants.insert(ordinal, (g.kind, g.stored.clone()));
    designations.push((word, handle(generation, ordinal)));
    Ok(snapshot(ordinal as u64, g))
}

/// Designate each arrived word on the receiving unit's cell. A word that
/// arrives again is designated anew: the latest arrival's ordinal is the
/// one its later operations use.
pub(in crate::tape) fn designate(
    group: Group,
    registry: &mut Registry,
    designations: &[(u64, u64)],
) {
    for &(word, h) in designations {
        registry.designate_in_unit(word, &group.cell.provider, h);
    }
}
