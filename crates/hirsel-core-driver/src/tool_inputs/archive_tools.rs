use super::{ContentHash, Link, Path, PathBuf, host_tools, io, recorded};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

const TOOLS: [&str; 2] = ["llvm-ar", "llvm-objcopy"];

pub(crate) fn tool_dir(link: &Link) -> io::Result<PathBuf> {
    let root = match link {
        Link::Installed(recipe) => recipe
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| io::Error::other("archive recipe has no installation around it"))?
            .to_owned(),
        Link::Driver(_) => std::env::var_os("LLVM_SYS_231_PREFIX")
            .ok_or_else(|| io::Error::other("archive tools require the pinned LLVM prefix"))?
            .into(),
    };
    root.join("bin").canonicalize()
}

pub(crate) fn tool_digest(path: &Path) -> io::Result<ContentHash> {
    Ok(recorded(path, &host_tools()?)?.content)
}

pub(super) fn install(bin: &Path, root: &Path) -> io::Result<BTreeMap<String, ContentHash>> {
    let mut tools = BTreeMap::new();
    for name in TOOLS {
        let source = bin.join(name);
        // An installation using a toolchain packaged before the archive tools
        // become available can still build executables.
        match std::fs::read(&source) {
            Ok(bytes) => {
                let key = format!("bin/{name}");
                crate::installation::install_file(&bytes, &root.join(&key), 0o755)?;
                tools.insert(key, ContentHash(Sha256::digest(&bytes).into()));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(tools)
}
