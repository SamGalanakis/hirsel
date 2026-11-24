//! A resident build can skip repeated name lookups while the kernel observes
//! every change to the local directories that contain its code objects:
//! inotify on Linux; on macOS, kqueue on each directory and on each cached
//! object, since kqueue cannot name the entry that changed in a directory.

use super::{CodeStore, ObjectKind};
use hirsel_core_ids::ContentHash;
use hirsel_core_ir::release_recipe::ObjectRef;
use rustc_hash::FxHashMap;
use std::fs::{self, File, Metadata};
use std::io::{self, Read};
#[cfg(target_os = "linux")]
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::MetadataExt;

pub(crate) struct Watch {
    #[cfg(target_os = "linux")]
    events: File,
    #[cfg(target_os = "macos")]
    events: std::os::fd::OwnedFd,
    /// The watched directories, open for as long as the watch.
    #[cfg(target_os = "macos")]
    #[allow(dead_code)]
    held: [std::os::fd::OwnedFd; 3],
    /// The directory and name each cached entry's descriptor watches.
    #[cfg(target_os = "macos")]
    watched: FxHashMap<i32, (usize, ContentHash)>,
    directories: [(i32, Metadata); 3],
    present: [FxHashMap<ContentHash, Checked>; 3],
}

struct Checked {
    len: u64,
    seen: bool,
    /// An event-only descriptor on the cached file; closing it ends its watch.
    #[cfg(target_os = "macos")]
    watch: std::os::fd::OwnedFd,
}

/// The cached entries macOS watches: one descriptor each.
#[cfg(target_os = "macos")]
const MAX_WATCHED: usize = 4096;

const KINDS: [ObjectKind; 3] = [ObjectKind::Body, ObjectKind::Annex, ObjectKind::Group];

fn directory(store: &CodeStore, kind: ObjectKind) -> std::path::PathBuf {
    store.root.join(kind.directory())
}

fn same_directory(a: &Metadata, b: &Metadata) -> bool {
    (a.dev(), a.ino(), a.mode(), a.uid(), a.gid()) == (b.dev(), b.ino(), b.mode(), b.uid(), b.gid())
}

impl Watch {
    #[cfg(target_os = "macos")]
    pub(super) fn open(store: &CodeStore) -> io::Result<Self> {
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
        // SAFETY: the kernel returns a new owned descriptor on success.
        let fd = unsafe { libc::kqueue() };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: fd is the new descriptor returned by kqueue.
        let events = unsafe { OwnedFd::from_raw_fd(fd) };
        let mut directories = Vec::new();
        let mut held = Vec::new();
        for kind in KINDS {
            let file = File::open(directory(store, kind))?;
            let metadata = file.metadata()?;
            // SAFETY: fstatfs writes into this valid statfs buffer.
            let mut info = std::mem::MaybeUninit::<libc::statfs>::uninit();
            if unsafe { libc::fstatfs(file.as_raw_fd(), info.as_mut_ptr()) } < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: successful fstatfs initialized info.
            let local = unsafe { info.assume_init() }.f_flags & libc::MNT_LOCAL as u32 != 0;
            if !metadata.is_dir() || !local {
                return Err(io::ErrorKind::Unsupported.into());
            }
            // The directory's own replacement or permission change loses the
            // watch; entries are watched one by one as they are cached.
            kqueue::watch(
                &events,
                file.as_raw_fd(),
                libc::NOTE_DELETE | libc::NOTE_RENAME | libc::NOTE_REVOKE | libc::NOTE_ATTRIB,
            )?;
            directories.push((file.as_raw_fd(), metadata));
            held.push(OwnedFd::from(file));
        }
        Ok(Self {
            events,
            held: held.try_into().unwrap(),
            watched: FxHashMap::default(),
            directories: directories.try_into().unwrap(),
            present: Default::default(),
        })
    }

    /// Consume every change the kernel has posted until now.
    #[cfg(target_os = "macos")]
    fn drain(&mut self, invalidated: &mut Vec<(ObjectKind, ObjectRef)>) -> bool {
        use std::os::fd::AsRawFd;
        loop {
            // SAFETY: an all-integer event buffer.
            let mut posted: [libc::kevent; 64] = unsafe { std::mem::zeroed() };
            let zero = libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            // SAFETY: the kqueue and the output buffer are live.
            let n = unsafe {
                libc::kevent(
                    self.events.as_raw_fd(),
                    std::ptr::null(),
                    0,
                    posted.as_mut_ptr(),
                    posted.len() as i32,
                    &zero,
                )
            };
            if n < 0 {
                if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return false;
            }
            for event in &posted[..n as usize] {
                let ident = event.ident as i32;
                let Some((directory, hash)) = self.watched.remove(&ident) else {
                    // A directory itself changed.
                    return false;
                };
                // Only the entry still holding this descriptor is stale.
                let present = &mut self.present[directory];
                if present
                    .get(&hash)
                    .is_some_and(|checked| checked.watch.as_raw_fd() == ident)
                    && let Some(Checked {
                        len, seen: true, ..
                    }) = present.remove(&hash)
                {
                    invalidated.push((KINDS[directory], ObjectRef { hash, len }));
                }
            }
            if (n as usize) < posted.len() {
                return true;
            }
        }
    }

    #[cfg(target_os = "linux")]
    pub(super) fn open(store: &CodeStore) -> io::Result<Self> {
        // SAFETY: the kernel returns a new owned descriptor on success.
        let fd = unsafe { libc::inotify_init1(libc::IN_CLOEXEC | libc::IN_NONBLOCK) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: fd is the new descriptor returned by inotify_init1.
        let events = unsafe { File::from_raw_fd(fd) };
        let mut directories = Vec::new();
        for kind in KINDS {
            let file = File::open(directory(store, kind))?;
            let metadata = file.metadata()?;
            // SAFETY: fstatfs writes into this valid statfs buffer.
            let mut info = std::mem::MaybeUninit::<libc::statfs>::uninit();
            if unsafe { libc::fstatfs(file.as_raw_fd(), info.as_mut_ptr()) } < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: successful fstatfs initialized info.
            let fs_type = unsafe { info.assume_init() }.f_type;
            if !metadata.is_dir() || !matches!(fs_type, libc::EXT4_SUPER_MAGIC | libc::TMPFS_MAGIC)
            {
                return Err(io::ErrorKind::Unsupported.into());
            }
            // Use the descriptor to attach to the exact inode checked above.
            let path =
                std::ffi::CString::new(format!("/proc/self/fd/{}", file.as_raw_fd())).unwrap();
            let mask = libc::IN_CREATE
                | libc::IN_DELETE
                | libc::IN_MOVED_FROM
                | libc::IN_MOVED_TO
                | libc::IN_ATTRIB
                | libc::IN_MODIFY
                | libc::IN_DELETE_SELF
                | libc::IN_MOVE_SELF
                | libc::IN_ONLYDIR;
            // SAFETY: both descriptors are live and path is NUL terminated.
            let watch = unsafe { libc::inotify_add_watch(fd, path.as_ptr(), mask) };
            if watch < 0 {
                return Err(io::Error::last_os_error());
            }
            if directories.iter().any(|(prior, _)| *prior == watch) {
                return Err(io::ErrorKind::Unsupported.into());
            }
            directories.push((watch, metadata));
        }
        Ok(Self {
            events,
            directories: directories.try_into().unwrap(),
            present: Default::default(),
        })
    }

    /// Recheck the path bindings and consume changes before using remembered
    /// names. Lose the watch on replacement, unmount, or queue overflow.
    fn refresh(
        &mut self,
        store: &CodeStore,
        invalidated: &mut Vec<(ObjectKind, ObjectRef)>,
    ) -> bool {
        if !self.directories.iter().zip(KINDS).all(|((_, meta), kind)| {
            fs::metadata(directory(store, kind)).is_ok_and(|live| same_directory(meta, &live))
        }) {
            return false;
        }
        self.drain(invalidated)
    }

    #[cfg(target_os = "linux")]
    fn drain(&mut self, invalidated: &mut Vec<(ObjectKind, ObjectRef)>) -> bool {
        let mut bytes = [0; 4096];
        loop {
            match self.events.read(&mut bytes) {
                Ok(n) if n > 0 => {
                    if !self.invalidate(&bytes[..n], invalidated) {
                        return false;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return true,
                _ => return false,
            }
        }
    }

    #[cfg(target_os = "linux")]
    fn invalidate(
        &mut self,
        mut events: &[u8],
        invalidated: &mut Vec<(ObjectKind, ObjectRef)>,
    ) -> bool {
        while !events.is_empty() {
            const HEADER: usize = std::mem::size_of::<libc::inotify_event>();
            // SAFETY: read_unaligned copies a complete event header from the
            // kernel's byte stream. Its fields are integers.
            let event = unsafe {
                events[..HEADER]
                    .as_ptr()
                    .cast::<libc::inotify_event>()
                    .read_unaligned()
            };
            let (name, rest) = events[HEADER..].split_at(event.len as usize);
            events = rest;
            if event.mask
                & (libc::IN_IGNORED
                    | libc::IN_Q_OVERFLOW
                    | libc::IN_DELETE_SELF
                    | libc::IN_MOVE_SELF
                    | libc::IN_UNMOUNT)
                != 0
            {
                return false;
            }
            let Some(directory) = self.directories.iter().position(|(wd, _)| *wd == event.wd)
            else {
                return false;
            };
            if name.is_empty() {
                // Permissions or other inode state changed on the directory.
                return false;
            }
            let name = name.split(|b| *b == 0).next().unwrap();
            if let Some(hash) = hash_name(name)
                && let Some(Checked {
                    len, seen: true, ..
                }) = self.present[directory].remove(&hash)
            {
                invalidated.push((KINDS[directory], ObjectRef { hash, len }));
            }
        }
        true
    }

    /// Watch the file just checked, if it is still the one at its name.
    #[cfg(target_os = "macos")]
    fn watch_entry(
        &mut self,
        store: &CodeStore,
        kind: ObjectKind,
        object: ObjectRef,
        metadata: &Metadata,
    ) -> Option<std::os::fd::OwnedFd> {
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
        use std::os::unix::ffi::OsStrExt;
        const O_EVTONLY: i32 = 0x8000;
        if self.watched.len() >= MAX_WATCHED {
            return None;
        }
        let path =
            std::ffi::CString::new(store.path(kind, object.hash).as_os_str().as_bytes()).ok()?;
        // SAFETY: path is NUL terminated; success returns a new descriptor.
        let fd = unsafe {
            libc::open(
                path.as_ptr(),
                O_EVTONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return None;
        }
        // SAFETY: fd is the new descriptor returned by open.
        let watch = unsafe { OwnedFd::from_raw_fd(fd) };
        kqueue::watch(
            &self.events,
            fd,
            libc::NOTE_DELETE
                | libc::NOTE_WRITE
                | libc::NOTE_EXTEND
                | libc::NOTE_ATTRIB
                | libc::NOTE_LINK
                | libc::NOTE_RENAME
                | libc::NOTE_REVOKE,
        )
        .ok()?;
        // The watch began after the check: the name must still hold that file.
        let live = File::from(watch.try_clone().ok()?).metadata().ok()?;
        if (
            live.dev(),
            live.ino(),
            live.mode(),
            live.nlink(),
            live.len(),
        ) != (
            metadata.dev(),
            metadata.ino(),
            metadata.mode(),
            metadata.nlink(),
            metadata.len(),
        ) {
            return None;
        }
        let index = KINDS.iter().position(|&candidate| candidate == kind)?;
        self.watched.insert(watch.as_raw_fd(), (index, object.hash));
        Some(watch)
    }

    fn contains(
        &mut self,
        store: &CodeStore,
        kind: ObjectKind,
        object: ObjectRef,
    ) -> io::Result<bool> {
        let index = KINDS
            .iter()
            .position(|&candidate| candidate == kind)
            .unwrap();
        let present = &mut self.present[index];
        if let Some(checked) = present.get_mut(&object.hash)
            && checked.len == object.len
        {
            checked.seen = true;
            return Ok(true);
        }
        let Some(metadata) = checked_object(store, kind, object)? else {
            return Ok(false);
        };
        // Writable or aliased files are checked by content each time.
        if metadata.mode() & 0o222 == 0 && metadata.nlink() == 1 {
            #[cfg(target_os = "macos")]
            let Some(watch) = self.watch_entry(store, kind, object, &metadata) else {
                return Ok(true);
            };
            self.present[index].insert(
                object.hash,
                Checked {
                    len: object.len,
                    seen: true,
                    #[cfg(target_os = "macos")]
                    watch,
                },
            );
        }
        Ok(true)
    }
}

fn checked_object(
    store: &CodeStore,
    kind: ObjectKind,
    object: ObjectRef,
) -> io::Result<Option<Metadata>> {
    let file = match store.open(kind, object.hash) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let metadata = file.metadata()?;
    let damaged = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "code object changed: {}",
                store.path(kind, object.hash).display()
            ),
        )
    };
    if metadata.len() != object.len {
        return Err(damaged());
    }
    let mut file = file.take(object.len.saturating_add(1));
    let hash = kind.hash_stream(object.len, |digest| {
        if io::copy(&mut file, digest)? == object.len {
            Ok(())
        } else {
            Err(damaged())
        }
    })?;
    if hash != object.hash {
        return Err(damaged());
    }
    Ok(Some(metadata))
}

pub(crate) struct Presence<'a> {
    store: &'a CodeStore,
    watch: Option<&'a mut Watch>,
    validate: bool,
}

impl<'a> Presence<'a> {
    pub(crate) fn new(
        store: &'a CodeStore,
        state: Option<&'a mut Option<Watch>>,
        validate: bool,
    ) -> Self {
        let watch = state.and_then(|state| {
            if !state
                .as_mut()
                .is_some_and(|watch| watch.refresh(store, &mut Vec::new()))
            {
                *state = Watch::open(store).ok();
            }
            state.as_mut()
        });
        Self {
            store,
            watch,
            validate,
        }
    }

    pub(crate) fn contains(&mut self, kind: ObjectKind, object: ObjectRef) -> io::Result<bool> {
        match self.watch.as_mut() {
            Some(watch) => watch.contains(self.store, kind, object),
            None if self.validate => Ok(checked_object(self.store, kind, object)?.is_some()),
            None => self.store.path(kind, object.hash).try_exists(),
        }
    }

    pub(crate) fn put(&mut self, kind: ObjectKind, bytes: &[u8]) -> io::Result<ObjectRef> {
        if !self.validate {
            return self.store.put(kind, bytes);
        }
        let object = ObjectRef {
            hash: kind.hash(bytes),
            len: bytes.len() as u64,
        };
        self.put_addressed(kind, object, bytes)
    }

    pub(crate) fn put_addressed(
        &mut self,
        kind: ObjectKind,
        object: ObjectRef,
        bytes: &[u8],
    ) -> io::Result<ObjectRef> {
        if self.contains(kind, object)? {
            return Ok(object);
        }
        self.store.put_addressed(kind, object, bytes)?;
        // A concurrent publication may have won the non-replacing write.
        if self.contains(kind, object)? {
            return Ok(object);
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "code object disappeared: {}",
                self.store.path(kind, object.hash).display()
            ),
        ))
    }

    /// Check names that changed while this publication was using them. A lost
    /// event stream invalidates every name this publication reused.
    pub(crate) fn complete(&mut self) -> io::Result<()> {
        let Some(watch) = &mut self.watch else {
            return Ok(());
        };
        let mut invalidated = Vec::new();
        if !watch.refresh(self.store, &mut invalidated) {
            for (kind, checked) in KINDS.into_iter().zip(&mut watch.present) {
                invalidated.extend(checked.drain().filter_map(
                    |(hash, Checked { len, seen, .. })| {
                        seen.then_some((kind, ObjectRef { hash, len }))
                    },
                ));
            }
        }
        for (kind, object) in invalidated {
            if checked_object(self.store, kind, object)?.is_none() {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!(
                        "code object disappeared: {}",
                        self.store.path(kind, object.hash).display()
                    ),
                ));
            }
        }
        Ok(())
    }
}

impl Drop for Presence<'_> {
    fn drop(&mut self) {
        if let Some(watch) = &mut self.watch {
            for kind in &mut watch.present {
                kind.retain(|_, checked| std::mem::take(&mut checked.seen));
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn hash_name(name: &[u8]) -> Option<ContentHash> {
    let encoded: &[u8; 64] = name.try_into().ok()?;
    let nibble = |b: u8| match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    };
    let mut hash = [0; 32];
    for (byte, chars) in hash.iter_mut().zip(encoded.chunks_exact(2)) {
        *byte = nibble(chars[0])? << 4 | nibble(chars[1])?;
    }
    Some(ContentHash(hash))
}

#[cfg(target_os = "macos")]
mod kqueue {
    use std::io;
    use std::os::fd::{AsRawFd, OwnedFd};

    /// Post an EVFILT_VNODE watch for `fflags` on `fd` to `queue`.
    pub(super) fn watch(queue: &OwnedFd, fd: i32, fflags: u32) -> io::Result<()> {
        let change = libc::kevent {
            ident: fd as usize,
            filter: libc::EVFILT_VNODE,
            flags: libc::EV_ADD | libc::EV_CLEAR | libc::EV_RECEIPT,
            fflags,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        // SAFETY: an all-integer receipt slot.
        let mut receipt: libc::kevent = unsafe { std::mem::zeroed() };
        // SAFETY: the queue, the change and the receipt slot are live.
        let n = unsafe {
            libc::kevent(
                queue.as_raw_fd(),
                &change,
                1,
                &mut receipt,
                1,
                std::ptr::null(),
            )
        };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        if receipt.flags & libc::EV_ERROR != 0 && receipt.data != 0 {
            return Err(io::Error::from_raw_os_error(receipt.data as i32));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lost_watch_requires_current_object_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let store = CodeStore::for_build(dir.path().join("code-store"));
        for kind in KINDS {
            store.put(kind, b"original").unwrap();
        }
        let body = store.put(ObjectKind::Body, b"original").unwrap();
        let mut watch = None;
        assert!(
            Presence::new(&store, Some(&mut watch), true)
                .contains(ObjectKind::Body, body)
                .unwrap()
        );

        let bodies = directory(&store, ObjectKind::Body);
        fs::rename(&bodies, dir.path().join("old-bodies")).unwrap();
        fs::create_dir(bodies).unwrap();
        fs::write(store.path(ObjectKind::Body, body.hash), b"modified").unwrap();
        for state in [Some(&mut watch), None] {
            let error = Presence::new(&store, state, true)
                .contains(ObjectKind::Body, body)
                .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        }
    }

    #[test]
    fn a_watched_removal_is_checked_on_completion() {
        let dir = tempfile::tempdir().unwrap();
        let store = CodeStore::for_build(dir.path().join("code-store"));
        for kind in KINDS {
            store.put(kind, b"original").unwrap();
        }
        let body = store.put(ObjectKind::Body, b"original").unwrap();
        let mut watch = None;
        assert!(
            Presence::new(&store, Some(&mut watch), true)
                .contains(ObjectKind::Body, body)
                .unwrap()
        );
        assert!(watch.is_some(), "the store's directories are watched");
        let mut presence = Presence::new(&store, Some(&mut watch), true);
        assert!(presence.contains(ObjectKind::Body, body).unwrap());
        fs::remove_file(store.path(ObjectKind::Body, body.hash)).unwrap();
        assert_eq!(
            presence.complete().unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }
}
