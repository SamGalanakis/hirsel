//! The provider's handle tables. Units on every slot thread issue on one
//! provider at once, so the tables sit behind one lock, held only for the
//! lookup or update and never across a syscall that can wait.
use super::{Listing, OsProvider};
use std::{
    collections::BTreeMap,
    os::fd::{AsRawFd, OwnedFd},
    sync::{MutexGuard, atomic::Ordering::Relaxed},
};

#[derive(Default)]
pub(super) struct Tables {
    pub(super) directories: BTreeMap<u64, OwnedFd>,
    pub(super) files: BTreeMap<u64, OwnedFd>,
    pub(super) listings: BTreeMap<u64, Listing>,
    pub(super) in_flight_syncs: Vec<*mut crate::os_sync::SyncCell>,
}

impl OsProvider {
    pub(super) fn id(&self) -> u64 {
        let id = self.next.fetch_add(1, Relaxed);
        if id == u64::MAX {
            crate::process::stop(b"HostHandles");
        }
        id
    }
    pub(super) fn tables(&self) -> MutexGuard<'_, Tables> {
        self.tables.lock().unwrap_or_else(|e| e.into_inner())
    }
    /// The descriptor behind a live directory handle. Directories stay open
    /// until the run ends, so the number outlives the lock.
    pub(super) fn directory(&self, handle: u64) -> i32 {
        self.tables().directories[&handle].as_raw_fd()
    }
    /// The descriptor behind a live file handle. The issuing unit owns the
    /// handle, so no other unit closes it while this operation runs.
    pub(super) fn file(&self, handle: u64) -> i32 {
        self.tables().files[&handle].as_raw_fd()
    }
    pub(super) fn insert_file(&self, fd: OwnedFd) -> u64 {
        let id = self.id();
        self.tables().files.insert(id, fd);
        id
    }
    pub(super) fn insert_directory(&self, fd: OwnedFd) -> u64 {
        let id = self.id();
        self.tables().directories.insert(id, fd);
        id
    }
}
