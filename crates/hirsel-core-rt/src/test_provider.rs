//! Deterministic providers used by durable-library tests.
//!
//! The crash model keeps inode bytes and directory names as separate durable
//! domains. `sync` publishes an inode's bytes; `sync_dir` publishes names.
//! Clean restart discards pending changes. Damaged restart samples which
//! pending writes and directory operations survive the cut.

#[cfg(feature = "testing")]
pub mod crash {
    use std::collections::{BTreeMap, BTreeSet};

    #[derive(Clone, Debug, Eq, PartialEq)]
    pub enum Op<'a> {
        Create(&'a str),
        Write(&'a str, &'a [u8]),
        Append(&'a str, &'a [u8]),
        Sync(&'a str),
        Rename(&'a str, &'a str),
        Remove(&'a str),
        SyncDir,
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct Snapshot {
        pub names: BTreeMap<String, u64>,
        pub data: BTreeMap<u64, Vec<u8>>,
    }

    #[derive(Clone, Debug, Default)]
    struct NameChange {
        removed: Option<(String, u64)>,
        installed: Option<(String, Option<u64>, u64)>,
    }

    impl NameChange {
        fn persist(self, names: &mut BTreeMap<String, u64>) {
            // A rename is atomic and both ends must have the inode identities
            // seen when it was issued. Skipping a create cannot revive its inode.
            if self
                .removed
                .as_ref()
                .is_some_and(|(name, inode)| names.get(name) != Some(inode))
                || self
                    .installed
                    .as_ref()
                    .is_some_and(|(name, before, _)| names.get(name).copied() != *before)
            {
                return;
            }
            if let Some((name, _)) = self.removed {
                names.remove(&name);
            }
            if let Some((name, _, inode)) = self.installed {
                names.insert(name, inode);
            }
        }
    }

    struct Damage(u64);

    /// A damage seed with this bit set names one explicit subset of a Direct
    /// write's candidate sectors: bit i of the low 62 bits lands candidate i.
    /// Seeds without it keep their patterns (none, all, first, last, alternate).
    pub const SECTOR_SUBSET: u64 = 1 << 62;

    /// What a damaged restart kept of one file's pending bytes.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Image {
        Old,
        New,
        Partial,
    }

    impl Damage {
        fn next(&mut self) -> u64 {
            // SplitMix64, with wrapping arithmetic for reproducible cuts.
            self.0 = self.0.wrapping_add(0x9e3779b97f4a7c15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
            z ^ (z >> 31)
        }

        /// Returns the number of candidate sectors of a Direct file.
        fn file(&mut self, durable: &mut Vec<u8>, live: &[u8], direct: Option<u64>) -> usize {
            const SECTOR: usize = 512;
            if durable == live {
                return 0;
            }
            if let Some(mask) = direct {
                // An O_DIRECT write lands as any subset of its sectors, below
                // or past durable EOF; a hole before a landed sector reads as
                // zeros. Every sector that differs or lies past EOF is a
                // candidate; the mask picks none, all, the first, the last or
                // alternate candidates.
                let old_len = durable.len();
                let candidates: Vec<usize> = (0..live.len())
                    .step_by(SECTOR)
                    .filter(|&at| {
                        let stop = (at + SECTOR).min(live.len());
                        stop > old_len || durable[at..stop] != live[at..stop]
                    })
                    .collect();
                let last = candidates.len().saturating_sub(1);
                let mut image = durable.clone();
                let mut end = old_len;
                for (index, &at) in candidates.iter().enumerate() {
                    let lands = match mask {
                        _ if mask & SECTOR_SUBSET != 0 => index < 62 && mask >> index & 1 == 1,
                        0 => false,
                        1 => true,
                        2 => index == 0,
                        3 => index == last,
                        _ => index % 2 == 0,
                    };
                    if lands {
                        let stop = (at + SECTOR).min(live.len());
                        image.resize(image.len().max(stop), 0);
                        image[at..stop].copy_from_slice(&live[at..stop]);
                        end = end.max(stop);
                    }
                }
                image.truncate(end);
                *durable = image;
                return candidates.len();
            }
            let pending = &live[durable.len()..];
            let (written, zero_fill) = if pending.is_empty() {
                (0, false)
            } else {
                match self.next() % 4 {
                    0 => (0, false),
                    1 => ((self.next() % pending.len() as u64) as usize, false),
                    2 => ((self.next() % pending.len() as u64) as usize, true),
                    _ => (pending.len(), false),
                }
            };
            // Only a pending overwrite can change bytes below durable EOF.
            // Each intersecting sector takes its old or new bytes. If the
            // straddling sector has new suffix bytes, take its new prefix too.
            let durable_len = durable.len();
            for (old, new) in durable.chunks_mut(SECTOR).zip(live.chunks(SECTOR)) {
                let (new_prefix, new_suffix) = new.split_at(old.len());
                if old != new_prefix
                    && ((!new_suffix.is_empty() && written > 0) || self.next() & 1 == 0)
                {
                    old.copy_from_slice(new_prefix);
                }
            }
            durable.extend_from_slice(&pending[..written]);
            if zero_fill {
                durable.resize(durable_len + pending.len(), 0);
            }
            0
        }
    }

    #[derive(Clone, Debug, Default)]
    pub struct FileSystem {
        next_inode: u64,
        live_names: BTreeMap<String, u64>,
        durable_names: BTreeMap<String, u64>,
        live_data: BTreeMap<u64, Vec<u8>>,
        durable_data: BTreeMap<u64, Vec<u8>>,
        pending_names: Vec<NameChange>,
        pending_truncations: BTreeSet<u64>,
        direct: BTreeSet<u64>,
    }

    impl FileSystem {
        /// Provider issue for a model operation. Delivery is immediate because
        /// the model has no asynchronous host state.
        pub fn issue(&mut self, op: &Op<'_>) {
            self.apply(op);
        }

        pub fn apply(&mut self, op: &Op<'_>) {
            match op {
                Op::Create(name) => {
                    assert!(!self.live_names.contains_key(*name));
                    let inode = self.next_inode;
                    self.next_inode += 1;
                    self.live_names.insert((*name).into(), inode);
                    self.live_data.insert(inode, Vec::new());
                    self.pending_names.push(NameChange {
                        installed: Some(((*name).into(), None, inode)),
                        ..NameChange::default()
                    });
                }
                Op::Write(name, bytes) => {
                    let inode = self.live_names[*name];
                    self.write_inode(inode, bytes.to_vec());
                }
                Op::Append(name, bytes) => {
                    let inode = self.live_names[*name];
                    self.live_data
                        .get_mut(&inode)
                        .unwrap()
                        .extend_from_slice(bytes);
                }
                Op::Sync(name) => {
                    let inode = self.live_names[*name];
                    self.sync_inode(inode);
                }
                Op::Rename(from, to) => {
                    if from == to {
                        assert!(self.live_names.contains_key(*from));
                        return;
                    }
                    let inode = self.live_names.remove(*from).unwrap();
                    let replaced = self.live_names.insert((*to).into(), inode);
                    self.pending_names.push(NameChange {
                        removed: Some(((*from).into(), inode)),
                        installed: Some(((*to).into(), replaced, inode)),
                    });
                }
                Op::Remove(name) => {
                    let inode = self.live_names.remove(*name).unwrap();
                    self.pending_names.push(NameChange {
                        removed: Some(((*name).into(), inode)),
                        ..NameChange::default()
                    });
                }
                Op::SyncDir => {
                    self.durable_names = self.live_names.clone();
                    self.pending_names.clear();
                }
            }
        }

        /// Persist one directory entry before a crash, leaving unrelated
        /// directory operations volatile.
        pub fn persist_name(&mut self, name: &str) {
            self.durable_names
                .insert(name.to_owned(), self.live_names[name]);
        }

        /// Persist one removal before a crash, leaving other names untouched.
        pub fn persist_removal(&mut self, name: &str) {
            assert!(!self.live_names.contains_key(name));
            assert!(self.durable_names.remove(name).is_some());
        }

        pub fn restart(&mut self) {
            self.live_names = self.durable_names.clone();
            self.live_data = self.durable_data.clone();
            // A file whose name is durable and whose bytes never were is empty.
            for &inode in self.durable_names.values() {
                self.live_data.entry(inode).or_default();
            }
            self.pending_names.clear();
            self.pending_truncations.clear();
        }

        /// Marks an inode as opened O_DIRECT, so a damaged restart keeps a
        /// sector subset of its pending bytes instead of a prefix.
        pub fn mark_direct(&mut self, inode: u64) {
            self.direct.insert(inode);
        }

        pub fn is_direct(&self, name: &str) -> bool {
            self.inode(name)
                .is_some_and(|inode| self.direct.contains(&inode))
        }

        /// Returns, for each inode whose pending bytes were damaged, whether
        /// its image kept the old bytes, took the new ones, or a mix, and for
        /// a Direct inode its candidate sector count.
        pub fn restart_damaged(&mut self, cut: u64, seed: u64) -> Vec<(Image, Option<usize>)> {
            let mut damage = Damage(seed ^ cut.wrapping_mul(0xd1342543de82ef95));
            let mut images = Vec::new();
            for change in std::mem::take(&mut self.pending_names) {
                if damage.next() & 1 == 0 {
                    change.persist(&mut self.durable_names);
                }
            }
            for &inode in self.durable_names.values() {
                let durable = self.durable_data.entry(inode).or_default();
                // Keep the pre-truncate image if a shrink crossed durable EOF.
                // Later appends to the truncated file are a different extent.
                if !self.pending_truncations.contains(&inode)
                    && let Some(live) = self.live_data.get(&inode)
                    && durable != live
                {
                    let old = durable.clone();
                    let mask =
                        self.direct
                            .contains(&inode)
                            .then_some(if seed & SECTOR_SUBSET != 0 {
                                seed
                            } else {
                                seed % 5
                            });
                    let candidates = damage.file(durable, live, mask);
                    let image = if *durable == old {
                        Image::Old
                    } else if durable == live {
                        Image::New
                    } else {
                        Image::Partial
                    };
                    images.push((image, mask.map(|_| candidates)));
                }
            }
            self.restart();
            images
        }

        pub fn read(&self, name: &str) -> Option<&[u8]> {
            self.read_inode(self.inode(name)?)
        }

        pub fn read_inode(&self, inode: u64) -> Option<&[u8]> {
            self.live_data.get(&inode).map(Vec::as_slice)
        }

        pub fn durable_inode(&self, inode: u64) -> &[u8] {
            self.durable_data.get(&inode).map_or(&[], Vec::as_slice)
        }

        pub fn write_inode(&mut self, inode: u64, bytes: Vec<u8>) {
            if self
                .durable_data
                .get(&inode)
                .is_some_and(|old| bytes.len() < old.len())
            {
                self.pending_truncations.insert(inode);
            }
            self.live_data.insert(inode, bytes);
        }

        pub fn sync_inode(&mut self, inode: u64) {
            self.sync_image(inode, self.live_data[&inode].clone());
        }

        pub fn sync_image(&mut self, inode: u64, bytes: Vec<u8>) {
            self.durable_data.insert(inode, bytes);
            self.pending_truncations.remove(&inode);
        }

        pub fn inode(&self, name: &str) -> Option<u64> {
            self.live_names.get(name).copied()
        }

        pub fn snapshot(&self) -> Snapshot {
            Snapshot {
                names: self.live_names.clone(),
                data: self.live_data.clone(),
            }
        }

        /// Every boundary, including before the first operation and after the
        /// last, is a valid deterministic crash cut.
        pub fn cuts(operation_count: usize) -> std::ops::RangeInclusive<usize> {
            0..=operation_count
        }
    }

    pub type CrashProvider = FileSystem;

    pub fn run_until_restart(ops: &[Op<'_>], cut: usize) -> FileSystem {
        assert!(cut <= ops.len());
        let mut fs = FileSystem::default();
        for op in &ops[..cut] {
            fs.apply(op);
        }
        fs.restart();
        fs
    }
}

#[cfg(feature = "testing")]
pub mod lock_spool {
    use std::collections::{BTreeMap, VecDeque};

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum LockResult {
        Acquired,
        Held,
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct Message {
        pub turn: u64,
        pub run: u64,
        pub position: u64,
        pub bytes: Vec<u8>,
    }

    #[derive(Default)]
    pub struct Provider {
        locks: BTreeMap<String, u64>,
        spool: BTreeMap<String, VecDeque<Message>>,
        next_turn: u64,
        last_position: BTreeMap<(u64, String), u64>,
    }

    impl Provider {
        pub fn try_lock(&mut self, run: u64, name: &str) -> LockResult {
            match self.locks.get(name) {
                Some(&owner) if owner != run => LockResult::Held,
                _ => {
                    self.locks.insert(name.into(), run);
                    LockResult::Acquired
                }
            }
        }

        pub fn unlock(&mut self, run: u64, name: &str) {
            assert_eq!(self.locks.get(name), Some(&run));
            self.locks.remove(name);
        }

        pub fn append(&mut self, run: u64, stream: &str, position: u64, bytes: &[u8]) {
            let key = (run, stream.to_owned());
            let expected = self.last_position.get(&key).map_or(0, |p| p + 1);
            assert_eq!(position, expected, "spool positions must be consecutive");
            self.last_position.insert(key, position);
            let turn = self.next_turn;
            self.spool
                .entry(stream.into())
                .or_default()
                .push_back(Message {
                    turn,
                    run,
                    position,
                    bytes: bytes.to_vec(),
                });
            self.next_turn += 1;
        }

        pub fn receive(&mut self, stream: &str) -> Option<Message> {
            self.spool.get_mut(stream)?.pop_front()
        }

        pub fn pending(&self, stream: &str) -> usize {
            self.spool.get(stream).map_or(0, VecDeque::len)
        }
    }
}

#[cfg(all(test, feature = "testing"))]
mod tests {
    use super::crash::{FileSystem, Op, run_until_restart};

    #[test]
    fn every_cut_loses_only_unsynced_bytes_and_names() {
        let ops = [
            Op::Create("a"),
            Op::Write("a", b"x"),
            Op::Sync("a"),
            Op::Rename("a", "b"),
            Op::SyncDir,
        ];
        let mut seen = 0;
        for cut in FileSystem::cuts(ops.len()) {
            let fs = run_until_restart(&ops, cut);
            match cut {
                0..=3 => assert_eq!(fs.read("a"), None),
                4 => assert_eq!(fs.read("a"), None),
                5 => assert_eq!(fs.read("b"), Some(&b"x"[..])),
                _ => unreachable!(),
            }
            seen += 1;
        }
        assert_eq!(seen, ops.len() + 1);
    }

    #[test]
    fn a_second_run_is_deterministic_and_does_not_reuse_inodes() {
        let ops = [
            Op::Create("a"),
            Op::Write("a", b"x"),
            Op::Sync("a"),
            Op::SyncDir,
        ];
        let first = run_until_restart(&ops, ops.len()).snapshot();
        let second = run_until_restart(&ops, ops.len()).snapshot();
        assert_eq!(first, second);
        assert_eq!(first.data.len(), 1);
    }

    #[test]
    fn append_survives_only_after_file_sync() {
        let ops = [
            Op::Create("a"),
            Op::Write("a", b"x"),
            Op::Sync("a"),
            Op::SyncDir,
            Op::Append("a", b"y"),
            Op::Sync("a"),
        ];
        assert_eq!(run_until_restart(&ops, 5).read("a"), Some(&b"x"[..]));
        assert_eq!(run_until_restart(&ops, 6).read("a"), Some(&b"xy"[..]));
    }

    #[test]
    fn rename_overwrite_and_remove_survive_only_after_directory_sync() {
        let rename = [
            Op::Create("a"),
            Op::Write("a", b"a"),
            Op::Sync("a"),
            Op::Create("b"),
            Op::Write("b", b"b"),
            Op::Sync("b"),
            Op::SyncDir,
            Op::Rename("a", "b"),
            Op::SyncDir,
        ];
        assert_eq!(run_until_restart(&rename, 8).read("b"), Some(&b"b"[..]));
        assert_eq!(run_until_restart(&rename, 9).read("b"), Some(&b"a"[..]));

        let remove = [
            Op::Create("a"),
            Op::Write("a", b"a"),
            Op::Sync("a"),
            Op::SyncDir,
            Op::Remove("a"),
            Op::SyncDir,
        ];
        assert_eq!(run_until_restart(&remove, 5).read("a"), Some(&b"a"[..]));
        assert_eq!(run_until_restart(&remove, 6).read("a"), None);
    }
}

#[cfg(all(test, feature = "testing"))]
mod lock_spool_tests {
    use super::lock_spool::{LockResult, Provider};

    #[test]
    fn two_runs_share_one_lock_and_one_deterministic_spool() {
        let mut provider = Provider::default();
        assert_eq!(provider.try_lock(1, "store"), LockResult::Acquired);
        assert_eq!(provider.try_lock(2, "store"), LockResult::Held);
        provider.unlock(1, "store");
        assert_eq!(provider.try_lock(2, "store"), LockResult::Acquired);

        provider.append(1, "reply", 0, b"one");
        provider.append(2, "reply", 0, b"two");
        assert_eq!(provider.pending("reply"), 2);
        assert_eq!(provider.receive("reply").unwrap().bytes, b"one");
        assert_eq!(provider.receive("reply").unwrap().bytes, b"two");
        assert_eq!(provider.receive("reply"), None);
    }

    #[test]
    #[should_panic(expected = "spool positions must be consecutive")]
    fn spool_rejects_a_position_gap() {
        let mut provider = Provider::default();
        provider.append(1, "reply", 1, b"gap");
    }
}
