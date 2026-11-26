//! Environment configuration for the untied unit scheduler.

/// `HIRSEL_UNTIED=W`: run the root unit, and every helper unit it makes, on W
/// worker threads. Only executables that can spawn a helper unit read it.
/// An invalid or zero worker count exits with status 2.
pub(crate) fn workers() -> Option<usize> {
    const NAME: &str = "HIRSEL_UNTIED";
    let value = std::env::var(NAME);
    if matches!(value, Err(std::env::VarError::NotPresent)) {
        return None;
    }
    let workers = value
        .ok()
        .and_then(|value| value.parse::<std::num::NonZeroUsize>().ok())
        .unwrap_or_else(|| invalid_env(NAME, "a positive count of workers"));
    Some(workers.get())
}

pub(super) fn idle_spin_ns() -> u64 {
    const NAME: &str = "HIRSEL_UNTIED_SPIN_US";
    let value = std::env::var(NAME);
    if matches!(value, Err(std::env::VarError::NotPresent)) {
        return 5_000;
    }
    value
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .and_then(|micros| micros.checked_mul(1_000))
        .unwrap_or_else(|| {
            invalid_env(
                NAME,
                "a duration in microseconds that fits in u64 nanoseconds",
            )
        })
}

#[cold]
fn invalid_env(name: &str, expected: &str) -> ! {
    eprintln!("hirsel: {name} must be {expected}");
    std::process::exit(2);
}
pub(super) fn lifo_on() -> bool {
    std::env::var("HIRSEL_UNTIED_LIFO")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(1)
        != 0
}
