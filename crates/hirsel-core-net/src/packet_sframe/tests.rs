// bug: a ratchet from a base key longer than 32 bytes leaves the old key's tail in the slot,
// or derives from the stale length (TLS-8 PC11, base key slot erasure).
// oracle: Python `hmac` over RFC 9605 §5.1 and §4.4.2 (sframe-import-build-2702/oracle.py),
// base `00..3f`, one step, KID 9.
use super::*;
use crate::SframeDirection;

fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

#[test]
fn ratchet_overwrites_a_long_base_key_in_place() {
    let base: Vec<u8> = (0..64).collect();
    let mut schedule = Sframe::<()>::import(SframeDirection::Receive, Some(&base)).unwrap();
    let retired = schedule.advance(1);
    assert!(retired.is_empty());
    let slot = schedule.base_slot().unwrap();
    assert_eq!(
        slot[..32],
        hex("960090c2835894f71950d616161898b8a692ab90df271dad97602c3d6b1d036a")[..]
    );
    assert_eq!(slot[32..], [0; 32]);
    let opener = schedule.opener(0, 9, ());
    let keys = opener.keys.unwrap();
    assert_eq!(keys.payload(), hex("f3b3da94dcd5d9c9d8b1f6f9b513be43"));
    assert_eq!(keys.iv()[..], hex("fec067cb406075a7fa5706d0")[..]);
}
