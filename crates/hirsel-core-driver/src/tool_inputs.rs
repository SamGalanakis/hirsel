//! Identity of host tools selected for linking. Selection is repeated on every
//! request, so a newly installed earlier PATH candidate cannot reuse a later one.
use crate::{PackageProduct, Store};
use hirsel_core_ids::ContentHash;
use serde::Serialize;
use std::ffi::OsStr;
use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Serialize)]
pub(crate) struct Tool {
    pub(crate) path: PathBuf,
    content: ContentHash,
}
fn selected(program: &Path, search: &OsStr) -> io::Result<PathBuf> {
    if program.components().count() > 1 || program.is_absolute() {
        return std::fs::canonicalize(program);
    }
    std::env::split_paths(search)
        .map(|directory| directory.join(program))
        .find(|candidate| {
            candidate.is_file()
                && candidate
                    .metadata()
                    .is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
        })
        .map(std::fs::canonicalize)
        .transpose()?
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("link tool {} is not on PATH", program.display()),
            )
        })
}
fn identity(program: &Path, search: &OsStr, store: &Store) -> io::Result<Tool> {
    let path = selected(program, search)?;
    let m = path.metadata()?;
    let revision = (
        &path,
        m.dev(),
        m.ino(),
        m.len(),
        m.mode(),
        m.mtime(),
        m.mtime_nsec(),
        m.ctime(),
        m.ctime_nsec(),
    );
    let key = ContentHash::of_parts(
        "hirsel.host-tool-revision.v1",
        [crate::encode(&revision)?.as_slice()],
    );
    let selection = format!(
        "tool-{}",
        key.0.iter().map(|b| format!("{b:02x}")).collect::<String>()
    );
    let cached = store
        .selection(&selection)?
        .map(|hash| store.get(hash))
        .transpose()?
        .flatten();
    let content = if let Some(bytes) = cached.filter(|bytes| bytes.len() == 32) {
        ContentHash(
            bytes
                .as_slice()
                .try_into()
                .expect("external digest length checked"),
        )
    } else {
        let content = ContentHash::of_parts(
            "hirsel.host-tool-content.v1",
            [std::fs::read(&path)?.as_slice()],
        );
        let hash = store.put(&content.0)?;
        store.select(&selection, hash)?;
        content
    };
    Ok(Tool { path, content })
}
pub(crate) fn link_tools(cc: &Path) -> io::Result<(Tool, Tool)> {
    let search = std::env::var_os("PATH").unwrap_or_default();
    let store = Store::new(&PackageProduct::default_cache().join("host-tools"))?;
    let driver = identity(cc, &search, &store)?;
    // Resolve the pinned LLVM link editor rather than the host `ld`.  The
    // absolute path is part of the tool identity and is passed back to clang
    // at link time, so a host linker cannot silently enter the cache.
    let result = Command::new(&driver.path)
        .arg("-print-prog-name=ld.lld")
        .output()?;
    if !result.status.success() {
        return Err(io::Error::other(format!(
            "cannot select pinned link editor through {}: {}",
            driver.path.display(),
            String::from_utf8_lossy(&result.stderr)
        )));
    }
    let linker = std::str::from_utf8(&result.stdout)
        .map_err(io::Error::other)?
        .trim();
    if linker.is_empty() || linker.contains('\n') {
        return Err(io::Error::other(
            "C driver did not name one pinned link editor",
        ));
    }
    Ok((driver, identity(Path::new(linker), &search, &store)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn earlier_path_candidate_and_replaced_tool_change_selection() {
        let work = tempfile::tempdir().unwrap();
        let store = Store::new(&work.path().join("cache")).unwrap();
        let first = work.path().join("first");
        let second = work.path().join("second");
        std::fs::create_dir(&first).unwrap();
        std::fs::create_dir(&second).unwrap();
        let write = |p: &Path| {
            std::fs::write(p, b"tool").unwrap();
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        write(&second.join("cc"));
        let search = std::env::join_paths([&first, &second]).unwrap();
        let initial = identity(Path::new("cc"), &search, &store).unwrap();
        assert_eq!(initial.path, second.join("cc"));
        write(&first.join("cc"));
        let earlier = identity(Path::new("cc"), &search, &store).unwrap();
        assert_eq!(earlier.path, first.join("cc"));
        let replacement = first.join("new-cc");
        write(&replacement);
        std::fs::write(&replacement, b"changed").unwrap();
        std::fs::rename(replacement, first.join("cc")).unwrap();
        let replaced = identity(Path::new("cc"), &search, &store).unwrap();
        assert_eq!(earlier.path, replaced.path);
        assert_ne!(earlier.content, replaced.content);
    }
}
