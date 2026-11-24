use crate::charge;

/// Bug: charge-v1 transcribed wrongly (MEM-24). Hand values: List of 3 three-byte
/// elements: 9 → 16; Map of 4 entries u64→u64: 4 × (8+8+16) = 128; Rc of a 12-byte
/// payload: 20 → 24; channel of 2 items of 24: 48 + 64 = 112; growth from 0 needing 1 is 4,
/// from 4 needing 5 is 8, from 8 needing 100 is 100; overflow is None, not a wrap.
/// A task with an 8-byte result and `bytes 4096`: 256 + 8 + 32 + 4096 = 4392; a
/// 16-byte resource with `grant bytes 256`: 16 + 32 + 256 = 304; a standalone Text
/// of 5 bytes: 8 + 5 = 13 → 16; a span builder of capacity 3 for u32: 12 + 8 = 20 →
/// 24; `seq.index_in` of 3 elements: 24 + 3 × 8 = 48.
#[test]
fn charge_v1_matches_the_spec_table() {
    assert_eq!(charge::seq(3, 3), Some(16));
    assert_eq!(charge::map(4, 8, 8), Some(128));
    assert_eq!(charge::counted(12), Some(24));
    assert_eq!(charge::task(8, 4096), Some(4392));
    assert_eq!(charge::resource(16, 256), Some(304));
    assert_eq!(charge::standalone(5), Some(16));
    assert_eq!(charge::span_builder(3, 4), Some(24));
    assert_eq!(charge::channel(2, 24), Some(112));
    assert_eq!(charge::seq_index(3), Some(48));
    assert_eq!(charge::grown_capacity(0, 1), Some(4));
    assert_eq!(charge::grown_capacity(4, 5), Some(8));
    assert_eq!(charge::grown_capacity(8, 100), Some(100));
    assert_eq!(charge::seq(u64::MAX, 2), None);
    assert_eq!(charge::round8(u64::MAX - 3), None);
    // FORM #113, hand-derived floor((2^64 - 8) / s) for logical size s: a bound one too
    // high would let LLVM assume a length the admission (MEM-22, MEM-24) never grants.
    // s = 3: 3 × 6148914691236517202 = 2^64 - 10, rounds to 2^64 - 8; one more is past 2^64 - 1.
    // s = 9 is a (u64, u8) record (stride 16): the spec's bound is by the 9, not the 16.
    for (s, bound) in [
        (2, 9223372036854775804),
        (3, 6148914691236517202),
        (8, 2305843009213693951),
        (9, 2049638230412172400),
        (24, 768614336404564650),
    ] {
        assert_eq!(charge::seq_len_bound(s), Some(bound), "size {s}");
        assert!(charge::seq(bound, s).is_some(), "size {s} admits its bound");
        assert_eq!(charge::seq(bound + 1, s), None, "size {s} refuses one past");
    }
    assert_eq!(charge::seq_len_bound(1), None);
    assert_eq!(charge::seq_len_bound(0), None);
}
