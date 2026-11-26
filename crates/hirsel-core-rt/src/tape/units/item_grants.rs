//! A granted or narrowed capability, or a `Secret` designation, that crosses
//! units inside a value: an item, a helper's entry or its result
//! (docs/recording.md, "Handle snapshots", unit tapes U3). Its grant is read from the cell that
//! recorded it before any cell is locked, since the sending or receiving
//! unit's own cell is locked while the item's value is written. Where it
//! arrives it takes a new object ordinal and the receiving unit's registry
//! designates its word on the receiver's own cell, so its operations there
//! reach the receiver's stream.
use super::super::schedule::Group;
use super::super::*;
use crate::host_file::{DYNAMIC_GRANT, Registry};
use hirsel_core_layout::cross;
use hirsel_core_net::{CredGrant, exec::ExecRules};
use std::sync::Arc;

/// One capability word's grant as the cell that recorded it holds it, or a
/// `Secret` word's designation.
pub(crate) struct Granted {
    kind: u64,
    word: u64,
    grant: Result<Held, &'static str>,
}

enum Held {
    Capability(Grant),
    /// A `Secret`'s tag 7 body: its designation and canonical policy text.
    Secret(Vec<u8>),
}

struct Grant {
    grant: CrossGrant,
    inner: Option<Inner>,
    exec: Option<Arc<ExecRules>>,
    /// On replay, a Net with no live source: its run-pinned rules.
    replay_net: Option<u64>,
}

/// A capability's grant as a value that carries it to another unit
/// snapshots it, and as the unit that holds it keeps it by ordinal.
#[derive(Clone)]
pub(in crate::tape) enum CrossGrant {
    /// Tag 5: the grant snapshot's kind and its stored bytes.
    Granted { kind: u8, stored: Vec<u8> },
    /// Tag 5 for a `Cred`: its snapshot, and the record the receiving unit
    /// binds to its own ordinal.
    Cred { stored: Vec<u8>, record: CredGrant },
    /// Tag 6: a narrowed `Exec`, of virtual construction, whose profile no
    /// snapshot holds; it names the stream it was narrowed in.
    Homed { stream: u32 },
}

impl CrossGrant {
    pub(in crate::tape) fn granted(kind: u8, stored: Vec<u8>) -> Self {
        Self::Granted { kind, stored }
    }
}

/// An arrived word's designation in the receiving unit: its handle on the
/// receiver's cell, or for a `Cred` the grant bound to that handle.
pub(in crate::tape) struct Designation {
    word: u64,
    handle: u64,
    cred: Option<CredGrant>,
}

impl Designation {
    /// Designate the word on `provider`, the receiving unit's cell.
    pub(in crate::tape) fn apply(&self, provider: *const Provider, registry: &mut Registry) {
        let handle = match &self.cred {
            None => self.handle,
            Some(record) => {
                // A fetch carries its grant's handle, so the receiver's own
                // grant names the receiver's ordinal.
                let bound = Box::new(
                    crate::cred::Granted::bound(record.clone(), self.handle)
                        .expect("a recorded Cred grant parses"),
                );
                let pointer = &*bound as *const crate::cred::Granted as u64;
                registry.run_grants().unit_creds.lock().unwrap().push(bound);
                pointer
            }
        };
        registry.designate_in_unit(self.word, provider, handle);
    }
}

const UNGRANTED: &str = "a value holds a capability not granted through the recording";
const VIRTUAL: &str =
    "a value holds a capability of virtual construction, whose snapshot this build does not write";
const NARROWED: &str =
    "a value holds a narrowed capability whose grant this build does not snapshot";
const HOST_SIZE: &str = "a value holds an Env or Cred capability whose snapshot a tape cannot hold";
const NET_RULES: &str = "a value holds a Net capability whose rules a snapshot cannot hold";
const UNRESOLVED: &str = "a value holds a capability the recording did not resolve";

/// Each of `words`, by row kind: a capability resolved through `registry`,
/// the unit's own designation or the run's; a `Secret` through `context`, a
/// context of the run, whose secrets lock is never held with a cell's.
/// # Safety
/// `context` is live, and each `Secret` word holds a live designation.
pub(crate) unsafe fn resolve(
    context: *const crate::Context,
    registry: &Registry,
    words: &[(u64, u64)],
) -> Vec<Granted> {
    words
        .iter()
        .map(|&(kind, word)| Granted {
            kind,
            word,
            grant: if kind == cross::SECRET {
                // SAFETY: by the contract.
                Ok(Held::Secret(unsafe { secret(context, word) }))
            } else {
                grant(registry, word).map(Held::Capability)
            },
        })
        .collect()
}

/// A `Secret` word's designation and policy. Its word is the run's, so any
/// unit's context resolves it.
unsafe fn secret(context: *const crate::Context, word: u64) -> Vec<u8> {
    // SAFETY: by the caller's contract.
    let secret = unsafe { crate::secret::resolve(context, word) };
    let mut body = Vec::new();
    secret.origin.put(&mut body);
    let policy = secret.policy.text();
    put_varint(&mut body, policy.len() as u64);
    body.extend_from_slice(policy.as_bytes());
    body
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
    if let Some(&kind) = registry.run_grants().host_grants.lock().unwrap().get(&word) {
        return host(provider, registry.grant_handle(word), kind);
    }
    let h = registry.grant_handle(word);
    let state = cell_of(provider.state).state.lock().unwrap();
    let live = state
        .live
        .as_ref()
        .filter(|_| state.generation == generation_of(h))
        .ok_or(UNGRANTED)?;
    let o = ordinal_of(h);
    let inner = live.objects[o - 1].inner;
    // On replay a Net has no source; its rules are the ones replay pinned.
    let replay_net = inner
        .is_none()
        .then(|| live.replay_net_rules.get(&o).copied())
        .flatten();
    let grant = match live.grants.get(&o).cloned() {
        Some(grant @ CrossGrant::Homed { .. }) => grant,
        Some(CrossGrant::Granted { kind, stored }) => CrossGrant::Granted {
            kind,
            stored: match kind {
                0 | 2..=6 | 8 | 9 | 13 => stored,
                // A Net's rules, granted or narrowed, are its source's handle.
                1 => net_rules(inner.map(|i| i.handle).or(replay_net).ok_or(NET_RULES)?)?,
                _ => return Err(UNGRANTED),
            },
        },
        // A Cred that arrived here is read through its grant's word.
        Some(CrossGrant::Cred { .. }) => return Err(UNGRANTED),
        // A narrowed Exec made here: its profile is no snapshot's, so it
        // crosses as a capability of virtual construction homed here.
        None if live.exec_rules.contains_key(&o) => CrossGrant::Homed {
            stream: super::stream(live),
        },
        None => return Err(NARROWED),
    };
    Ok(Grant {
        grant,
        inner,
        exec: live.exec_rules.get(&o).cloned(),
        replay_net,
    })
}

/// An `Env` or `Cred` grant, whose registry handle is the run's own grant.
/// An `Env` is read where it is used, so it needs no tape object behind
/// it; a `Cred`'s fetches reach the source behind its grant's tape handle.
fn host(provider: &Provider, h: u64, kind: u8) -> Result<Grant, &'static str> {
    if kind == crate::entry::ENV {
        // SAFETY: an Env word's handle is an Env the run pins.
        let env = unsafe { crate::env::from_handle(h) }.record();
        return Ok(Grant {
            grant: CrossGrant::Granted {
                kind,
                stored: format::env_snapshot(&env).map_err(|_| HOST_SIZE)?,
            },
            inner: None,
            exec: None,
            replay_net: None,
        });
    }
    // SAFETY: a Cred word's handle is a `cred::Granted` the run pins.
    let granted = unsafe { &*(h as *const crate::cred::Granted) };
    let state = cell_of(provider.state).state.lock().unwrap();
    let live = state
        .live
        .as_ref()
        .filter(|_| state.generation == generation_of(granted.handle))
        .ok_or(UNGRANTED)?;
    Ok(Grant {
        grant: CrossGrant::Cred {
            stored: format::cred_snapshot(&granted.record).map_err(|_| HOST_SIZE)?,
            record: granted.record.clone(),
        },
        inner: live.objects[ordinal_of(granted.handle) - 1].inner,
        exec: None,
        replay_net: None,
    })
}

/// A Net's rules as its snapshot holds them: the rule count, then each rule
/// as the header's grant snapshot writes it.
fn net_rules(rules: u64) -> Result<Vec<u8>, &'static str> {
    // SAFETY: a recorded Net's source handle is its run-pinned rule list,
    // whose rows the run keeps live.
    let rules = unsafe { (*(rules as *const crate::net::Rules)).as_slice() };
    format::net_snapshot(rules).map_err(|_| NET_RULES)
}

/// A capability snapshot: tag 5, `ordinal`, then the kind and its stored
/// bytes as the grant snapshot encodes them, or for a narrowed `Exec` tag 6,
/// `ordinal`, its kind and home stream; a `Secret`'s: tag 7, `ordinal`, then
/// its designation and policy.
fn snapshot(ordinal: u64, held: &Held) -> Vec<u8> {
    let mut out = vec![];
    match held {
        Held::Capability(g) => match &g.grant {
            CrossGrant::Homed { stream } => {
                // Tag 6: the kind and the home stream, no rules.
                out.push(6);
                put_varint(&mut out, ordinal);
                out.push(crate::entry::EXEC);
                out.extend_from_slice(&stream.to_le_bytes());
            }
            CrossGrant::Granted { kind, stored } => granted(&mut out, ordinal, *kind, stored),
            CrossGrant::Cred { stored, .. } => {
                granted(&mut out, ordinal, crate::entry::CRED, stored);
            }
        },
        Held::Secret(body) => {
            out.push(7);
            put_varint(&mut out, ordinal);
            out.extend_from_slice(body);
        }
    }
    out
}

/// Tag 5: `ordinal`, then the kind and its stored bytes as the grant
/// snapshot encodes them.
fn granted(out: &mut Vec<u8>, ordinal: u64, kind: u8, stored: &[u8]) {
    out.push(5);
    put_varint(out, ordinal);
    out.push(kind);
    put_varint(out, stored.len() as u64);
    out.extend_from_slice(stored);
}

fn find(grants: &[Granted], kind: u64, word: u64) -> Result<&Held, &'static str> {
    let g = grants
        .iter()
        .find(|g| g.kind == kind && g.word == word)
        .ok_or(UNRESOLVED)?;
    g.grant.as_ref().map_err(|why| *why)
}

/// A sent capability's or `Secret`'s snapshot; the canonical form leaves its
/// ordinal out.
pub(in crate::tape) fn sent(
    grants: &[Granted],
    kind: u64,
    word: u64,
) -> Result<Vec<u8>, &'static str> {
    find(grants, kind, word).map(|held| snapshot(0, held))
}

/// An arriving capability: a new object ordinal in the receiving unit, its
/// snapshot, and its word's designation there, applied once the whole item
/// is written. An arriving `Secret` takes a new ordinal and keeps its
/// designation and its word, which are the run's.
pub(in crate::tape) fn arrived(
    live: &mut Live,
    generation: u32,
    grants: &[Granted],
    kind: u64,
    word: u64,
    designations: &mut Vec<Designation>,
) -> Result<Vec<u8>, &'static str> {
    let held = find(grants, kind, word)?;
    let Held::Capability(g) = held else {
        let ordinal = live.unit_object();
        return Ok(snapshot(ordinal, held));
    };
    live.objects.push(Object {
        inner: g.inner,
        open_resource: false,
    });
    let ordinal = live.objects.len();
    if let Some(rules) = &g.exec {
        live.exec_rules.insert(ordinal, rules.clone());
    }
    if let Some(rules) = g.replay_net {
        live.replay_net_rules.insert(ordinal, rules);
    }
    live.grants.insert(ordinal, g.grant.clone());
    // An Env is read through the run's own grant wherever it is used, so its
    // word keeps the run's designation.
    let cred = match &g.grant {
        CrossGrant::Granted { kind, .. } if *kind == crate::entry::ENV => None,
        CrossGrant::Cred { record, .. } => Some(Some(record.clone())),
        _ => Some(None),
    };
    if let Some(cred) = cred {
        designations.push(Designation {
            word,
            handle: handle(generation, ordinal),
            cred,
        });
    }
    Ok(snapshot(ordinal as u64, held))
}

/// Designate each arrived word on the receiving unit's cell. A word that
/// arrives again is designated anew: the latest arrival's ordinal is the
/// one its later operations use.
pub(in crate::tape) fn designate(
    group: Group,
    registry: &mut Registry,
    designations: &[Designation],
) {
    for d in designations {
        d.apply(&group.cell.provider, registry);
    }
}
