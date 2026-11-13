//! Opaque destination identities (FORM v1b, MEM-4). Process-wide uniqueness also
//! prevents reuse after an earlier run or region has released its storage.

use std::sync::atomic::{AtomicU64, Ordering};

static DESTINATIONS: AtomicU64 = AtomicU64::new(0);

fn take(counter: &AtomicU64) -> u64 {
    counter
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
        .map(|previous| previous + 1)
        .unwrap_or_else(|_| crate::process::stop(b"DestinationIds"))
}

pub(crate) fn next() -> u64 {
    take(&DESTINATIONS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_u64_identity_is_representable_without_wrapping() {
        let counter = AtomicU64::new(u64::MAX - 1);
        assert_eq!(take(&counter), u64::MAX);
        assert_eq!(counter.load(Ordering::Relaxed), u64::MAX);
    }

    #[test]
    #[ignore = "isolated run stop, invoked by the named parent"]
    fn destination_overflow_child() {
        let counter = AtomicU64::new(u64::MAX);
        std::hint::black_box(take(&counter));
        panic!("destination overflow returned");
    }

    #[test]
    fn destination_overflow_is_a_named_run_stop() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "destination::tests::destination_overflow_child",
                "--test-threads=1",
            ])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(102));
        assert_eq!(output.stderr, b"run stop: DestinationIds\n");
    }
}
