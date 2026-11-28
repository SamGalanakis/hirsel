//! `Convert` on the evaluator.
use super::*;

/// A `Convert` of `x` from `from` to `to`: the result and its flag.
pub(crate) fn convert(mode: ConvertMode, from: Ty, to: Ty, x: u64) -> (u64, u64) {
    let char_ty = Ty::Scalar(Scalar::Char);
    match mode {
        ConvertMode::Widen if from == char_ty => (x, 0),
        ConvertMode::TryNarrow if to == char_ty => {
            let ok = (x < 0xD800) || (0xE000..=0x10FFFF).contains(&x);
            (if ok { x } else { 0 }, u64::from(ok))
        }
        ConvertMode::Widen => (fit(to, int(from, x)).expect("widen fits"), 0),
        ConvertMode::Truncate => (mask(to, x), 0),
        // The source's value, not its bits: a signed source widened by a narrow keeps its sign.
        ConvertMode::TryNarrow => {
            let v = int(from, x);
            (mask(to, v as u64), u64::from(fit(to, v).is_some()))
        }
        ConvertMode::FloatToInt => {
            let f = float(from, x);
            let (lo, hi) = range(to);
            if f.is_nan() {
                (0, 1)
            } else if f.trunc() >= lo as f64 && f.trunc() < (hi + 1) as f64 {
                (mask(to, f.trunc() as i128 as u64), 0)
            } else {
                (0, 2)
            }
        }
        ConvertMode::IntToFloat => {
            let v = int(from, x);
            let r = if is_f32(to) {
                u64::from((v as f32).to_bits())
            } else {
                (v as f64).to_bits()
            };
            (r, 0)
        }
        ConvertMode::Reinterpret if from.is_float() => (bits(from, float(from, x)), 0),
        ConvertMode::Reinterpret => (bits(to, float(to, x)), 0),
        ConvertMode::FloatResize => (bits(to, float(from, x)), 0),
    }
}
