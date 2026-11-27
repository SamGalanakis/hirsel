//! Package edits need the authority result for each complete graph. Keep its
//! intermediate compiler records in one request-local spool. Large checks spill
//! to an unlinked file and release it with the last body that can read it.
use std::collections::HashMap;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub(super) struct Records(Mutex<Spool>);

struct Spool {
    file: tempfile::SpooledTempFile,
    entries: HashMap<PathBuf, (u64, usize)>,
}

impl Records {
    pub fn new() -> Self {
        Self(Mutex::new(Spool {
            file: tempfile::spooled_tempfile(4 << 20),
            entries: HashMap::new(),
        }))
    }

    pub fn write(&self, path: &Path, parts: &[&[u8]]) -> io::Result<()> {
        let mut spool = self.0.lock().unwrap();
        let start = spool.file.seek(SeekFrom::End(0))?;
        let mut len = 0;
        for part in parts {
            spool.file.write_all(part)?;
            len += part.len();
        }
        spool.entries.insert(path.to_owned(), (start, len));
        Ok(())
    }

    pub fn read(&self, path: &Path, limit: Option<u64>) -> io::Result<Option<Vec<u8>>> {
        let mut spool = self.0.lock().unwrap();
        let Some(&(start, len)) = spool.entries.get(path) else {
            return Ok(None);
        };
        if limit.is_some_and(|limit| len as u64 > limit) {
            return Ok(None);
        }
        spool.file.seek(SeekFrom::Start(start))?;
        let mut bytes = vec![0; len];
        spool.file.read_exact(&mut bytes)?;
        Ok(Some(bytes))
    }

    pub fn remove(&self, path: &Path) {
        self.0.lock().unwrap().entries.remove(path);
    }
}

/// Compare every byte before eliding a rename. A symlink or a read error
/// takes the usual atomic replacement path, just like a differing frame.
pub(super) fn identical(path: &Path, parts: &[&[u8]]) -> bool {
    use std::os::unix::fs::OpenOptionsExt;
    let Ok(mut file) = std::fs::File::options()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    else {
        return false;
    };
    let Ok(meta) = file.metadata() else {
        return false;
    };
    if !meta.is_file() || meta.len() != parts.iter().map(|p| p.len() as u64).sum::<u64>() {
        return false;
    }
    let mut buffer = [0; 8192];
    parts.iter().all(|part| {
        part.chunks(8192).all(|chunk| {
            let bytes = &mut buffer[..chunk.len()];
            file.read_exact(bytes).is_ok() && bytes == chunk
        })
    })
}

#[test]
fn unchanged_records_keep_their_inode_and_damage_is_repaired() {
    use std::os::unix::fs::MetadataExt;
    let dir = tempfile::tempdir().unwrap();
    let store = super::Store::new(dir.path()).unwrap();
    let put = || {
        let key = store.put(b"immutable").unwrap();
        store.select("selected", key).unwrap();
        key
    };
    let key = put();
    let body = store.path(key);
    let selected = store.selection_path("selected");
    let inode = |path: &Path| std::fs::metadata(path).unwrap().ino();
    let before = (inode(&body), inode(&selected));
    put();
    assert_eq!((inode(&body), inode(&selected)), before);
    let mut bytes = std::fs::read(&body).unwrap();
    bytes[super::HEADER] ^= 1;
    std::fs::write(&body, bytes).unwrap();
    assert!(store.get(key).unwrap().is_none());
    put();
    assert_ne!(inode(&body), before.0);
    assert_eq!(inode(&selected), before.1);
    assert_eq!(store.get(key).unwrap().unwrap(), b"immutable");
}
