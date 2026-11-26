use super::{fit, int, mask, width};
use hirsel_core_ir::{BinaryOp, Ty};

pub(super) fn mul_high(a: u64, b: u64) -> u64 {
    ((u128::from(a) * u128::from(b)) >> 64) as u64
}

pub(super) fn int_binary(t: Ty, op: BinaryOp, a: u64, c: u64) -> Option<u64> {
    let (x, y) = (int(t, a), int(t, c));
    let w = width(t);
    match op {
        BinaryOp::Add => fit(t, x + y),
        BinaryOp::Sub => fit(t, x - y),
        BinaryOp::Mul => fit(t, x.checked_mul(y)?),
        BinaryOp::Div => fit(t, x.checked_div(y)?),
        BinaryOp::Rem => (y != 0).then(|| mask(t, (x % y) as u64)),
        BinaryOp::Shl | BinaryOp::Shr if !(0..i128::from(w)).contains(&y) => None,
        BinaryOp::Shl => Some(mask(t, a << y)),
        BinaryOp::Shr => Some(mask(t, (x >> y) as u64)),
        BinaryOp::BitAnd => Some(a & c),
        BinaryOp::BitOr => Some(a | c),
        BinaryOp::BitXor => Some(a ^ c),
        BinaryOp::MulHigh => Some(mul_high(a, c)),
        BinaryOp::Rotl => {
            let (x, n) = (mask(t, a), mask(t, c) % u64::from(w));
            Some(mask(t, x << n | x.checked_shr(w - n as u32).unwrap_or(0)))
        }
        BinaryOp::Min => Some(if x <= y { a } else { c }),
        BinaryOp::Max => Some(if x >= y { a } else { c }),
        BinaryOp::MinNumber | BinaryOp::MaxNumber => unreachable!("float only"),
    }
}
