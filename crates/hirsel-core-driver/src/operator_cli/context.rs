//! Trusted host pins. Resolution never consults the current package.
use super::finite::Failure;
use crate::launcher::session::Wire;
use crate::operator::{self, Answer, Ask};
use hirsel_core_ids::hex;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};

#[derive(Deserialize)]
struct Workstation {
    default: Option<String>,
    contexts: BTreeMap<String, Pin>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Pin {
    kind: Host,
    state: PathBuf,
    authority: String,
    uid: u32,
    group: Option<String>,
    incarnation: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum Host {
    Host,
}

#[derive(Serialize)]
pub(super) struct Target {
    #[serde(flatten)]
    pin: Pin,
    source: PathBuf,
    context: Option<String>,
}

pub(crate) fn operation_id(value: Option<&str>) -> Result<(u128, [u8; 16]), String> {
    let invalid = || "expected host/CATALOG/REQUEST with two 32-digit hexadecimal IDs".to_owned();
    let mut fields = value.ok_or_else(invalid)?.split('/');
    if fields.next() != Some("host") {
        return Err(invalid());
    }
    let catalog = catalog_id(fields.next().unwrap_or_default()).ok_or_else(invalid)?;
    let request = fields
        .next()
        .and_then(hex::decode)
        .and_then(|d| d.try_into().ok())
        .ok_or_else(invalid)?;
    if fields.next().is_some() {
        return Err(invalid());
    }
    Ok((catalog, request))
}

fn catalog_id(text: &str) -> Option<u128> {
    Some(u128::from_be_bytes(hex::decode(text)?.try_into().ok()?))
}

// A pin may live in a directory owned by root or the operator. A sticky system
// temporary directory protects its operator-owned children against replacement.
fn trusted_path(path: &Path, uid: u32) -> Result<(), Failure> {
    if !path.is_absolute() || path.components().any(|c| c == Component::ParentDir) {
        return Err(Failure::context("expected an absolute path without '..'"));
    }
    for parent in path.ancestors().skip(1) {
        let meta = fs::symlink_metadata(parent).map_err(Failure::context)?;
        if !meta.is_dir()
            || (meta.uid() != uid && meta.uid() != 0)
            || meta.mode() & 0o022 != 0 && !(meta.uid() == 0 && meta.mode() & 0o1000 != 0)
        {
            return Err(Failure::context(format!(
                "untrusted directory {}",
                parent.display()
            )));
        }
    }
    Ok(())
}

impl Target {
    pub fn authority(&self) -> Result<u128, Failure> {
        catalog_id(&self.pin.authority).ok_or_else(|| Failure::context("invalid catalog identity"))
    }
    pub fn state(&self) -> Result<&Path, Failure> {
        let uid = operator::operator_uid();
        let state = &self.pin.state;
        trusted_path(state, uid)?;
        let meta = fs::symlink_metadata(state).map_err(Failure::io)?;
        if !meta.is_dir() || meta.uid() != uid || meta.mode() & 0o777 != 0o700 {
            return Err(Failure::context(
                "host state must be an operator-owned directory with mode 0700",
            ));
        }
        Ok(state)
    }
    pub fn name(&self) -> Option<&str> {
        self.pin.group.as_deref()
    }

    pub fn resolve(selected: Option<&str>) -> Result<Self, Failure> {
        let uid = operator::operator_uid();
        let file = selected.map(Path::new).filter(|p| p.is_absolute());
        let source = match file {
            Some(path) => path.to_owned(),
            None => {
                if selected.is_some_and(|s| s.contains('/') || s.is_empty()) {
                    return Err(Failure::context(
                        "select a context name or an absolute pin file",
                    ));
                }
                std::env::var_os("XDG_CONFIG_HOME")
                    .filter(|p| !p.is_empty())
                    .map(PathBuf::from)
                    .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".config")))
                    .ok_or_else(|| Failure::context("no workstation configuration directory"))?
                    .join("hirsel/config.toml")
            }
        };
        eprintln!("context {}", source.display());
        trusted_path(&source, uid)?;
        let mut input = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&source)
            .map_err(Failure::context)?;
        let meta = input.metadata().map_err(Failure::context)?;
        if !meta.is_file() || (meta.uid() != uid && meta.uid() != 0) || meta.mode() & 0o022 != 0 {
            return Err(Failure::context(
                "context must be an operator- or root-owned regular file protected from other writers",
            ));
        }
        let mut text = String::new();
        input.read_to_string(&mut text).map_err(Failure::context)?;
        let (pin, context) = if file.is_some() {
            (toml::from_str(&text).map_err(Failure::context)?, None)
        } else {
            let mut settings: Workstation = toml::from_str(&text).map_err(Failure::context)?;
            let name = selected
                .map(str::to_owned)
                .or(settings.default)
                .ok_or_else(|| Failure::context("no default host context"))?;
            let pin = settings
                .contexts
                .remove(&name)
                .ok_or_else(|| Failure::context("unknown host context"))?;
            (pin, Some(name))
        };
        let target = Self {
            pin,
            source,
            context,
        };
        eprintln!("target {}", serde_json::to_string(&target).unwrap());
        Ok(target)
    }

    pub fn exchange(&self, authority: u128, request: &Ask) -> Result<Answer, Failure> {
        self.query(authority, request, None)
    }
    pub fn query(
        &self,
        authority: u128,
        request: &Ask,
        wait: Option<&super::wait::Wait>,
    ) -> Result<Answer, Failure> {
        if let Some(w) = wait {
            w.remaining()?;
        }
        let mut conn = self.open(authority, wait.map(|w| w.observer))?;
        self.send(&mut conn, authority, request)?;
        conn.recv()
            .map_err(Failure::io)?
            .ok_or_else(Failure::disconnected)
    }

    pub fn send(
        &self,
        conn: &mut operator::Connection,
        authority: u128,
        request: &Ask,
    ) -> Result<(), Failure> {
        conn.send(&Ask::Pinned(
            (authority >> 64) as u64,
            authority as u64,
            request.encode(),
        ))
        .map_err(Failure::io)
    }

    pub fn open(
        &self,
        authority: u128,
        observer: Option<super::Observer>,
    ) -> Result<operator::Connection, Failure> {
        let uid = operator::operator_uid();
        if self.pin.uid != uid || catalog_id(&self.pin.authority) != Some(authority) {
            return Err(Failure::context(
                "operation and context must pin the same authority and effective UID",
            ));
        }
        if self.pin.incarnation.as_ref().is_some_and(|s| {
            self.pin.group.is_none() || s.parse::<u64>().map(|v| v.to_string()).as_ref() != Ok(s)
        }) {
            return Err(Failure::context(
                "incarnation requires a group and a decimal u64",
            ));
        }
        let state = self.state()?;
        let socket = state.join(crate::product::bundled::OPERATOR_SOCKET);
        let meta = fs::symlink_metadata(&socket).map_err(Failure::io)?;
        if !meta.file_type().is_socket() || meta.uid() != uid || meta.mode() & 0o777 != 0o600 {
            return Err(Failure::context(
                "operator.sock must be an operator-owned socket with mode 0600",
            ));
        }
        let mut conn = operator::connect(state).map_err(Failure::io)?;
        if let Some(observer) = observer {
            conn.observe(observer).map_err(Failure::io)?;
        } else {
            conn.set_read_timeout(Some(super::PIN_WAIT))
                .map_err(Failure::io)?;
        }
        conn.send(&Ask::Identify).map_err(Failure::io)?;
        match conn.recv().map_err(Failure::io)? {
            Some(Answer::Catalog(hi, lo))
                if (u128::from(hi) << 64 | u128::from(lo)) == authority =>
            {
                Ok(conn)
            }
            Some(Answer::Catalog(..)) => Err(Failure::context("catalog identity changed")),
            Some(Answer::Error(class, detail)) => Err(Failure::authority(class, detail)),
            Some(_) => Err(Failure::damaged("unexpected catalog handshake")),
            None => Err(Failure::disconnected()),
        }
    }

    pub fn check_group(&self, group: &crate::launcher::session::Group) -> Result<(), Failure> {
        if self.pin.group.as_ref().is_some_and(|g| g != &group.name)
            || self
                .pin
                .incarnation
                .as_ref()
                .is_some_and(|i| i != &group.incarnation.to_string())
        {
            Err(Failure::context(
                "request row contradicts the pinned group or incarnation",
            ))
        } else {
            Ok(())
        }
    }
}
