//! Limits shared across the frames of one code-object decode.

use std::{cell::Cell, fmt, mem::size_of, time::Instant};

#[derive(Clone, Copy, Debug)]
pub struct DecodeLimits {
    pub bytes: usize,
    pub members: usize,
    pub slots: usize,
    pub depth: usize,
    pub deadline: Option<Instant>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DecodeLimit {
    Bytes,
    Members,
    Slots,
    Depth,
    Deadline,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DecodeError {
    Malformed(&'static str),
    Unsupported(&'static str),
    Limit(DecodeLimit),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(message) | Self::Unsupported(message) => f.write_str(message),
            Self::Limit(limit) => write!(f, "code decode limit: {limit:?}"),
        }
    }
}

impl std::error::Error for DecodeError {}

impl From<DecodeError> for std::io::Error {
    fn from(error: DecodeError) -> Self {
        let kind = match error {
            DecodeError::Malformed(_) => std::io::ErrorKind::InvalidData,
            DecodeError::Unsupported(_) => std::io::ErrorKind::Unsupported,
            DecodeError::Limit(DecodeLimit::Deadline) => std::io::ErrorKind::TimedOut,
            DecodeError::Limit(_) => std::io::ErrorKind::OutOfMemory,
        };
        Self::new(kind, error)
    }
}

pub type DecodeResult<T> = Result<T, DecodeError>;

/// The limits of one decode attempt. The byte limit bounds the memory the
/// attempt holds at once (launch.md "Admission", step 3): bytes are taken by a
/// [`Debit`] for the allocation it funds and given back when that debit drops.
/// Members, slots and nesting are cumulative or bounded work and never return.
pub struct DecodeBudget {
    bytes: Cell<usize>,
    members: Cell<usize>,
    slots: Cell<usize>,
    depth: usize,
    deadline: Option<Instant>,
}

impl DecodeBudget {
    pub fn new(limits: DecodeLimits) -> Self {
        Self {
            bytes: Cell::new(limits.bytes),
            members: Cell::new(limits.members),
            slots: Cell::new(limits.slots),
            depth: limits.depth,
            deadline: limits.deadline,
        }
    }

    /// A payer holding no bytes yet.
    pub fn debit(&self) -> Debit<'_> {
        Debit {
            budget: Some(self),
            bytes: Cell::new(0),
        }
    }

    fn check(&self) -> DecodeResult<()> {
        if self.deadline.is_some_and(|end| Instant::now() >= end) {
            return Err(DecodeError::Limit(DecodeLimit::Deadline));
        }
        Ok(())
    }
}

/// Bytes of a [`DecodeBudget`] held for live allocations. Charge the
/// allocations an owner keeps to that owner's debit; dropping the debit with
/// them gives the bytes back. A growing vector is funded for both its old and
/// new buffers until the old one is freed.
#[must_use = "dropping a debit gives its bytes back"]
pub struct Debit<'b> {
    /// `None` for a payer no limit applies to, such as a local producer's.
    budget: Option<&'b DecodeBudget>,
    bytes: Cell<usize>,
}

impl<'b> Debit<'b> {
    /// A payer that refuses nothing.
    pub fn unlimited() -> Self {
        Self {
            budget: None,
            bytes: Cell::new(0),
        }
    }

    /// Another payer on the same budget, for allocations with their own lifetime.
    pub fn scratch(&self) -> Debit<'b> {
        Debit {
            budget: self.budget,
            bytes: Cell::new(0),
        }
    }

    /// Keep `other`'s bytes for as long as this payer.
    pub fn absorb(&self, other: Debit<'b>) {
        debug_assert!(self.budget.map(std::ptr::from_ref) == other.budget.map(std::ptr::from_ref));
        self.bytes.set(self.bytes.get() + other.bytes.replace(0));
    }

    pub fn check(&self) -> DecodeResult<()> {
        self.budget.map_or(Ok(()), DecodeBudget::check)
    }

    pub fn check_depth(&self, depth: usize) -> DecodeResult<()> {
        self.check()?;
        if self.budget.is_some_and(|budget| depth > budget.depth) {
            return Err(DecodeError::Limit(DecodeLimit::Depth));
        }
        Ok(())
    }

    pub fn charge_bytes(&self, amount: usize) -> DecodeResult<()> {
        if let Some(budget) = self.budget {
            budget.check()?;
            charge(&budget.bytes, amount, DecodeLimit::Bytes)?;
            self.bytes.set(self.bytes.get() + amount);
        }
        Ok(())
    }

    pub fn charge_members(&self, amount: usize) -> DecodeResult<()> {
        let Some(budget) = self.budget else {
            return Ok(());
        };
        budget.check()?;
        charge(&budget.members, amount, DecodeLimit::Members)
    }

    pub fn charge_slots(&self, amount: usize) -> DecodeResult<()> {
        let Some(budget) = self.budget else {
            return Ok(());
        };
        budget.check()?;
        charge(&budget.slots, amount, DecodeLimit::Slots)
    }

    pub fn allocation<T>(&self, count: usize) -> DecodeResult<()> {
        self.charge_bytes(bytes_of::<T>(count)?)
    }

    /// Replace a buffer this payer funds at `old` bytes by one of `new` bytes.
    /// Both are held while `grow` runs. The old bytes are given back once it
    /// succeeds, and the new ones if it fails.
    pub(crate) fn regrow(
        &self,
        old: usize,
        new: usize,
        grow: impl FnOnce() -> bool,
    ) -> DecodeResult<()> {
        self.charge_bytes(new)?;
        if !grow() {
            self.release(new);
            return Err(DecodeError::Limit(DecodeLimit::Bytes));
        }
        self.release(old);
        Ok(())
    }

    /// Make room for `additional` more items in a vector this payer funds,
    /// doubling its capacity so pushes stay amortised.
    pub fn reserve<T>(&self, vec: &mut Vec<T>, additional: usize) -> DecodeResult<()> {
        self.check()?;
        if additional > vec.capacity() - vec.len() {
            let count = vec
                .len()
                .checked_add(additional)
                .ok_or(DecodeError::Limit(DecodeLimit::Bytes))?;
            let capacity = count.max(vec.capacity().saturating_mul(2));
            let old = bytes_of::<T>(vec.capacity())?;
            self.regrow(old, bytes_of::<T>(capacity)?, || {
                vec.try_reserve_exact(capacity - vec.len()).is_ok()
            })?;
        }
        Ok(())
    }

    fn release(&self, amount: usize) {
        let Some(budget) = self.budget else {
            return;
        };
        let held = self.bytes.get();
        assert!(amount <= held, "a debit gives back only bytes it holds");
        self.bytes.set(held - amount);
        budget.bytes.set(budget.bytes.get() + amount);
    }
}

impl Drop for Debit<'_> {
    fn drop(&mut self) {
        self.release(self.bytes.get());
    }
}

fn bytes_of<T>(count: usize) -> DecodeResult<usize> {
    count
        .checked_mul(size_of::<T>())
        .ok_or(DecodeError::Limit(DecodeLimit::Bytes))
}

fn charge(remaining: &Cell<usize>, amount: usize, limit: DecodeLimit) -> DecodeResult<()> {
    remaining.set(
        remaining
            .get()
            .checked_sub(amount)
            .ok_or(DecodeError::Limit(limit))?,
    );
    Ok(())
}
