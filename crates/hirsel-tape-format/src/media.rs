//! Media capture operands in the canonical argument encoding.
use crate::{Args, Corrupt};

pub const KEY_UPDATE: u8 = 12;
const NEXT_OPENER: u8 = 24;

const FIRST: u8 = 1;
const SECOND: u8 = 2;
const ARRIVAL: u8 = 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Operand {
    Delivery(u64),
    First(u64),
    Second(u64),
    Arrival(u64),
}

impl Operand {
    pub fn wire(self, bytes: &mut [u8; 9]) -> &[u8] {
        let (ordinal, tag) = match self {
            Self::Delivery(ordinal) => (ordinal, None),
            Self::First(ordinal) => (ordinal, Some(FIRST)),
            Self::Second(ordinal) => (ordinal, Some(SECOND)),
            Self::Arrival(ordinal) => (ordinal, Some(ARRIVAL)),
        };
        bytes[..8].copy_from_slice(&ordinal.to_le_bytes());
        match tag {
            Some(tag) => {
                bytes[8] = tag;
                bytes
            }
            None => &bytes[..8],
        }
    }

    fn decode(bytes: &[u8]) -> Result<Self, Corrupt> {
        let (ordinal, tag) = bytes.split_first_chunk::<8>().ok_or(Corrupt)?;
        let ordinal = u64::from_le_bytes(*ordinal);
        match tag {
            [] => Ok(Self::Delivery(ordinal)),
            [FIRST] => Ok(Self::First(ordinal)),
            [SECOND] => Ok(Self::Second(ordinal)),
            [ARRIVAL] => Ok(Self::Arrival(ordinal)),
            _ => Err(Corrupt),
        }
    }
}

pub(super) fn validate_args(args: &Args) -> Result<(), Corrupt> {
    let Args::Literal(bytes) = args.operands() else {
        return Ok(());
    };
    // These operations have an origin operand and fit within ARGS_LITERAL.
    // Spare is zero, and each field length precedes the concatenated operands.
    let (len, operands) = match bytes.as_slice() {
        [KEY_UPDATE, 0, len, 1, operands @ ..] => {
            let (_, origin) = operands.split_last().ok_or(Corrupt)?;
            (*len, origin)
        }
        [NEXT_OPENER, 0, len, operands @ ..] => (*len, operands),
        [KEY_UPDATE | NEXT_OPENER, ..] => return Err(Corrupt),
        _ => return Ok(()),
    };
    if usize::from(len) != operands.len() {
        return Err(Corrupt);
    }
    Operand::decode(operands)?;
    Ok(())
}
