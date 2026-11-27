//! A static value as the text form prints it.
use super::*;

pub(super) fn static_value(value: &StaticValue) -> String {
    match value {
        StaticValue::Bits(bits) => bits.to_string(),
        StaticValue::Bytes(_) => unreachable!("static bytes are bound as a bytes static"),
        StaticValue::Fields { case, fields } => {
            let fields = fields
                .iter()
                .map(static_value)
                .collect::<Vec<_>>()
                .join(", ");
            match case {
                Some(case) => format!("case {case} [{fields}]"),
                None => format!("[{fields}]"),
            }
        }
    }
}

/// A constant's bits as text: decimal for integers, bool and char; hex bits for floats.
pub(super) fn constant(t: Ty, bits: u64) -> String {
    match t {
        Ty::Scalar(Scalar::Bool) => (if bits == 0 { "false" } else { "true" }).into(),
        Ty::Scalar(Scalar::F32 | Scalar::F64) | Ty::Ptr => format!("{bits:#x}"),
        t => match t.int() {
            Some((true, w)) => (((bits << (64 - w)) as i64) >> (64 - w)).to_string(),
            _ => bits.to_string(),
        },
    }
}
