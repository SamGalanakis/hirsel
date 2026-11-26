//! A segment sync, and the named sync failures it can carry. A direct
//! (O_DSYNC) segment write is its write and this sync, so the failures fall
//! on the write.
use super::window::WINDOW_SYNC_FAILURE;
use super::{
    Bridge, ORPHAN_END_SYNC_FAILURE, Op, Outcome, SECOND_SEGMENT_SHORT_WRITE,
    SECOND_SEGMENT_SYNC_FAILURE, WindowObservation, fork_frame_start, is_segment,
};

pub(super) const ENTRIES: u8 = 44;
pub(super) const NEXT_ENTRIES: u8 = 45;
pub(super) const CLOSE_ENTRIES: u8 = 48;

impl Bridge {
    /// Counts one sync for journal-sync-count, explicit or carried by a
    /// Durable or Direct write (§3.3b).
    pub(super) fn count_sync(&mut self, name: &str) {
        if self.phase == 1 && self.armed {
            let slot = if name.ends_with(".end") {
                1
            } else if is_segment(name) {
                2
            } else {
                3
            };
            self.armed_io[slot] += 1;
        }
    }

    pub(super) fn sync_file(&mut self, handle: u64) -> Outcome {
        let file = &self.files[&handle];
        let name = file.name.clone();
        let inode = file.inode;
        let reader = file.mode == 0;
        self.count_sync(&name);
        if is_segment(&name) && reader {
            self.reader_syncs[0] += 1;
        }
        if self.sync_late_write(inode) {
            return Outcome::default();
        }
        if self.phase == 1
            && self.armed
            && self.sync_failure == SECOND_SEGMENT_SHORT_WRITE
            && is_segment(&name)
        {
            assert_eq!(self.failed_syncs, 0, "store synced after a short write");
            self.segment_syncs += 1;
        }
        let window_sync = self.phase == 1 && is_segment(&name);
        let matches = match self.sync_failure {
            WINDOW_SYNC_FAILURE if window_sync => self
                .window
                .as_ref()
                .is_some_and(WindowObservation::syncs_zeros),
            1 => name.starts_with("j-") && !name.ends_with(".end"),
            2 => name.starts_with("j-") && name.ends_with(".end"),
            3 => name.starts_with("p-"),
            // The runner's poison run: one frame per group, so the
            // second armed segment sync carries the call's effect frame.
            SECOND_SEGMENT_SYNC_FAILURE
                if self.phase == 1 && self.armed && is_segment(&name) && !self.crashed =>
            {
                self.segment_syncs += 1;
                self.segment_syncs == 2
            }
            5 if self.phase == 1 && self.armed && is_segment(&name) && !self.crashed => {
                self.split_syncs += 1;
                self.split_syncs == 2
            }
            _ => false,
        };
        if self.phase == 1 && (self.armed || self.sync_failure == WINDOW_SYNC_FAILURE) && matches {
            self.failed_syncs += 1;
            if self.sync_failure == 5 {
                // Only the first seven bytes of B reached storage.
                // A's successful sync is the only durable commit.
                let torn = if self.files[&handle].mode == super::DIRECT {
                    // Direct: the sector holding B's start lands, nothing else.
                    let sector = self.split_at / 512 * 512;
                    let new = self.fs.read(&name).unwrap();
                    let mut torn = self.fs.durable_inode(self.files[&handle].inode).to_vec();
                    torn.resize(torn.len().max(sector + 512), 0);
                    torn[sector..sector + 512].copy_from_slice(&new[sector..sector + 512]);
                    torn
                } else {
                    self.fs.read(&name).unwrap()[..self.split_at + 7].to_vec()
                };
                self.fs.issue(&Op::Write(&name, &torn));
                self.fs.issue(&Op::Sync(&name));
            }
            // A retry on this live Db must never succeed.
            assert_eq!(self.failed_syncs, 1, "store retried a failed sync");
            return Self::error(5);
        }
        if !self.exists(&name) {
            // journal-ack holds a sync across other tasks' turns, so a
            // cut may remove the segment's end file first. A sync of an
            // unlinked file reaches no live name. A restart that revives
            // the name sees its last synced bytes, which a crash just
            // before this sync gives anyway, so this hides no state.
            return if self.crashed {
                Self::error(5)
            } else {
                Outcome::default()
            };
        }
        let first_crash = self.orphan_end
            && self.phase == 1
            && self.files[&handle].mode == super::DURABLE
            && name.starts_with("j-")
            && name.ends_with(".end");
        let fork_start = (self.phase == 1 && is_segment(&name))
            .then(|| self.fork_observation.as_ref().filter(|fork| !fork.synced))
            .flatten()
            .and_then(|_| fork_frame_start(self.fs.read(&name).unwrap()));
        if let Some(start) = fork_start {
            let fork = self.fork_observation.as_ref().unwrap();
            if (2..=4).contains(&fork.image) {
                // The write was issued, but the Store's sync did not
                // succeed. W2 may retain a complete or torn write.
                if fork.image == 3 {
                    let torn = self.fs.read(&name).unwrap()[..start + 7].to_vec();
                    self.fs.issue(&Op::Write(&name, &torn));
                }
                self.fs.issue(&Op::Sync(&name));
                if fork.image == 4 {
                    let end = format!("{name}.end");
                    self.fs.issue(&Op::Write(&end, b"bad END"));
                    self.fs.issue(&Op::Sync(&end));
                }
                self.crashed = true;
                return Self::error(5);
            }
        }
        if self.apply(Op::Sync(&name)) {
            if window_sync && let Some(window) = self.window.as_mut() {
                window.synced();
            }
            if fork_start.is_some() {
                let fork = self.fork_observation.as_mut().unwrap();
                fork.synced = true;
                self.floor = 3;
                if fork.image == 1 {
                    self.crashed = true;
                    return Self::error(5);
                }
            }
            if first_crash {
                // Both inode syncs have finished. Only the END name
                // survives the first crash, before the directory sync.
                self.fs.persist_name(&name);
                if self.sync_failure == ORPHAN_END_SYNC_FAILURE {
                    self.orphan_end_path = Some(name);
                }
                self.crashed = true;
            }
            Outcome::default()
        } else {
            Self::error(5)
        }
    }

    pub(super) fn close_entries(&mut self, handle: u64) -> Outcome {
        self.listings.remove(&handle);
        Outcome::default()
    }

    pub(super) fn assert_no_leaked_handles(&self) {
        assert!(self.files.is_empty(), "run leaked provider file handles");
        assert!(
            self.listings.is_empty(),
            "run leaked provider listing handles"
        );
    }

    pub(super) fn open_entries(&mut self, path: &[u8]) -> Outcome {
        let prefix = std::str::from_utf8(path).expect("Hirsel String path");
        let names: Vec<String> = self
            .fs
            .snapshot()
            .names
            .into_keys()
            .filter_map(|entry| {
                let rest = if prefix.is_empty() {
                    entry.as_str()
                } else {
                    entry.strip_prefix(prefix)?
                };
                (!rest.is_empty() && !rest.contains('/')).then(|| rest.to_owned())
            })
            .collect();
        let id = self.next_file;
        self.next_file += 1;
        self.listings.insert(id, (names, 0));
        Outcome {
            value: id,
            ..Outcome::default()
        }
    }

    pub(super) fn next_entries(&mut self, handle: u64, data: &mut [u8], len: usize) -> Outcome {
        let Some((names, cursor)) = self.listings.get_mut(&handle) else {
            return Self::error(5);
        };
        let mut at = len;
        let mut count = 0u64;
        while *cursor < names.len() {
            let bytes = names[*cursor].as_bytes();
            let need = 3 + bytes.len();
            if data.len() - at < need {
                break;
            }
            data[at] = 0; // File
            data[at + 1..at + 3].copy_from_slice(&(bytes.len() as u16).to_le_bytes());
            data[at + 3..at + need].copy_from_slice(bytes);
            at += need;
            *cursor += 1;
            count += 1;
        }
        Outcome {
            value: count,
            written: (at - len) as u64,
            ..Outcome::default()
        }
    }
}
