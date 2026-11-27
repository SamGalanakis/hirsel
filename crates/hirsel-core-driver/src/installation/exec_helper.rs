//! The desktop Exec helper's installed image.
use hirsel_core_ids::ContentHash;
use hirsel_core_layout::{Os, Target};
use std::io;
use std::path::{Path, PathBuf};

pub(super) fn read<'a>(path: &Path, target: Option<&'a str>) -> io::Result<(Vec<u8>, &'a str)> {
    let target =
        target.ok_or_else(|| io::Error::other("--exec-helper requires --embed-runtime"))?;
    match Target::from_triple(target).map(Target::os) {
        Some(Os::Linux | Os::Macos) => Ok((std::fs::read(path)?, target)),
        Some(Os::Ios) | None => Err(io::Error::other("--exec-helper requires a desktop target")),
    }
}

pub(super) fn install(root: &Path, bytes: &[u8], target: &str) -> io::Result<PathBuf> {
    let relative = Path::new("runtime")
        .join(target)
        .join(ContentHash::of_bytes(bytes).to_string())
        .join("hirsel-exec-helper");
    super::install_file(bytes, &root.join(&relative), 0o755)?;
    Ok(relative)
}
