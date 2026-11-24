use crate::{Capacity, Context, limit::Payer};

/// Bytes admitted from `payer` and held for the unit runtime; the limit
/// record stays live while they are outstanding. Dropping a part refunds it
/// at the unit.
pub(super) struct Held {
    pub(super) payer: Payer,
    pub(super) bytes: u64,
}

impl Held {
    pub(super) fn admit(context: &Context, bytes: u64) -> Result<Self, Capacity> {
        let payer = context.payer();
        payer.admit(Some(bytes))?;
        Ok(Held { payer, bytes })
    }

    /// A part of at most `bytes`, refunded on its own.
    pub(super) fn split(&mut self, bytes: u64) -> Held {
        let bytes = bytes.min(self.bytes);
        self.bytes -= bytes;
        Held {
            payer: self.payer,
            bytes,
        }
    }

    /// Hand `bytes` to an owner that refunds them itself.
    pub(super) fn partition(&mut self, bytes: u64) -> Payer {
        self.bytes -= bytes;
        self.payer
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        if self.bytes != 0 {
            self.payer.settle_at_unit(self.bytes);
        }
    }
}
