//! NET-18 part 2: the host's own addresses, kept from the host's
//! address-change source, which is a snapshot of the assigned addresses and a
//! stream of changes. The table is filled when the first public-only connect is
//! issued. Before each answer the source is read until it would block, and a
//! lost change takes a new snapshot. The source is chosen at compile time:
//! netlink on Linux (`linux.rs`), `getifaddrs` and `PF_ROUTE` on macOS
//! (`darwin.rs`). iOS reads a fresh snapshot for each connect.
use std::collections::BTreeSet;
#[cfg(not(target_os = "ios"))]
use std::sync::Mutex;

#[cfg(target_os = "macos")]
mod darwin;
#[cfg(target_vendor = "apple")]
mod ifaddrs;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
use darwin as source;
#[cfg(target_os = "linux")]
use linux as source;

/// One assigned address: (family, 0 for IPv4 and 1 for IPv6; its bits;
/// interface index; prefix length). An address held on two interfaces, or twice
/// with two prefix lengths, stays until every assignment is dropped.
type Key = (u8, u128, u32, u8);

#[cfg(not(target_os = "ios"))]
struct Table {
    changes: source::Changes,
    held: BTreeSet<Key>,
}

/// A connect issued while the table is filled waits on this lock.
#[cfg(not(target_os = "ios"))]
static TABLE: Mutex<Option<Table>> = Mutex::new(None);

/// Whether the address `ip` of `family` (0 for IPv4, its bits in the low 32;
/// 1 for IPv6), or an IPv4 address embedded in it by NET-18's table, is
/// assigned to the host. An error is the errno that kept the table from being
/// read, and the caller refuses the connect.
pub fn holds(family: u8, ip: u128) -> Result<bool, i32> {
    #[cfg(target_os = "ios")]
    return snapshot_holds(ifaddrs::snapshot, family, ip);

    #[cfg(not(target_os = "ios"))]
    changed_holds(family, ip)
}

#[cfg(any(test, target_os = "ios"))]
fn snapshot_holds(
    snapshot: impl FnOnce() -> Result<BTreeSet<Key>, i32>,
    family: u8,
    ip: u128,
) -> Result<bool, i32> {
    snapshot().map(|held| contains(&held, family, ip))
}

#[cfg(not(target_os = "ios"))]
fn changed_holds(family: u8, ip: u128) -> Result<bool, i32> {
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
    Ok(contains(&table.held, family, ip))
}

fn contains(held: &BTreeSet<Key>, family: u8, ip: u128) -> bool {
    let held = |family: u8, ip: u128| {
        held.range((family, ip, 0, 0)..=(family, ip, u32::MAX, u8::MAX))
            .next()
            .is_some()
    };
    if family == 0 {
        held(0, ip)
    } else {
        held(1, ip) || crate::class::embedded_v4(ip).any(|v4| held(0, u128::from(v4)))
    }
}

#[cfg(not(target_os = "ios"))]
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

#[cfg(test)]
#[test]
fn public_connect_refreshes_own_addresses_and_refuses_snapshot_failure() {
    // 8.8.8.8 is public. Its assignment changes between connect requests.
    let ip = 0x0808_0808;
    let mut snapshots = [
        Ok(BTreeSet::new()),
        Ok(BTreeSet::from([(0, ip, 1, 32)])),
        Err(libc::EIO),
    ]
    .into_iter();
    // The provider issues a public-only connect only for Ok(false).
    let mut connect = || snapshot_holds(|| snapshots.next().unwrap(), 0, ip);
    assert_eq!(connect(), Ok(false));
    assert_eq!(connect(), Ok(true));
    assert_eq!(connect(), Err(libc::EIO));
}
