//! A granted capability that crosses units inside an item (docs/recording.md,
//! "Handle snapshots", unit tapes U3). Its grant is read from the cell that
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

const UNGRANTED: &str = "an item holds a capability not granted through the recording";
const VIRTUAL: &str =
    "an item holds a capability of virtual construction, whose snapshot this build does not write";
const NARROWED: &str =
    "an item holds a narrowed capability, whose snapshot this build does not write";
const KIND: &str =
    "an item holds a Net, Env or Cred capability, whose snapshot this build does not write";
const UNRESOLVED: &str = "an item holds a capability the recording did not resolve";

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
    if !matches!(kind, 0 | 2..=6 | 8 | 13) {
        return Err(KIND);
    }
    Ok(Grant {
        kind,
        stored,
        inner: live.objects[o - 1].inner,
        exec: live.exec_rules.get(&o).cloned(),
    })
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
pub(super) fn sent(grants: &[Granted], word: u64) -> Result<Vec<u8>, &'static str> {
    find(grants, word).map(|g| snapshot(0, g))
}

/// An arriving capability: a new object ordinal in the receiving unit, its
/// snapshot, and its word's designation there, applied once the whole item
/// is written.
pub(super) fn arrived(
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
pub(super) fn designate(group: Group, registry: &mut Registry, designations: &[(u64, u64)]) {
    for &(word, h) in designations {
        registry.designate_in_unit(word, &group.cell.provider, h);
    }
}
