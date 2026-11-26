//! A submitted source tree (`launcher.Tree`), its manifest (launch.md "Source by content")
//! and the package roots its lock may pin (launch.md "Package roots per group").
use super::super::session::{Failure, Reader, Wire, Writer};
use std::io;

/// Submitted source (`launcher.Tree`): a workspace's files, the package directory to
/// publish within it, and the `--grant` and `--dir` values its recipe binds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Tree {
    pub(crate) package: String,
    pub(crate) grants: Vec<String>,
    pub(crate) dirs: Vec<String>,
    pub(crate) files: Vec<(String, Vec<u8>)>,
    // The platform checks these against the candidate's preview before importing.
    pub(crate) required_entries: Vec<(u32, Vec<u8>)>,
}

/// The most codec bytes a tree may take (`launcher.TREE_BYTES`): its manifest's limit and
/// its content's.
pub(crate) const TREE_BYTES: usize = 5 << 20;

/// The most file content one tree holds (`launcher.SOURCE_BYTES`).
pub(crate) const SOURCE_BYTES: usize = 4 << 20;

impl Tree {
    /// The codec bytes of the tree's `launcher.Manifest`: the tree with each file's content
    /// replaced by its SHA-256 and length, which is what an operator's Begin carries.
    pub(crate) fn manifest(&self) -> Vec<u8> {
        use sha2::Digest as _;
        let mut w = Writer::default();
        w.bytes(self.package.as_bytes());
        w.strings(&self.grants);
        w.strings(&self.dirs);
        w.len(self.required_entries.len());
        for (kind, key) in &self.required_entries {
            w.u32(*kind);
            w.bytes(key);
        }
        w.len(self.files.len());
        for (path, bytes) in &self.files {
            w.bytes(path.as_bytes());
            w.bytes(&sha2::Sha256::digest(bytes));
            w.u64(bytes.len() as u64);
        }
        w.0
    }
}

/// The lock file at the root of a submitted tree.
const LOCK_FILE: &str = "hirsel.lock";

/// Whether every package root `tree`'s lock pins is one of the group's `packages`: the
/// launcher's package store is shared, so a group links only the roots its registration
/// names (launch.md "Package roots per group").
pub(in crate::launcher) fn lock_roots_allowed(
    tree: &Tree,
    packages: &[[u8; 32]],
) -> Result<(), Failure> {
    let Some((_, lock)) = tree.files.iter().find(|(path, _)| path == LOCK_FILE) else {
        return Ok(());
    };
    let Some(pinned) = crate::package_lock::pinned_roots(lock) else {
        return Err(Failure::Invalid(format!("{LOCK_FILE} is not a lock file")));
    };
    match pinned
        .into_iter()
        .find(|(_, root, _)| !packages.contains(&root.0))
    {
        None => Ok(()),
        Some((id, root, source)) => Err(Failure::Invalid(format!(
            "{id} {root} is not one of the group's package roots, and --frozen does not fetch (source {source})"
        ))),
    }
}

impl Wire for Tree {
    fn put(&self, w: &mut Writer) {
        w.bytes(self.package.as_bytes());
        w.strings(&self.grants);
        w.strings(&self.dirs);
        w.len(self.files.len());
        for (path, bytes) in &self.files {
            w.bytes(path.as_bytes());
            w.bytes(bytes);
        }
        w.len(self.required_entries.len());
        for (kind, key) in &self.required_entries {
            w.u32(*kind);
            w.bytes(key);
        }
    }
    fn get(r: &mut Reader<'_>) -> io::Result<Self> {
        let package = r.string()?;
        let grants = r.strings()?;
        let dirs = r.strings()?;
        let files = (0..r.count(8)?)
            .map(|_| Ok((r.string()?, r.bytes()?)))
            .collect::<io::Result<_>>()?;
        let required_entries = (0..r.count(8)?)
            .map(|_| Ok((r.u32()?, r.bytes()?)))
            .collect::<io::Result<_>>()?;
        Ok(Self {
            package,
            grants,
            dirs,
            files,
            required_entries,
        })
    }
}
