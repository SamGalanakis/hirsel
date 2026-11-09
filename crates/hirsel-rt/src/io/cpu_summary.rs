//! Pointer-free CPU retirement receipts. Physical publication and retries never
//! reach this record; its charges are the prefix actually offered at the head.
use crate::budget::stretch::{PIECES_MAX, RETAINED_MAX, Stretch};
use hirsel_run_schema::BudgetKind;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CpuPiece {
    pub credit: u64,
    pub net: i64,
    pub high: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CpuSummary {
    pub version: u32,
    /// Serial prefix number within this batch, independent of physical width.
    pub retirement: u64,
    pub ordinal: u64,
    /// Initialized fields in recipe order, excluding padding and addresses.
    pub value: Vec<u8>,
    pub stop: Option<BudgetKind>,
    pub native: Option<CpuNativeSummary>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CpuNativeSummary {
    pub descriptor: u32,
    pub value_bytes: u64,
    pub retained_count: u32,
    pub pieces: Vec<CpuPiece>,
    pub level: u64,
    pub limit: u64,
}

impl CpuSummary {
    pub(crate) fn pieces(charges: &Stretch) -> Vec<CpuPiece> {
        charges
            .pieces()
            .map(|(credit, piece)| CpuPiece {
                credit,
                net: piece.net,
                high: piece.high,
            })
            .collect()
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.version != 4
            || self.ordinal >= 32
            || self.value.len() > super::super::tasks::ordered::values::BYTES_MAX
            || self.native.as_ref().is_some_and(|native| !native.valid())
        {
            return Err("invalid settled CPU summary".into());
        }
        Ok(())
    }
}

impl CpuNativeSummary {
    fn valid(&self) -> bool {
        self.value_bytes <= 256
            && self.retained_count <= RETAINED_MAX as u32
            && !self.pieces.is_empty()
            && self.pieces.len() <= PIECES_MAX
            && self.pieces[0].credit == 0
            && self
                .pieces
                .iter()
                .all(|piece| piece.high >= 0 && piece.net <= piece.high)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tape_wire::{Wire, decode};

    #[test]
    fn cpu_summary_decoder_enforces_version_and_original_summary_bounds() {
        let summary = CpuSummary {
            version: 4,
            retirement: 0,
            ordinal: 0,
            value: 42u64.to_le_bytes().to_vec(),
            stop: None,
            native: Some(CpuNativeSummary {
                descriptor: 0,
                value_bytes: 8,
                retained_count: 0,
                pieces: vec![CpuPiece {
                    credit: 0,
                    net: 16,
                    high: 16,
                }],
                level: 16,
                limit: 64,
            }),
        };
        for change in [
            (|s: &mut CpuSummary| s.version = 1) as fn(&mut CpuSummary),
            |s| s.ordinal = 32,
            |s| s.native.as_mut().unwrap().value_bytes = 257,
            |s| s.native.as_mut().unwrap().retained_count = RETAINED_MAX as u32 + 1,
            |s| s.native.as_mut().unwrap().pieces.clear(),
            |s| {
                let native = s.native.as_mut().unwrap();
                native
                    .pieces
                    .resize(PIECES_MAX + 1, native.pieces[0].clone());
            },
            |s| s.native.as_mut().unwrap().pieces[0].credit = 1,
            |s| s.native.as_mut().unwrap().pieces[0].high = -1,
            |s| s.native.as_mut().unwrap().pieces[0].high = 15,
        ] {
            let mut invalid = summary.clone();
            change(&mut invalid);
            let mut bytes = Vec::new();
            invalid.encode(&mut bytes);
            assert!(decode::<CpuSummary>(&bytes).is_err(), "{invalid:?}");
        }
    }
}
