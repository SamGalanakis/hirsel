use super::*;

type VectorBytes = [u8; 32];

/// Scalar registers retain their existing one-word storage. Only a frame that
/// actually uses a vector inserts entries in this table.
#[derive(Default)]
pub(super) struct Vectors(HashMap<ValueId, VectorBytes>);

impl Vectors {
    pub(super) fn value(&self, id: ValueId) -> VectorBytes {
        self.0[&id]
    }

    pub(super) fn set(&mut self, id: ValueId, bytes: VectorBytes) {
        self.0.insert(id, bytes);
    }

    pub(super) fn push(&self, b: &Body, vals: &[u64], id: ValueId, words: &mut Vec<u64>) {
        if let Ty::Vector(shape) = b.values[id.index()] {
            let bytes = self.value(id);
            for chunk in bytes[..shape.size() as usize].chunks_exact(8) {
                words.push(u64::from_le_bytes(chunk.try_into().unwrap()));
            }
        } else {
            words.push(vals[id.index()]);
        }
    }

    pub(super) fn unpack(&mut self, b: &Body, vals: &mut [u64], ids: &[ValueId], words: &[u64]) {
        let mut words = words.iter().copied();
        for &id in ids {
            if let Ty::Vector(shape) = b.values[id.index()] {
                let mut bytes = [0; 32];
                for chunk in bytes[..shape.size() as usize].chunks_exact_mut(8) {
                    chunk.copy_from_slice(&words.next().unwrap().to_le_bytes());
                }
                self.set(id, bytes);
            } else if let Some(word) = words.next() {
                vals[id.index()] = word;
            }
        }
    }
}

pub(super) fn word_count(ty: Ty) -> usize {
    match ty {
        Ty::Vector(shape) => (shape.size() / 8) as usize,
        _ => 1,
    }
}

fn get_lane(bytes: &VectorBytes, shape: Vector, lane: usize) -> u64 {
    let width = shape.lane().size() as usize;
    let offset = lane * width;
    let mut word = [0; 8];
    word[..width].copy_from_slice(&bytes[offset..offset + width]);
    u64::from_le_bytes(word)
}

fn make_vector(shape: Vector, mut lane: impl FnMut(usize) -> u64) -> VectorBytes {
    let mut bytes = [0; 32];
    let width = shape.lane().size() as usize;
    for (i, at) in bytes[..shape.size() as usize]
        .chunks_exact_mut(width)
        .enumerate()
    {
        at.copy_from_slice(&lane(i).to_le_bytes()[..width]);
    }
    bytes
}

pub(super) enum ResultValue {
    Scalar(u64),
    Vector(VectorBytes),
}

pub(super) fn apply(
    op: SimdOp,
    result: Ty,
    first: Ty,
    a: VectorBytes,
    b: VectorBytes,
    scalar: u64,
) -> X<ResultValue> {
    let shape = |ty| match ty {
        Ty::Vector(v) => v,
        _ => unreachable!("integer vector"),
    };
    use SimdOp::*;
    let bytes = match op {
        From | Lanes => unreachable!("vector loads are handled by the evaluator's memory"),
        Cast => a,
        Splat => make_vector(shape(result), |_| scalar),
        Swizzle => {
            let table = shape(first);
            let out = shape(result);
            make_vector(out, |i| {
                let index = b[i] as usize;
                if index < table.lanes() as usize {
                    u64::from(a[index])
                } else {
                    0
                }
            })
        }
        Any => {
            let shape = shape(first);
            let any = a[..shape.size() as usize].iter().any(|&byte| byte != 0);
            return Ok(ResultValue::Scalar(u64::from(any)));
        }
        Bitmask => {
            let shape = shape(first);
            let top = shape.lane().size() * 8 - 1;
            let mask = (0..shape.lanes() as usize)
                .fold(0, |mask, i| mask | ((get_lane(&a, shape, i) >> top) << i));
            return Ok(ResultValue::Scalar(mask));
        }
        MaddI16 => {
            let from = shape(first);
            let lane = Ty::Scalar(from.lane());
            let product = |i| int(lane, get_lane(&a, from, i)) * int(lane, get_lane(&b, from, i));
            make_vector(shape(result), |i| {
                (product(2 * i) + product(2 * i + 1)) as u64
            })
        }
        PackSaturating => {
            let from = shape(first);
            let out = shape(result);
            let wide = Ty::Scalar(from.lane());
            let (min, max) = range(Ty::Scalar(out.lane()));
            let half = from.lanes() as usize;
            make_vector(out, |i| {
                let word = if i < half {
                    get_lane(&a, from, i)
                } else {
                    get_lane(&b, from, i - half)
                };
                int(wide, word).clamp(min, max) as u64
            })
        }
        _ => {
            let shape = shape(result);
            let lane = Ty::Scalar(shape.lane());
            let count = if matches!(op, Shl | Shr) {
                let count = int(lane, scalar);
                if !(0..i128::from(width(lane))).contains(&count) {
                    return Err(Stop::Cause("Overflow"));
                }
                count as u32
            } else {
                0
            };
            make_vector(shape, |i| {
                let a = get_lane(&a, shape, i);
                let b = get_lane(&b, shape, i);
                match op {
                    WrappingAdd => a.wrapping_add(b),
                    WrappingSub => a.wrapping_sub(b),
                    WrappingMul => a.wrapping_mul(b),
                    MulhiI16 => ((int(lane, a) * int(lane, b)) >> 16) as u64,
                    SaturatingAdd | SaturatingSub => {
                        let (min, max) = range(lane);
                        let (a, b) = (int(lane, a), int(lane, b));
                        let exact = if op == SaturatingAdd { a + b } else { a - b };
                        exact.clamp(min, max) as u64
                    }
                    Eq => {
                        if a == b {
                            u64::MAX
                        } else {
                            0
                        }
                    }
                    Lt => {
                        if int(lane, a) < int(lane, b) {
                            u64::MAX
                        } else {
                            0
                        }
                    }
                    BitAnd => a & b,
                    BitOr => a | b,
                    BitXor => a ^ b,
                    BitNot => !a,
                    Shl => a << count,
                    Shr => (int(lane, a) >> count) as u64,
                    _ => unreachable!("handled above"),
                }
            })
        }
    };
    Ok(ResultValue::Vector(bytes))
}
