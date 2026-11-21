//! Limits shared across the frames of one code-object decode.

use std::{fmt, mem::size_of, time::Instant};

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

/// A decoder charges cumulative allocations and table entries against the
/// same attempt budget, including temporary allocations.
pub struct DecodeBudget {
    remaining: DecodeLimits,
}

impl DecodeBudget {
    pub fn new(limits: DecodeLimits) -> Self {
        Self { remaining: limits }
    }

    pub fn unlimited() -> Self {
        Self::new(DecodeLimits {
            bytes: usize::MAX,
            members: usize::MAX,
            slots: usize::MAX,
            depth: usize::MAX,
            deadline: None,
        })
    }

    pub fn check(&self) -> DecodeResult<()> {
        if self
            .remaining
            .deadline
            .is_some_and(|end| Instant::now() >= end)
        {
            return Err(DecodeError::Limit(DecodeLimit::Deadline));
        }
        Ok(())
    }

    pub fn check_depth(&self, depth: usize) -> DecodeResult<()> {
        self.check()?;
        if depth > self.remaining.depth {
            return Err(DecodeError::Limit(DecodeLimit::Depth));
        }
        Ok(())
    }

    pub fn charge_bytes(&mut self, amount: usize) -> DecodeResult<()> {
        self.check()?;
        charge(&mut self.remaining.bytes, amount, DecodeLimit::Bytes)
    }

    pub fn charge_members(&mut self, amount: usize) -> DecodeResult<()> {
        self.check()?;
        charge(&mut self.remaining.members, amount, DecodeLimit::Members)
    }

    pub fn charge_slots(&mut self, amount: usize) -> DecodeResult<()> {
        self.check()?;
        charge(&mut self.remaining.slots, amount, DecodeLimit::Slots)
    }

    pub(crate) fn allocation<T>(&mut self, count: usize) -> DecodeResult<()> {
        let bytes = count
            .checked_mul(size_of::<T>())
            .ok_or(DecodeError::Limit(DecodeLimit::Bytes))?;
        self.charge_bytes(bytes)
    }

    pub(crate) fn reserve<T>(&mut self, vec: &mut Vec<T>, additional: usize) -> DecodeResult<()> {
        self.check()?;
        if additional > vec.capacity() - vec.len() {
            let count = vec
                .len()
                .checked_add(additional)
                .ok_or(DecodeError::Limit(DecodeLimit::Bytes))?;
            let capacity = count.max(vec.capacity().saturating_mul(2));
            self.allocation::<T>(capacity)?;
            vec.try_reserve_exact(capacity - vec.len())
                .map_err(|_| DecodeError::Limit(DecodeLimit::Bytes))?;
        }
        Ok(())
    }
}

fn charge(remaining: &mut usize, amount: usize, limit: DecodeLimit) -> DecodeResult<()> {
    *remaining = remaining
        .checked_sub(amount)
        .ok_or(DecodeError::Limit(limit))?;
    Ok(())
}
