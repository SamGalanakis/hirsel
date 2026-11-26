//! NET-18 part 2: the host's own addresses, kept from the host's
//! address-change source, which is a snapshot of the assigned addresses and a
//! stream of changes. The table is filled when the first public-only connect is
//! issued. Before each answer the source is read until it would block, and a
//! lost change takes a new snapshot. The source is chosen at compile time:
//! netlink on Linux (`linux.rs`), `getifaddrs` and `PF_ROUTE` on macOS
//! (`darwin.rs`).
use std::collections::BTreeSet;
use std::sync::Mutex;

#[cfg(any(target_os = "macos", target_os = "ios"))]
mod darwin;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(any(target_os = "macos", target_os = "ios"))]
use darwin as source;
#[cfg(target_os = "linux")]
use linux as source;

/// One assigned address: (family, 0 for IPv4 and 1 for IPv6; its bits;
/// interface index; prefix length). An address held on two interfaces, or twice
/// with two prefix lengths, stays until every assignment is dropped.
type Key = (u8, u128, u32, u8);

struct Table {
    changes: source::Changes,
    held: BTreeSet<Key>,
}

/// A connect issued while the table is filled waits on this lock.
static TABLE: Mutex<Option<Table>> = Mutex::new(None);

/// Whether the address `ip` of `family` (0 for IPv4, its bits in the low 32;
/// 1 for IPv6), or an IPv4 address embedded in it by NET-18's table, is
/// assigned to the host. An error is the errno that kept the table from being
/// read, and the caller refuses the connect.
pub fn holds(family: u8, ip: u128) -> Result<bool, i32> {
    let mut table = TABLE.lock().unwrap_or_else(|e| e.into_inner());
    let table = match &mut *table {
        Some(table) => {
            table.settle()?;
            table
        }
        empty => {
            // Subscribe first, so a change made during the snapshot is queued.
            let changes = source::subscribe()?;
            let mut table = Table {
                changes,
                held: source::snapshot()?,
            };
            table.settle()?;
            empty.insert(table)
        }
    };
    let held = |family: u8, ip: u128| {
        table
            .held
            .range((family, ip, 0, 0)..=(family, ip, u32::MAX, u8::MAX))
            .next()
            .is_some()
    };
    Ok(if family == 0 {
        held(0, ip)
    } else {
        held(1, ip) || crate::class::embedded_v4(ip).any(|v4| held(0, u128::from(v4)))
    })
}

impl Table {
    /// Read every queued change, taking a new snapshot until a drain loses
    /// nothing.
    fn settle(&mut self) -> Result<(), i32> {
        while self.changes.drain(&mut self.held)? {
            self.held = source::snapshot()?;
        }
        Ok(())
    }
}

fn errno() -> i32 {
    std::io::Error::last_os_error()
        .raw_os_error()
        .unwrap_or(libc::EIO)
}
