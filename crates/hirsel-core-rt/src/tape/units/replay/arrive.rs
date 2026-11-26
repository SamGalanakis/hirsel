//! A word that arrives in a replaying unit inside a value, a helper's entry,
//! its result or an item, made from its recorded part (docs/recording.md,
//! "Handle snapshots"). It takes the receiving unit's next object ordinal, as
//! the recorder numbered it there.
use super::super::super::*;
use super::Unpacked;
use crate::secret::{Made, Origin, Secrets};
use hirsel_core_layout::cross::{CAPABILITY, CLIENT, FUNCTION, MEDIA, SECRET, SHARED, SOCKET};
use hirsel_core_net::SecretPolicy;
use std::sync::Mutex;

/// The word of one owner part of `kind`, read from `c`: a function's code
/// word, or a `Secret`'s new designation in `secrets`, with no bytes behind
/// it. Each other handle is refused by name; a helper's entry makes its
/// capabilities itself.
pub(in crate::tape) fn word(
    live: &mut Live,
    secrets: Option<&Mutex<Secrets>>,
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
            let secrets = secrets.ok_or(Unpacked::Unsupported(NO_SECRETS))?;
            Ok(secrets
                .lock()
                .unwrap()
                .arrived(Origin { made, stream }, policy))
        }
        (CAPABILITY, 1) => Err(Unpacked::Unsupported(CAPABILITY_HELD)),
        (MEDIA, 1) => Err(Unpacked::Unsupported(KEY_OBJECT)),
        (CLIENT | SHARED | SOCKET, 1) => Err(Unpacked::Unsupported(HANDLE)),
        _ => Err(Unpacked::Layout),
    }
}

const NO_CODE: &str = "a value holding a function the replaying build does not have";
const DERIVED: &str = "a value holding a Secret derived or unwrapped in another unit";
const NO_SECRETS: &str = "a value holding a Secret in a unit without host files";
const CAPABILITY_HELD: &str = "a helper result or an item holding a capability";
const KEY_OBJECT: &str = "a value holding a key object";
const HANDLE: &str = "a value holding a client, a Shared holder or a socket";
