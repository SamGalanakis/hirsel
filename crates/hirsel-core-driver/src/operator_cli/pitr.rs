//! A restore decision pins the reviewed source cut and destination. The helper
//! receives directory handles for the same grant coordinates on plan and apply.
use super::{
    finite::{Envelope, Failure},
    store_process,
};
use crate::cli::{Request, StoreOption as O};
use hirsel_core_ids::hex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

#[derive(Serialize, Deserialize)]
pub(super) struct RestorePlan {
    source: PathBuf,
    destination: PathBuf,
    source_dir: DirectoryId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    archive: Option<super::archive::Archive>,
    destination_parent: DirectoryId,
    existing_destination: Option<DirectoryId>,
    pub(super) attempt: String,
    point: Point,
}

#[derive(Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct DirectoryId {
    device: String,
    inode: String,
}
impl DirectoryId {
    pub(super) fn read(dir: &File) -> Result<Self, Failure> {
        let m = dir.metadata().map_err(Failure::io)?;
        Ok(Self {
            device: m.dev().to_string(),
            inode: m.ino().to_string(),
        })
    }
}
#[derive(Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct Point {
    pub parent: String,
    pub boundary: String,
    pub branch: String,
    pub version: String,
    pub at_ms: String,
    pub main_version: String,
    pub main_at_ms: String,
    pub base: String,
    pub unit: String,
    pub frame_end: String,
    pub activation: String,
    pub release_binding: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub child: Option<String>,
}

fn directory(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(path)
}

pub(super) fn source_handle(path: &Path, archive: bool) -> Result<File, Failure> {
    if archive {
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .map_err(Failure::io)
    } else {
        directory(path).map_err(Failure::io)
    }
}
fn selector(text: &str) -> Result<Vec<String>, Failure> {
    fn invalid() -> Failure {
        Failure::input(
            "target",
            "expected latest, version:BRANCH,V, at:BRANCH,MS or boundary:TOKEN",
        )
    }
    if text == "latest" {
        return Ok(vec![text.into()]);
    }
    let (kind, value) = text.split_once(':').ok_or_else(invalid)?;
    if kind == "boundary" && !value.is_empty() && hex::decode(value).is_some() {
        return Ok(vec![kind.into(), value.into()]);
    }
    let (branch, number) = value.split_once(',').ok_or_else(invalid)?;
    let branch = if branch == "main" {
        0
    } else {
        branch.parse::<u32>().map_err(|_| invalid())?
    };
    let n: u64 = number.parse().map_err(|_| invalid())?;
    if !matches!(kind, "at" | "version") {
        return Err(invalid());
    }
    Ok(vec![kind.into(), branch.to_string(), n.to_string()])
}

fn invoke(from: &File, to: &File, verb: &str, args: &[String]) -> Result<Vec<u8>, Failure> {
    let output = store_process::command("store_tool", from, to)
        .map_err(|e| Failure::blocked("Unavailable", &e.to_string()))?
        .arg(verb)
        .args(args)
        .output()
        .map_err(Failure::io)?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        return Err(match output.status.code() {
            Some(2) => Failure::input("target", detail),
            Some(75) => Failure::blocked("Held", &detail),
            _ => Failure::authority(crate::operator::Class::Store, detail.into_owned()),
        });
    }
    Ok(output.stdout)
}

fn plan_at(
    source: &File,
    archive: Option<&super::archive::Archive>,
    args: &[String],
) -> Result<Point, Failure> {
    let scratch = tempfile::tempdir().map_err(Failure::io)?;
    let dir = directory(scratch.path()).map_err(Failure::io)?;
    if let Some(profile) = archive {
        return serde_json::from_value(profile.invoke(&dir, "plan-archive", args, false)?)
            .map_err(Failure::damaged);
    }
    serde_json::from_slice(&invoke(source, &dir, "plan-backup", args)?).map_err(Failure::damaged)
}

fn resolved(path: &Path) -> io::Result<PathBuf> {
    match fs::symlink_metadata(path) {
        Ok(_) => path.canonicalize(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let parent = path.parent().ok_or(e)?;
            Ok(resolved(parent)?.join(path.file_name().ok_or(io::ErrorKind::InvalidInput)?))
        }
        Err(e) => Err(e),
    }
}
pub(super) fn check_evidence_paths(source: &Path, destination: &Path) -> Result<(), Failure> {
    let storage = resolved(&super::pitr_decision::operations_dir()?).map_err(Failure::io)?;
    let temp = std::env::temp_dir().canonicalize().map_err(Failure::io)?;
    for store in [source, destination] {
        if storage.starts_with(store) || store.starts_with(&storage) || temp.starts_with(store) {
            return Err(Failure::input(
                "to",
                "operation evidence and temporary files must lie outside both stores",
            ));
        }
    }
    Ok(())
}
pub(super) fn attempt() -> Result<String, Failure> {
    let mut nonce = [0; 16];
    File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut nonce))
        .map_err(Failure::io)?;
    Ok(hex::encode(&nonce))
}
fn plan(req: Request<O>, env: &mut Envelope<'_>) -> Result<(), Failure> {
    let (mut src, mut dst, mut selected, mut explicit) = (None, None, None, false);
    for o in req.options {
        match o {
            O::RestoreFrom(v) if src.is_none() => src = Some(v),
            O::RestoreTo(v) if dst.is_none() => dst = Some(v),
            O::RestoreTarget(v) if selected.is_none() => selected = Some(selector(&v)?),
            O::Plan if !explicit => explicit = true,
            O::Json => (),
            _ => return Err(Failure::input("arguments", "duplicate or unrelated option")),
        }
    }
    let input = src.ok_or_else(|| Failure::input("from", "required"))?;
    let archive_path = input.strip_prefix('@');
    let source = fs::canonicalize(archive_path.unwrap_or(&input)).map_err(Failure::io)?;
    let archive = archive_path
        .map(|_| super::archive::Archive::read(&source))
        .transpose()?;
    let dest = dst.ok_or_else(|| Failure::input("to", "required"))?;
    let args = selected.ok_or_else(|| Failure::input("target", "required"))?;
    if let Some(profile) = dest.strip_prefix('@') {
        let plan = super::bucket::plan(&source, archive, Path::new(profile), &args)?;
        return save_plan(super::pitr_decision::StorePlan::Bucket(plan), explicit, env);
    }
    let destination = PathBuf::from(dest);
    let name = destination
        .file_name()
        .ok_or_else(|| Failure::input("to", "expected a child directory"))?;
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .canonicalize()
        .map_err(Failure::io)?;
    let destination = parent.join(name);
    if destination.starts_with(&source) || source.starts_with(&destination) {
        return Err(Failure::input("to", "source and destination overlap"));
    }
    check_evidence_paths(&source, &destination)?;
    let from = source_handle(&source, archive.is_some())?;
    let par = directory(&parent).map_err(Failure::io)?;
    let existing_destination = match directory(&destination) {
        Ok(d) => {
            if fs::read_dir(&destination)
                .map_err(Failure::io)?
                .next()
                .is_some()
            {
                return Err(Failure::input("to", "expected an empty destination"));
            }
            Some(DirectoryId::read(&d)?)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(Failure::io(e)),
    };
    let point = plan_at(&from, archive.as_ref(), &args)?;
    let plan = RestorePlan {
        source,
        destination,
        source_dir: DirectoryId::read(&from)?,
        archive,
        destination_parent: DirectoryId::read(&par)?,
        existing_destination,
        attempt: attempt()?,
        point,
    };
    save_plan(
        super::pitr_decision::StorePlan::Restore(plan),
        explicit,
        env,
    )
}

fn save_plan(
    plan: super::pitr_decision::StorePlan,
    explicit: bool,
    env: &mut Envelope<'_>,
) -> Result<(), Failure> {
    env.contract = if explicit { "plan" } else { "submit" };
    super::pitr_decision::save_plan(&plan)?;
    plan.held(env);
    if !explicit {
        env.exit = 75
    }
    Ok(())
}

pub(crate) fn restore(req: Request<O>) -> i32 {
    let machine = req.options.iter().any(|o| matches!(o, O::Json));
    let mut envelope = Envelope::read("store restore");
    if let Err(f) = plan(req, &mut envelope) {
        envelope.fail(f)
    }
    envelope.render(machine)
}

impl RestorePlan {
    pub(super) fn held(&self, digest: &str, env: &mut Envelope<'_>) {
        let id = format!("store/{}", self.attempt);
        env.target = json!({"kind":"directory","destination":self.destination});
        env.operation = json!({"kind":"store","id":id,"request_id":self.attempt});
        env.result = json!({"plan_digest":digest,"source":self.source,"destination":self.destination,"attempt":self.attempt,"point":self.point});
        env.next = vec![json!({"method":"op decide","operation":id})];
        env.outcome = json!({"kind":"AwaitDecision","effect":"none"});
    }

    fn dest(&self, create: bool) -> Result<Option<File>, Failure> {
        let par = directory(
            self.destination
                .parent()
                .ok_or_else(|| Failure::damaged("missing parent"))?,
        )
        .map_err(Failure::io)?;
        if self.destination_parent != DirectoryId::read(&par)? {
            return Err(Failure::input("to", "destination parent changed"));
        }
        let name = std::ffi::CString::new(
            self.destination
                .file_name()
                .ok_or_else(|| Failure::input("to", "missing name"))?
                .as_encoded_bytes(),
        )
        .map_err(|_| Failure::input("to", "invalid path"))?;
        use std::os::fd::{AsRawFd, FromRawFd};
        if create && self.existing_destination.is_none() {
            // SAFETY: par owns the directory and name is one NUL-terminated component.
            if unsafe { libc::mkdirat(par.as_raw_fd(), name.as_ptr(), 0o700) } < 0 {
                let e = io::Error::last_os_error();
                if e.kind() != io::ErrorKind::AlreadyExists {
                    return Err(Failure::io(e));
                }
            }
            par.sync_all().map_err(Failure::io)?;
        }
        // SAFETY: openat resolves the final component under the checked directory.
        let fd = unsafe {
            libc::openat(
                par.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            let e = io::Error::last_os_error();
            if !create && e.kind() == io::ErrorKind::NotFound {
                return Ok(None);
            }
            return Err(Failure::io(e));
        }
        // SAFETY: openat returned a fresh owned descriptor.
        let to = unsafe { File::from_raw_fd(fd) };
        let actual = DirectoryId::read(&to)?;
        if self
            .existing_destination
            .as_ref()
            .is_some_and(|d| *d != actual)
            || self.source_dir == actual
        {
            return Err(Failure::input("to", "destination identity changed"));
        }
        Ok(Some(to))
    }

    pub(super) fn observe(&self, path: &Path) -> Result<Option<Value>, Failure> {
        let Some(expected) = super::pitr_decision::read::<DirectoryId>(path)? else {
            return Ok(None);
        };
        let to = self
            .dest(false)?
            .ok_or_else(|| Failure::damaged("reserved destination disappeared"))?;
        if DirectoryId::read(&to)? != expected {
            return Err(Failure::damaged("reserved destination was replaced"));
        }
        let mut args = self.point.pinned();
        args.extend([
            self.attempt.clone(),
            self.point.version.clone(),
            self.point.at_ms.clone(),
        ]);
        let bytes = invoke(&to, &to, "observe-restore", &args)?;
        let child = std::str::from_utf8(&bytes)
            .map_err(Failure::damaged)?
            .trim();
        if child == "pending" {
            return Ok(None);
        }
        Ok(Some(self.point.receipt(
            child,
            &self.attempt,
            &self.destination,
        )?))
    }

    pub(super) fn publish(&self, path: &Path) -> Result<Value, Failure> {
        let from = source_handle(&self.source, self.archive.is_some())?;
        if let Some(expected) = &self.archive {
            if expected != &super::archive::Archive::read(&self.source)? {
                return Err(Failure::input("from", "archive descriptor changed"));
            }
        }
        if self.source_dir != DirectoryId::read(&from)? {
            return Err(Failure::input("from", "source directory changed"));
        }
        check_evidence_paths(&self.source, &self.destination)?;
        let mut args = self.point.pinned();
        if plan_at(&from, self.archive.as_ref(), &args)? != self.point {
            return Err(Failure::input(
                "plan_digest",
                "selected boundary or release changed",
            ));
        }
        let to = self
            .dest(true)?
            .ok_or_else(|| Failure::damaged("missing destination"))?;
        super::pitr_decision::once(path, &DirectoryId::read(&to)?)?;
        args.push(self.attempt.clone());
        let mut applied: Point = if let Some(archive) = &self.archive {
            serde_json::from_value(archive.invoke(&to, "publish-archive", &args, false)?)
                .map_err(Failure::damaged)?
        } else {
            serde_json::from_slice(&invoke(&from, &to, "publish-backup", &args)?)
                .map_err(Failure::damaged)?
        };
        let child = applied
            .child
            .take()
            .ok_or_else(|| Failure::damaged("missing child identity"))?;
        if applied != self.point {
            return Err(Failure::damaged("published boundary differs from the plan"));
        }
        self.point.receipt(&child, &self.attempt, &self.destination)
    }
}

impl Point {
    pub(super) fn pinned(&self) -> Vec<String> {
        vec![
            "selected".into(),
            self.branch.clone(),
            self.boundary.clone(),
        ]
    }
    pub(super) fn receipt(
        &self,
        child: &str,
        attempt: &str,
        destination: impl Serialize,
    ) -> Result<Value, Failure> {
        if child == self.parent || !hex::decode(child).is_some_and(|v| v.len() == 16) {
            return Err(Failure::damaged("invalid child identity"));
        }
        let mut receipt = serde_json::to_value(self).map_err(Failure::damaged)?;
        receipt["child"] = json!(child);
        receipt["attempt"] = json!(attempt);
        receipt["verification"] = json!("store_graph");
        receipt["publication"] = json!("Published");
        receipt["destination"] = json!(destination);
        Ok(receipt)
    }
}
