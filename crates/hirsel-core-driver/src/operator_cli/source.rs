//! `--source DIR`: a deploy or registration that sends a source tree by content (launch.md
//! "Source by content").
use super::{Approvals, Error, Upload, approvals, grant};
use crate::operator::Input;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// A source tree a deploy or registration sends by content (launch.md "Source by
/// content"): the manifest goes in the Begin or Register, and each blob the host asks for
/// by its SHA-256 follows as Data.
pub(super) struct Source {
    blobs: std::collections::HashMap<[u8; 32], Vec<u8>>,
}

impl Source {
    /// The tree under `dir`: every regular file, by its `/`-joined path, except those under
    /// a component that starts with `.`; a symbolic link or any other kind of file refuses.
    fn read(dir: &Path, package: &str, grants: Vec<String>) -> Result<(Self, Vec<u8>), Error> {
        let mut files = Vec::new();
        walk(dir, "", &mut files)?;
        files.sort();
        let content: usize = files.iter().map(|(_, bytes)| bytes.len()).sum();
        if content > crate::launcher::admit::SOURCE_BYTES {
            return Err(format!(
                "--source: {} holds {content} bytes of files, over the {} a source may",
                dir.display(),
                crate::launcher::admit::SOURCE_BYTES
            )
            .into());
        }
        let blobs = files
            .iter()
            .map(|(_, bytes)| (Sha256::digest(bytes).into(), bytes.clone()))
            .collect();
        let tree = crate::launcher::admit::Tree {
            package: package.to_owned(),
            grants,
            files,
            ..Default::default()
        };
        Ok((Self { blobs }, tree.manifest()))
    }

    /// The blobs a `Need` named, in its order, as one byte stream.
    pub(super) fn stream(&self, wanted: &[[u8; 32]]) -> Result<Vec<u8>, Error> {
        let mut out = Vec::new();
        for digest in wanted {
            let blob = self.blobs.get(digest).ok_or_else(|| {
                format!(
                    "the host asked for a blob {} the source does not hold",
                    hirsel_core_ids::hex::encode(digest)
                )
            })?;
            out.extend_from_slice(blob);
        }
        Ok(out)
    }
}

fn walk(dir: &Path, prefix: &str, files: &mut Vec<(String, Vec<u8>)>) -> Result<(), Error> {
    for entry in std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|name| format!("--source: {} is not a UTF-8 name", name.display()))?;
        if name.starts_with('.') {
            continue;
        }
        let path = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        let kind = entry.file_type()?;
        if kind.is_dir() {
            walk(&entry.path(), &path, files)?;
        } else if kind.is_file() {
            files.push((path, std::fs::read(entry.path())?));
        } else {
            return Err(format!("--source: {path} is not a regular file or directory").into());
        }
    }
    Ok(())
}

/// What a deploy or registration submits: with `--source DIR`, the tree's manifest, every
/// `--grant` as the tree's grant and its Foreign and Exec ones also as approvals; otherwise
/// the attested recipe R from `--code-store`, whose `--grant`s are approvals only.
pub(super) fn submission(
    source: Option<PathBuf>,
    positional: Option<&str>,
    code: Option<PathBuf>,
    lines: Vec<String>,
) -> Result<(Input, Option<Upload>, Option<Source>, Approvals), Error> {
    let mut grants = crate::package_dir::Grants::default();
    let Some(dir) = source else {
        for line in &lines {
            grant(&mut grants, line)?;
        }
        let upload = Upload::open(positional, code)?;
        let input = Input::Attested(upload.root.0.to_vec());
        return Ok((input, Some(upload), None, approvals(grants)));
    };
    if code.is_some() {
        return Err("--source sends the tree itself and takes no --code-store".into());
    }
    for line in &lines {
        if line.starts_with("Foreign=") || line.starts_with("Exec=") {
            grants.grant(line)?;
        }
    }
    let (source, manifest) = Source::read(&dir, positional.unwrap_or("."), lines)?;
    Ok((
        Input::Source(manifest),
        None,
        Some(source),
        approvals(grants),
    ))
}
