//! A static value as the text form prints it.
use super::*;

pub(super) fn static_value(value: &StaticValue) -> String {
    match value {
        StaticValue::Bits(bits) => bits.to_string(),
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
