use super::*;
use crate::package_add::stage;
use crate::package_lock::NAME as LOCK;
use std::io;
use tempfile::NamedTempFile;

pub(super) struct Edit {
    pub(super) dir: File,
    pub(super) relative: PathBuf,
    pub(super) before: Captured,
    pub(super) after: Captured,
}

impl Edit {
    pub(super) fn new(
        workspace: &Workspace,
        dir: File,
        relative: PathBuf,
    ) -> Result<Self, Failure> {
        let before = workspace
            .captured_manifest(&dir)?
            .ok_or("workspace manifest disappeared")?
            .clone();
        let after = before.clone();
        Ok(Self {
            dir,
            relative,
            before,
            after,
        })
    }

    pub(super) fn replace(&mut self, text: String) {
        self.after.bytes = text.into_bytes().into();
    }

    pub(super) fn changed(&self) -> bool {
        self.before.bytes != self.after.bytes
    }
}

struct Staged<'a> {
    dir: &'a File,
    name: &'static str,
    before: Option<NamedTempFile>,
    after: NamedTempFile,
    path: PathBuf,
}

impl<'a> Staged<'a> {
    fn new(
        dir: &'a File,
        name: &'static str,
        before: Option<&Captured>,
        after: &[u8],
    ) -> io::Result<Self> {
        Ok(Self {
            dir,
            name,
            before: before.map(|b| stage(dir, name, &b.bytes)).transpose()?,
            after: stage(dir, name, after)?,
            path: crate::host::fd_path(dir)?.join(name),
        })
    }
}

pub(crate) fn commit<'a>(
    command: &str,
    cwd: &Path,
    scope: &File,
    workspace: &Workspace,
    edits: impl IntoIterator<Item = (&'a File, &'a Captured, &'a Captured)>,
    before_lock: Option<&Captured>,
    lock: &Lock,
) -> Result<Option<String>, Failure> {
    let mut staged = Vec::new();
    for (dir, before, after) in edits {
        staged.push(Staged::new(dir, MANIFEST, Some(before), &after.bytes)?);
    }
    if let Some((bytes, _)) = lock.pending() {
        staged.push(Staged::new(scope, LOCK, before_lock, bytes.as_bytes())?);
    }
    let current = current_scope(cwd, scope, command)?;
    if !workspace.same_manifests(&current) || Lock::capture(&current)?.as_ref() != before_lock {
        return Err(format!(
            "a manifest or hirsel.lock changed while hirsel {command} ran; nothing was written"
        )
        .into());
    }
    let mut published = Vec::new();
    for Staged {
        dir,
        name,
        before,
        after,
        path,
    } in staged
    {
        let result = match after.persist(&path) {
            Ok(_) => {
                published.push((dir, name, before, path));
                dir.sync_all()
            }
            Err(error) => Err(error.error),
        };
        if let Err(error) = result {
            let mut failed = Vec::new();
            for (dir, name, before, path) in published.into_iter().rev() {
                let restored = match before {
                    Some(before) => before.persist(&path).map(|_| ()).map_err(|e| e.error),
                    None => std::fs::remove_file(&path),
                }
                .and_then(|()| dir.sync_all());
                if let Err(rollback) = restored {
                    failed.push(format!("{name}: {rollback}"));
                }
            }
            return if failed.is_empty() {
                Err(format!("{command} rolled back: {error}").into())
            } else {
                Ok(Some(format!(
                    "{command} publication failed: {error}\n  rollback failed: {}",
                    failed.join(", ")
                )))
            };
        }
    }
    Ok(None)
}
