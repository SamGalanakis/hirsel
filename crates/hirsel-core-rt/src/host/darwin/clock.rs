//! Hirsel's clocks on Darwin. The monotonic clock is `CLOCK_UPTIME_RAW`,
//! which, like Linux's `CLOCK_MONOTONIC`, stops while the machine sleeps; the
//! boot clock is `mach_continuous_time`, which does not; the wall clock is
//! `CLOCK_REALTIME`.
use std::{io, sync::OnceLock};

unsafe extern "C" {
    fn mach_continuous_time() -> u64;
}

fn read(clock: libc::clockid_t) -> Option<libc::timespec> {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: a valid writable timespec for a host clock.
    (unsafe { libc::clock_gettime(clock, &mut now) } == 0).then_some(now)
}

/// Nanoseconds on the monotonic clock.
#[inline(always)]
pub(crate) fn monotonic_ns() -> io::Result<u64> {
    let now = read(libc::CLOCK_UPTIME_RAW).ok_or_else(io::Error::last_os_error)?;
    let overflow = || io::Error::from_raw_os_error(libc::EOVERFLOW);
    let seconds = u64::try_from(now.tv_sec).map_err(|_| overflow())?;
    let nanos = u64::try_from(now.tv_nsec).map_err(|_| overflow())?;
    seconds
        .checked_mul(1_000_000_000)
        .and_then(|value| value.checked_add(nanos))
        .ok_or_else(overflow)
}

/// Milliseconds since the Unix epoch on the wall clock.
#[inline(always)]
pub(crate) fn wall_ms() -> Option<u64> {
    let now = read(libc::CLOCK_REALTIME)?;
    u64::try_from(i128::from(now.tv_sec) * 1000 + i128::from(now.tv_nsec) / 1_000_000).ok()
}

/// Nanoseconds on the suspend-inclusive boot clock.
#[inline(always)]
pub(crate) fn boot_ns() -> io::Result<u64> {
    static TIMEBASE: OnceLock<libc::mach_timebase_info> = OnceLock::new();
    let base = TIMEBASE.get_or_init(|| {
        let mut info = libc::mach_timebase_info { numer: 0, denom: 0 };
        // SAFETY: `info` is a valid writable timebase structure.
        unsafe { libc::mach_timebase_info(&mut info) };
        info
    });
    if base.denom == 0 {
        return Err(io::Error::from_raw_os_error(libc::EIO));
    }
    // SAFETY: mach_continuous_time has no preconditions.
    let ticks = unsafe { mach_continuous_time() };
    let ns = u128::from(ticks) * u128::from(base.numer) / u128::from(base.denom);
    u64::try_from(ns).map_err(|_| io::Error::from_raw_os_error(libc::EOVERFLOW))
}
