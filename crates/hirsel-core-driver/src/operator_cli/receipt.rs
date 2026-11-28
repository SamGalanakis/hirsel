//! Caller-owned recovery coordinates. Only immutable hashes and input locators are saved.
use super::finite::Failure;
use crate::launcher::session::Wire;
use crate::operator::Precondition;
use hirsel_core_ids::hex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
pub(super) struct Inputs {
    pub package: Option<String>,
    pub source: Option<PathBuf>,
    pub recipe: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollback: Option<u64>,
    pub code: Option<PathBuf>,
    pub create: Option<PathBuf>,
}
#[derive(Serialize, Deserialize)]
pub(super) struct Receipt {
    pub id: String,
    pub intent_digest: String,
    pub input_digest: String,
    pub subject: String,
    pub inputs: Inputs,
}
pub(super) fn digest(v: &impl Wire) -> String {
    hex::encode(&Sha256::digest(v.encode()))
}

pub(super) struct Location {
    pub id: String,
    pub path: PathBuf,
}

pub(super) fn operations_dir() -> Result<PathBuf, Failure> {
    let state = std::env::var_os("XDG_STATE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|v| PathBuf::from(v).join(".local/state")))
        .filter(|v| v.is_absolute())
        .ok_or_else(|| Failure::input("receipt", "an absolute state directory is required"))?;
    Ok(state.join("hirsel/operations"))
}
impl Location {
    pub fn new(authority: u128, d: &[u8]) -> Result<Self, Failure> {
        let request = hex::encode(d);
        let path = operations_dir()?.join(format!("host-{authority:032x}-{request}.json"));
        Ok(Self {
            id: format!("host/{authority:032x}/{request}"),
            path,
        })
    }
    pub fn read(&self) -> Result<Option<Receipt>, Failure> {
        let mut opts = fs::OpenOptions::new();
        let file = match opts
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&self.path)
        {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(Failure::io(e)),
        };
        let meta = file.metadata().map_err(Failure::io)?;
        if !meta.is_file()
            || meta.uid() != crate::operator::operator_uid()
            || meta.mode() & 0o077 != 0
        {
            return Err(Failure::input("receipt", "untrusted intent receipt"));
        }
        let r: Receipt = serde_json::from_reader(file).map_err(|e| Failure::input("receipt", e))?;
        if self.id != r.id {
            return Err(Failure::input(
                "receipt",
                "receipt names a different operation",
            ));
        }
        Ok(Some(r))
    }
    pub fn save(&self, receipt: &Receipt) -> Result<(), Failure> {
        match persist(&self.path, receipt) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                if self.read()?.is_some_and(|r| {
                    r.intent_digest == receipt.intent_digest
                        && r.inputs.rollback == receipt.inputs.rollback
                }) {
                    Ok(())
                } else {
                    Err(Failure::input(
                        "request-id",
                        "request ID already names another intent",
                    ))
                }
            }
            Err(e) => Err(Failure::io(e)),
        }
    }
    pub fn decision(&self, frame: &impl Wire) -> Result<(), Failure> {
        let hash = digest(frame);
        let path = self.path.with_extension(format!("decision-{hash}.json"));
        match persist(
            &path,
            &serde_json::json!({"id":self.id,"decision_digest":hash}),
        ) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(()),
            Err(e) => Err(Failure::io(e)),
        }
    }
    fn display_path(&self, root: &[u8], text: &str) -> PathBuf {
        self.path.with_extension(format!(
            "display-{}-{}.json",
            hex::encode(root),
            hex::encode(&Sha256::digest(text.as_bytes()))
        ))
    }
    pub fn display_approved(&self, root: &[u8], text: &str) -> Result<bool, Failure> {
        match fs::symlink_metadata(self.display_path(root, text)) {
            Ok(meta) => Ok(meta.is_file()
                && meta.uid() == crate::operator::operator_uid()
                && meta.mode() & 0o077 == 0),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(Failure::io(e)),
        }
    }
    pub fn save_display(&self, root: &[u8], text: &str) -> Result<(), Failure> {
        match persist(
            &self.display_path(root, text),
            &serde_json::json!({"id":self.id}),
        ) {
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(()),
            result => result.map_err(Failure::io),
        }
    }
    pub fn announce(&self) -> Result<(), Failure> {
        let mut err = io::stderr().lock();
        writeln!(
            err,
            "request {}  {}",
            self.id.rsplit('/').next().unwrap(),
            self.id
        )
        .and_then(|()| err.flush())
        .map_err(Failure::io)
    }
}
impl Receipt {
    pub fn precondition(&self) -> Result<Precondition, Failure> {
        hex::decode(&self.subject)
            .and_then(|b| Precondition::decode(&b).ok())
            .ok_or_else(|| Failure::input("receipt", "invalid stored precondition"))
    }
}
fn make_dirs(path: &Path) -> io::Result<()> {
    if path.is_dir() {
        return Ok(());
    }
    let parent = path.parent().ok_or(io::ErrorKind::InvalidInput)?;
    make_dirs(parent)?;
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => (),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists && path.is_dir() => (),
        Err(e) => return Err(e),
    }
    fs::File::open(parent)?.sync_all()
}
pub(super) fn persist(path: &Path, value: &impl Serialize) -> io::Result<()> {
    let dir = path.parent().unwrap();
    make_dirs(dir)?;
    let m = fs::symlink_metadata(dir)?;
    if !m.is_dir() || m.uid() != crate::operator::operator_uid() || m.mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "operations directory must be private",
        ));
    }
    let mut t = tempfile::NamedTempFile::new_in(dir)?;
    serde_json::to_writer(&mut t, value)?;
    t.write_all(b"\n")?;
    t.as_file().sync_all()?;
    let saved = t.persist_noclobber(path);
    fs::File::open(dir)?.sync_all()?;
    saved.map(|_| ()).map_err(|e| e.error)
}
