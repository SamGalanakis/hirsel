//! Spelling and locating the paths `hirsel add` writes.

use super::Failure;
use crate::package_content::Id;
use crate::package_files::walk;
use crate::package_manifest::SourceLocation;
use crate::package_workspace::{Member, Workspace, archive_path, identity};
use std::path::{Path, PathBuf};

/// `to` spelled relative to `from`; both are workspace-relative directories.
pub(super) fn relative_to(from: &Path, to: &Path) -> String {
    let common = from
        .components()
        .zip(to.components())
        .take_while(|(left, right)| left == right)
        .count();
    let mut parts = vec![".."; from.components().count() - common];
    let tail: Vec<_> = to.components().skip(common).collect();
    parts.extend(tail.iter().map(|part| {
        part.as_os_str()
            .to_str()
            .expect("workspace paths are UTF-8")
    }));
    if parts.is_empty() {
        ".".into()
    } else {
        parts.join("/")
    }
}

/// The member a directory argument names, matched by directory identity as discovery does.
pub(super) fn find_member<'a>(
    workspace: &'a Workspace,
    cwd: &Path,
    path: &str,
) -> Result<(&'a Id, &'a Member), Failure> {
    let refuse = || -> Failure { format!("{path} is not a member of this workspace").into() };
    let typed = Path::new(path);
    let (from, rest) = if typed.is_absolute() {
        (
            PathBuf::new(),
            typed
                .strip_prefix(&workspace.directory)
                .map_err(|_| refuse())?
                .to_path_buf(),
        )
    } else {
        (
            cwd.strip_prefix(&workspace.directory)?.to_path_buf(),
            typed.to_path_buf(),
        )
    };
    let mut components = Vec::new();
    for component in from.iter().chain(rest.iter()) {
        match component.to_str().ok_or_else(refuse)? {
            "" | "." | "/" => {}
            ".." => {
                components.pop().ok_or_else(refuse)?;
            }
            normal => components.push(normal),
        }
    }
    let wanted = identity(
        &walk(&workspace.root, Path::new(&components.join("/")))
            .and_then(|file| file.directory())
            .map_err(|_| refuse())?,
    )?;
    for (id, member) in &workspace.members {
        let dir = walk(&workspace.root, &member.directory).and_then(|file| file.directory());
        if let Ok(dir) = dir
            && identity(&dir)? == wanted
        {
            return Ok((id, member));
        }
    }
    Err(refuse())
}

/// The lock's spelling of a source declared in the root manifest.
pub(super) fn root_relative(source: &SourceLocation) -> Option<SourceLocation> {
    match source {
        SourceLocation::Archive(path) => {
            archive_path(Path::new(""), path).map(SourceLocation::Archive)
        }
        url => Some(url.clone()),
    }
}
