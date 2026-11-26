//! A word that arrives in a replaying unit inside a value, a helper's entry,
//! its result or an item, made from its recorded part (docs/recording.md,
//! "Handle snapshots"). It takes the receiving unit's next object ordinal, as
//! the recorder numbered it there. A `Secret` or `KeyShare` arrives with no
//! bytes and no charge: the stream that made it delivers its operations. A
//! granted capability arrives as a grant of the receiving unit's registry on
//! its own cell, so its operations are that unit's.
use super::super::super::*;
use super::Unpacked;
use crate::host_file::{Registry, RunGrants};
use crate::secret::{Made, Origin};
use hirsel_core_layout::cross::{CAPABILITY, CLIENT, FUNCTION, MEDIA, SECRET, SHARED, SOCKET};
use hirsel_core_net::{SecretPolicy, key_agreement::Group};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

/// Where a value arrives: the receiving unit's registry, if it has host
/// files, and the cell and generation of its tape.
#[derive(Clone, Copy)]
pub(in crate::tape) struct Receiver<'a> {
    pub(in crate::tape) registry: Option<&'a Registry>,
    pub(in crate::tape) provider: Provider,
    pub(in crate::tape) generation: u32,
}

/// The word of one owner part of `kind`, read from `c`: a function's code
/// word, a granted capability's new grant, a `Secret`'s new designation in
/// the run's secrets, or a key share with no key, both with no bytes behind
/// them. Each other handle is refused by name.
pub(in crate::tape) fn word(
    live: &mut Live,
    at: Receiver<'_>,
    kind: u64,
    c: &mut format::Cursor<'_>,
) -> Result<u64, Unpacked> {
    let grants = at.registry.map(Registry::run_grants);
    match (kind, c.u8()?) {
        (FUNCTION, 2) => {
            let identity = c.bytes()?;
            run::identities::code_of(identity).ok_or(Unpacked::Unsupported(NO_CODE))
        }
        (SECRET, 1) => {
            if c.u8()? != 7 || c.varint()? != live.unit_object() {
                return Err(Unpacked::Layout);
            }
            let made = c.u8()?;
            let stream = c.u32()?;
            let ordinal = c.varint()?;
            let made = match made {
                0 => Made::Granted(ordinal),
                1 => Made::Sealed(ordinal),
                4 => Made::Fetched(ordinal),
                5 => Made::GeneratedP256(ordinal),
                6 => Made::Prompted(ordinal),
                // Whether the program held a derived or unwrapped secret's
                // bytes follows its parent's, which the designation omits.
                2 | 3 => return Err(Unpacked::Unsupported(DERIVED)),
                _ => return Err(Unpacked::Layout),
            };
            let policy = std::str::from_utf8(c.bytes()?)
                .ok()
                .and_then(SecretPolicy::parse)
                .ok_or(Unpacked::Layout)?;
            let grants = grants.ok_or(Unpacked::Unsupported(NO_FILES))?;
            Ok(grants
                .secrets
                .lock()
                .unwrap()
                .arrived(Origin { made, stream }, policy))
        }
        (CAPABILITY, 1) => capability(live, at, c),
        (MEDIA, 1) => match c.u8()? {
            8 => key_share(live, grants, c),
            9..=12 => Err(Unpacked::Unsupported(KEY_OBJECT)),
            _ => Err(Unpacked::Layout),
        },
        (CLIENT, 1) => client(live, c),
        (SHARED, 1) => shared(live, at, c),
        (SOCKET, 1) => socket(live, c),
        _ => Err(Unpacked::Layout),
    }
}

/// A capability from its snapshot after the tag (recording.md, "Snapshots"):
/// tag 5, its ordinal where it arrives, its kind and stored bytes. It is
/// granted on the receiving unit's cell, which keeps its grant so a value
/// that carries it on snapshots it again.
fn capability(
    live: &mut Live,
    at: Receiver<'_>,
    c: &mut format::Cursor<'_>,
) -> Result<u64, Unpacked> {
    match c.u8()? {
        5 => {}
        6 => return Err(Unpacked::Unsupported(VIRTUAL)),
        _ => return Err(Unpacked::Layout),
    }
    let ordinal = c.varint()?;
    let kind = c.u8()?;
    // Net, Env, Exec and Cred replay against the rules the root's startup
    // installs by its own ordinals, which an arriving grant's lacks.
    if matches!(kind, 1 | 7 | 13 | 14) {
        return Err(Unpacked::Unsupported(RULED_GRANT));
    }
    let stored = c.bytes()?.to_vec();
    let registry = at.registry.ok_or(Unpacked::Unsupported(NO_FILES))?;
    if ordinal != live.unit_object() {
        return Err(Unpacked::Layout);
    }
    let grant = super::super::item_grants::CrossGrant::granted(kind, stored);
    live.grants.insert(ordinal as usize, grant);
    Ok(registry.grant(at.provider, handle(at.generation, ordinal as usize)))
}

/// Whether this replay reads a tape of another revision (`--diagnose`).
static ACROSS: AtomicBool = AtomicBool::new(false);

/// The replay reads a tape whose revision is not the replaying build's.
pub(in crate::tape) fn set_across(differs: bool) {
    ACROSS.store(differs, Relaxed);
}

/// A client from its snapshot after the tag (recording.md, "Snapshots"): a
/// stand-in with the item layout of the connection it names, numbered where
/// it arrives. A tape of another revision is refused: its recorded layout
/// need not be the replaying send site's.
fn client(live: &mut Live, c: &mut format::Cursor<'_>) -> Result<u64, Unpacked> {
    if c.u8()? != 0 {
        return Err(Unpacked::Layout);
    }
    if ACROSS.load(Relaxed) {
        return Err(Unpacked::Unsupported(ACROSS_CLIENT));
    }
    let ordinal = c.varint()?;
    let stream = c.u32()?;
    let of = c.varint()?;
    let [cap, room, logical, bytes] = [c.u64()?, c.u64()?, c.u64()?, c.u64()?];
    let rows = c.varint()?;
    let mut owners = Vec::new();
    for _ in 0..rows {
        owners.push([c.varint()?, c.varint()?, c.varint()?, c.varint()?]);
    }
    if ordinal != live.unit_object() {
        return Err(Unpacked::Layout);
    }
    let (conn, word) = crate::units_lang::replay::stand_in(cap, logical, room, bytes, &owners)
        .ok_or(Unpacked::Layout)?;
    let of = super::super::StandIn {
        stream,
        ordinal: of,
        cap,
        room,
        logical,
        bytes,
        owners,
    };
    super::super::stand_in(live, conn, word as usize, ordinal, of);
    Ok(word)
}

/// A `KeyShare` from its snapshot after the tag (recording.md, "Snapshots"):
/// its ordinal, media origin and group. Its agreement takes the recorded
/// verdict, so it holds no key, and the sending unit paid for it.
fn key_share(
    live: &mut Live,
    grants: Option<&RunGrants>,
    c: &mut format::Cursor<'_>,
) -> Result<u64, Unpacked> {
    let ordinal = c.varint()?;
    if ordinal != live.unit_object() {
        return Err(Unpacked::Layout);
    }
    let stream = c.u32()?;
    let delivery = c.varint()?;
    let admission = if delivery == 0 { c.varint()? } else { 0 };
    let group = Group::from_case(c.u8()?.into()).ok_or(Unpacked::Layout)?;
    let grants = grants.ok_or(Unpacked::Unsupported(NO_FILES))?;
    let ticket = crate::media::run_state(grants).agreements.arrived_share();
    Ok(crate::media::hold(crate::secret::key_share::Share {
        key: None,
        ticket,
        origin: (2, ordinal),
        charge: None,
        tape: crate::media::Origin::arrived((stream, delivery, admission), ordinal),
        group,
    }))
}

/// A `Shared` holder from its snapshot after the tag (recording.md, "Snapshots"):
/// tag 1, its arriving ordinal, home stream and home ordinal, then the payload
/// value. Allocates an uncharged replay stand-in node registered under its home.
fn shared(live: &mut Live, at: Receiver<'_>, c: &mut format::Cursor<'_>) -> Result<u64, Unpacked> {
    if c.u8()? != 1 {
        return Err(Unpacked::Layout);
    }
    let ordinal = c.varint()?;
    let home_stream = c.u32()?;
    let home_ordinal = c.varint()?;
    if ordinal != live.unit_object() {
        return Err(Unpacked::Layout);
    }
    let plain = c.bytes()?;
    let parts = c.varint()?;
    let (flat, bytes, owners) = if parts == 0 {
        (plain.to_vec(), plain.len(), Vec::new())
    } else {
        let (bytes, owners) = super::super::find_node_layout(home_stream, home_ordinal)
            .ok_or(Unpacked::Unsupported(FOREIGN_SHARED))?;
        let bytes = bytes as usize;
        let flat = super::packed_body(
            plain,
            parts,
            c,
            vec![0; bytes],
            (0, bytes),
            &owners,
            &mut |kind, c| word(live, at, kind, c),
        )?;
        (flat, bytes, owners)
    };
    // SAFETY: `flat` holds `bytes` flat bytes plus live `SEQ` element bytes laid out by `owners`.
    let node = unsafe { crate::shared::replay_stand_in(&flat, bytes, &owners) } as usize;
    super::super::shared_stand_in(
        live,
        node,
        ordinal,
        home_stream,
        home_ordinal,
        bytes as u64,
        owners,
    );
    Ok(node as u64)
}

/// A TCP `Socket` (tag 2) or `Listener` (tag 3) from its snapshot after the
/// tag (recording.md, "Snapshots").
fn socket(live: &mut Live, c: &mut format::Cursor<'_>) -> Result<u64, Unpacked> {
    let tag = c.u8()?;
    if tag == 4 {
        return Err(Unpacked::Unsupported(UDP_SOCKET));
    }
    if !matches!(tag, 2 | 3) {
        return Err(Unpacked::Layout);
    }
    let ordinal = c.varint()?;
    let snapshot = super::super::crossing::parse_snapshot(tag, c)?;
    if ordinal != live.unit_object() {
        return Err(Unpacked::Layout);
    }
    live.sockets.insert(ordinal as usize, snapshot);
    let key = ordinal << 2;
    live.takes
        .arrived_sockets
        .push((key, super::super::crossing::snapshot_origin(snapshot)));
    Ok(key)
}

const NO_CODE: &str = "a value holding a function the replaying build does not have";
const DERIVED: &str = "a value holding a Secret derived or unwrapped in another unit";
const NO_FILES: &str =
    "a value holding a capability, a Secret or a key share in a unit without host files";
const VIRTUAL: &str = "a value holding a capability of virtual construction";
const RULED_GRANT: &str = "a value holding a Net, Env, Exec or Cred capability";
const KEY_OBJECT: &str = "a value holding a schedule, a sealer, an opener or a header mask";
const FOREIGN_SHARED: &str = "a foreign Shared holder whose payload holds owners";
const UDP_SOCKET: &str = "a value holding a UdpSocket";
const ACROSS_CLIENT: &str = "a client arriving in a replay of another revision";
