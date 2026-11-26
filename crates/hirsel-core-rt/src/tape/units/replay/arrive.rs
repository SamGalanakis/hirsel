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
        (CLIENT | SHARED | SOCKET, 1) => Err(Unpacked::Unsupported(HANDLE)),
        _ => Err(Unpacked::Layout),
    }
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
const HANDLE: &str = "a value holding a client, a Shared holder or a socket";
