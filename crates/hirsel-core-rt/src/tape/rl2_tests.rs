use super::*;

#[test]
fn rl2_missing_close_and_buffer_fit() {
    let (original, _, verdict) = replay(HAND, 70_000, Variant::Same, &ending()).unwrap();
    assert_eq!(verdict, Ok(()));
    let (_, _, verdict) = replay(HAND, 70_000, Variant::KeepFile, &ending()).unwrap();
    assert_eq!(
        verdict,
        Err(Fault::Diverged(Divergence {
            at: 15,
            kind: Mismatch::Extra
        }))
    );
    let (short, _, verdict) = replay(HAND, 69_999, Variant::Same, &ending()).unwrap();
    assert_eq!(
        verdict,
        Err(Fault::Diverged(Divergence {
            at: 13,
            kind: Mismatch::Arg
        }))
    );
    assert!(
        short[8].bytes.is_empty(),
        "a refused read must leave its buffer untouched"
    );
    let (large, _, verdict) = replay(HAND, 70_001, Variant::Same, &ending()).unwrap();
    assert_eq!(verdict, Ok(()));
    assert_eq!(original, large);
}
