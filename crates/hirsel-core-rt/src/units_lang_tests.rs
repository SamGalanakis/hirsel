//! CONC-15/17 through the emitters' entry points.
use super::bench::run;

/// A root and a helper exchange items, a client inside one; the helper ends
/// when its inbox closes, and every byte both admitted is refunded.
#[test]
fn units_lang_echo() {
    assert_eq!(run(64)[0], 64);
}
