//! Dump and load durable W2 filesystem snapshots for T7 cross-build reopen.
//!
//! When `HIRSEL_W2_DUMP_DIR` is set, the post-restart durable image at phase 2
//! is written to `<dir>/fs/` alongside `<dir>/meta.txt` (`acked`, `floor`,
//! `events`). When `HIRSEL_W2_LOAD_DIR` is set, `store_crash.c` loads that
//! image into a fresh bridge and runs only phase 2 (`read`) under the current
//! build.
use std::{
    ffi::c_void,
    fs,
    path::{Component, Path},
    slice,
};

use super::{Bridge, Op};

fn safe_relative(name: &str) -> bool {
    let path = Path::new(name);
    !name.is_empty()
        && !path.is_absolute()
        && path.components().all(|c| matches!(c, Component::Normal(_)))
}

fn collect_files(root: &Path, current: &Path, out: &mut Vec<(String, Vec<u8>)>) -> bool {
    let Ok(entries) = fs::read_dir(current) else {
        return false;
    };
    let mut paths: Vec<_> = match entries.collect::<Result<Vec<_>, _>>() {
        Ok(list) => list.into_iter().map(|e| e.path()).collect(),
        Err(_) => return false,
    };
    paths.sort();
    for path in paths {
        let Ok(meta) = fs::symlink_metadata(&path) else {
            return false;
        };
        if meta.file_type().is_symlink() {
            return false;
        }
        if meta.is_dir() {
            if !collect_files(root, &path, out) {
                return false;
            }
        } else if meta.is_file() {
            let Ok(rel) = path.strip_prefix(root) else {
                return false;
            };
            let Some(name) = rel.to_str() else {
                return false;
            };
            let Ok(bytes) = fs::read(&path) else {
                return false;
            };
            out.push((name.to_owned(), bytes));
        }
    }
    true
}

impl Bridge {
    pub(crate) fn maybe_dump_snapshot(&self) {
        let Some(target) = std::env::var_os("HIRSEL_W2_DUMP_DIR") else {
            return;
        };
        if target.is_empty() {
            return;
        }
        let dir = Path::new(&target);
        let fs_dir = dir.join("fs");
        fs::create_dir_all(&fs_dir).expect("create W2 dump directory");
        let snap = self.fs.snapshot();
        for (name, inode) in &snap.names {
            assert!(safe_relative(name), "unsafe snapshot path: {name}");
            let dst = fs_dir.join(name);
            if let Some(parent) = dst.parent() {
                fs::create_dir_all(parent).expect("create W2 dump parent");
            }
            let bytes = snap.data.get(inode).map(Vec::as_slice).unwrap_or(&[]);
            fs::write(&dst, bytes).expect("write W2 snapshot file");
        }
        let meta = format!(
            "acked={}\nfloor={}\nevents={}\n",
            u8::from(self.acked),
            self.floor,
            self.events
        );
        fs::write(dir.join("meta.txt"), meta).expect("write W2 snapshot meta");
    }

    pub(crate) fn load_snapshot(&mut self, dir: &Path) -> bool {
        let fs_dir = dir.join("fs");
        if !fs_dir.is_dir() {
            return false;
        }
        let Ok(meta) = fs::read_to_string(dir.join("meta.txt")) else {
            return false;
        };
        let mut acked = None;
        let mut floor = None;
        for line in meta.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            match key {
                "acked" => acked = Some(value == "1"),
                "floor" => floor = value.parse::<u64>().ok(),
                "events" => {
                    if let Ok(n) = value.parse::<u64>() {
                        self.events = n;
                    }
                }
                _ => {}
            }
        }
        let (Some(acked), Some(floor)) = (acked, floor) else {
            return false;
        };
        let mut files = Vec::new();
        if !collect_files(&fs_dir, &fs_dir, &mut files) {
            return false;
        }
        for (name, bytes) in files {
            if !safe_relative(&name) {
                return false;
            }
            self.fs.issue(&Op::Create(&name));
            self.fs.issue(&Op::Write(&name, &bytes));
            self.fs.issue(&Op::Sync(&name));
        }
        self.fs.issue(&Op::SyncDir);
        self.acked = acked;
        self.floor = floor;
        true
    }
}

/// Load a dumped W2 durable snapshot into a fresh bridge before phase 2 (`read`).
///
/// # Safety
/// `p` is live and exclusively owned by the wrapper before any run; `dir` points
/// to `len` valid UTF-8 bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn store_crash_load_snapshot(
    p: *mut c_void,
    dir: *const u8,
    len: usize,
) -> u8 {
    let bridge = unsafe { &mut *p.cast::<Bridge>() };
    let bytes = if len == 0 {
        &[]
    } else {
        unsafe { slice::from_raw_parts(dir, len) }
    };
    let Ok(text) = std::str::from_utf8(bytes) else {
        return 0;
    };
    u8::from(bridge.load_snapshot(Path::new(text)))
}
