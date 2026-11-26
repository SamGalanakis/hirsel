//! A word that arrives in a replaying unit inside a value, a helper's entry,
//! its result or an item, made from its recorded part (docs/recording.md,
//! "Handle snapshots"). It takes the receiving unit's next object ordinal, as
//! the recorder numbered it there. A `Secret` or `KeyShare` arrives with no
//! bytes and no charge: the stream that made it delivers its operations.
use super::super::super::*;
use super::Unpacked;
use crate::host_file::RunGrants;
use crate::secret::{Made, Origin};
use hirsel_core_layout::cross::{CAPABILITY, CLIENT, FUNCTION, MEDIA, SECRET, SHARED, SOCKET};
use hirsel_core_net::{SecretPolicy, key_agreement::Group};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

/// The word of one owner part of `kind`, read from `c`: a function's code
/// word, a `Secret`'s new designation in the run's secrets, or a key share
/// with no key, both with no bytes behind them. Each other handle is refused
/// by name; a helper's entry makes its capabilities itself.
pub(in crate::tape) fn word(
    live: &mut Live,
    grants: Option<&RunGrants>,
    kind: u64,
    c: &mut format::Cursor<'_>,
) -> Result<u64, Unpacked> {
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
        (CAPABILITY, 1) => Err(Unpacked::Unsupported(CAPABILITY_HELD)),
        (MEDIA, 1) => match c.u8()? {
            8 => key_share(live, grants, c),
            9..=12 => Err(Unpacked::Unsupported(KEY_OBJECT)),
            _ => Err(Unpacked::Layout),
        },
        (CLIENT, 1) => client(live, c),
        (SHARED | SOCKET, 1) => Err(Unpacked::Unsupported(HANDLE)),
        _ => Err(Unpacked::Layout),
    }
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

const NO_CODE: &str = "a value holding a function the replaying build does not have";
const DERIVED: &str = "a value holding a Secret derived or unwrapped in another unit";
const NO_FILES: &str = "a value holding a Secret or key share in a unit without host files";
const CAPABILITY_HELD: &str = "a helper result or an item holding a capability";
const KEY_OBJECT: &str = "a value holding a schedule, a sealer, an opener or a header mask";
const HANDLE: &str = "a value holding a Shared holder or a socket";
const ACROSS_CLIENT: &str = "a client arriving in a replay of another revision";
