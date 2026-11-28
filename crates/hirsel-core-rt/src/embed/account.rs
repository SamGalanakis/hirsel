//! Per-generation host-client byte accounting (EMB-4).
//!
//! Partitions `config.client_bytes` into a fixed [`PROGRESS_RESERVE`] (one
//! maximum inbound frame plus control/terminal callback headroom) and the
//! admissible pool. Producer admissions reserve from the admissible pool before
//! registering or sending; C allocator failures map to [`AccountError::HostMemory`]
//! (`HIRSEL_HOST_MEMORY`) rather than aborting or conflating with `HIRSEL_FULL`.
use std::{collections::TryReserveError, sync::Mutex};

/// Default wire `max_frame` (WIRE 2.3).
pub const DEFAULT_MAX_FRAME: u64 = 262_144;
/// Fixed headroom for WebSocket frame header (14B) + control/terminal callbacks
/// (`SubEnd.ClientCapacity`, `session_ended`) that must still make progress at
/// full admissible utilization.
pub const TERMINAL_RESERVE: u64 = 8_192;
/// Minimum `client_bytes` accepted by `hirsel_embed_v1_start`.
pub const PROGRESS_RESERVE: u64 = DEFAULT_MAX_FRAME + 14 + TERMINAL_RESERVE;

/// Fixed accounting charge for one session record + its terminal `session_ended` reserve.
pub const SESSION_BASE_BYTES: u64 = 512;
/// Fixed accounting charge for one live call observer / lane slot.
pub const CALL_BASE_BYTES: u64 = 128;
/// Fixed accounting charge for one live subscription or room record.
pub const SUB_BASE_BYTES: u64 = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccountError {
    /// Host-client storage bound (`client_bytes` admissible pool) would be exceeded.
    Full,
    /// Underlying C allocator refused the allocation.
    HostMemory,
}

#[derive(Debug)]
struct Inner {
    total: u64,
    progress_reserve: u64,
    admissible_limit: u64,
    admissible_used: u64,
}

/// Shared byte account for one embedded runtime generation.
pub(crate) struct ClientAccount {
    inner: Mutex<Inner>,
}

impl ClientAccount {
    /// Construct a new account for `client_bytes`, or `Err("ClientBytesTooSmall")`
    /// if below [`PROGRESS_RESERVE`].
    pub(crate) fn new(client_bytes: u64) -> Result<Self, &'static str> {
        Self::with_progress_reserve(client_bytes, PROGRESS_RESERVE)
    }

    pub(crate) fn with_progress_reserve(
        client_bytes: u64,
        progress_reserve: u64,
    ) -> Result<Self, &'static str> {
        let Some(admissible_limit) = client_bytes.checked_sub(progress_reserve) else {
            return Err("ClientBytesTooSmall");
        };
        Ok(Self {
            inner: Mutex::new(Inner {
                total: client_bytes,
                progress_reserve,
                admissible_limit,
                admissible_used: 0,
            }),
        })
    }

    pub(crate) fn total(&self) -> u64 {
        self.inner.lock().unwrap().total
    }

    pub(crate) fn progress_reserve(&self) -> u64 {
        self.inner.lock().unwrap().progress_reserve
    }

    #[cfg(test)]
    pub(crate) fn admissible_limit(&self) -> u64 {
        self.inner.lock().unwrap().admissible_limit
    }

    pub(crate) fn admissible_used(&self) -> u64 {
        self.inner.lock().unwrap().admissible_used
    }

    /// Reserve `bytes` from the admissible pool. Returns `Err(AccountError::Full)`
    /// without mutating the balance if it would exceed `admissible_limit`.
    pub(crate) fn try_reserve(&self, bytes: u64) -> Result<u64, AccountError> {
        if bytes == 0 {
            return Ok(0);
        }
        let mut inner = self.inner.lock().unwrap();
        let Some(next) = inner.admissible_used.checked_add(bytes) else {
            return Err(AccountError::Full);
        };
        if next > inner.admissible_limit {
            return Err(AccountError::Full);
        }
        inner.admissible_used = next;
        Ok(bytes)
    }

    /// Refund `bytes` previously reserved from the admissible pool.
    pub(crate) fn refund(&self, bytes: u64) {
        if bytes == 0 {
            return;
        }
        let mut inner = self.inner.lock().unwrap();
        inner.admissible_used = inner.admissible_used.saturating_sub(bytes);
    }

    /// Copy `src` into a fallibly allocated `Vec<u8>` charged against the admissible pool.
    /// Reserves `overhead + src.len()` bytes before allocating; refunds on allocator refusal.
    pub(crate) fn try_copy_charged(
        &self,
        src: &[u8],
        overhead: u64,
    ) -> Result<(Vec<u8>, u64), AccountError> {
        let charge = overhead
            .checked_add(src.len() as u64)
            .ok_or(AccountError::Full)?;
        self.try_reserve(charge)?;
        match try_copy_slice(src) {
            Ok(vec) => Ok((vec, charge)),
            Err(err) => {
                self.refund(charge);
                Err(err)
            }
        }
    }
}

/// Fallibly allocate a `Vec<u8>` with exact capacity `len` via `try_reserve_exact`.
pub(crate) fn try_vec_with_capacity(len: usize) -> Result<Vec<u8>, AccountError> {
    let mut v = Vec::new();
    v.try_reserve_exact(len).map_err(map_reserve_err)?;
    Ok(v)
}

/// Fallibly copy `src` into a newly allocated `Vec<u8>`.
pub(crate) fn try_copy_slice(src: &[u8]) -> Result<Vec<u8>, AccountError> {
    if src.is_empty() {
        return Ok(Vec::new());
    }
    let mut v = try_vec_with_capacity(src.len())?;
    v.extend_from_slice(src);
    Ok(v)
}

fn map_reserve_err(_: TryReserveError) -> AccountError {
    AccountError::HostMemory
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_below_progress_reserve() {
        assert_eq!(
            ClientAccount::new(PROGRESS_RESERVE - 1).err(),
            Some("ClientBytesTooSmall")
        );
        let acc = ClientAccount::new(PROGRESS_RESERVE).unwrap();
        assert_eq!(acc.admissible_limit(), 0);
        assert_eq!(acc.try_reserve(1), Err(AccountError::Full));
    }

    #[test]
    fn exact_boundary_b_minus_1_vs_b() {
        let b = 1024u64;
        let acc_short = ClientAccount::new(PROGRESS_RESERVE + b - 1).unwrap();
        assert_eq!(acc_short.try_reserve(b), Err(AccountError::Full));
        assert_eq!(acc_short.admissible_used(), 0);

        let acc_exact = ClientAccount::new(PROGRESS_RESERVE + b).unwrap();
        assert_eq!(acc_exact.try_reserve(b), Ok(b));
        assert_eq!(acc_exact.admissible_used(), b);
        assert_eq!(acc_exact.try_reserve(1), Err(AccountError::Full));
        acc_exact.refund(b);
        assert_eq!(acc_exact.admissible_used(), 0);
        assert_eq!(acc_exact.try_reserve(b), Ok(b));
    }
}
