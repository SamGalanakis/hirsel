//! Native Exec admission. This module acquires the pinned resources before main.
use super::{Policy, Profile};
use crate::host::image as host;
use ring::digest::{Context, SHA256};
use std::collections::BTreeMap;
use std::ffi::CStr;
use std::fs::OpenOptions;
use std::io::{self, Read, Seek};
use std::os::fd::OwnedFd;
use std::os::unix::fs::OpenOptionsExt;
use std::sync::Arc;

pub struct PinnedImage {
    pub file: Arc<OwnedFd>,
    pub interpreter: Option<Arc<CStr>>,
}

pub struct PinnedProfile {
    pub spec: Profile,
    pub image: PinnedImage,
}

impl PinnedImage {
    pub fn open(profile: &Profile) -> Result<Self, String> {
        let step = |step, error| format!("{step}: {error}");
        let mut source = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY)
            .open(&profile.image)
            .map_err(|e| step("open image", e))?;
        if !source
            .metadata()
            .map_err(|e| step("stat image", e))?
            .is_file()
        {
            return Err("image is not a regular file".into());
        }
        let mut file = host::pin(&mut source)?;
        let mut digest = Context::new(&SHA256);
        let mut bytes = [0; 16384];
        loop {
            let len = match file.read(&mut bytes) {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                read => read.map_err(|e| step("read sealed image", e))?,
            };
            if len == 0 {
                break;
            }
            digest.update(&bytes[..len]);
        }
        if digest.finish().as_ref() != profile.pin {
            return Err("sealed image SHA-256 does not match the pin".into());
        }
        file.rewind().map_err(|e| step("rewind sealed image", e))?;
        let interpreter =
            host::interpreter(&mut file).map_err(|e| step("parse sealed image", e))?;
        file.rewind().map_err(|e| step("rewind sealed image", e))?;
        Ok(Self {
            file: Arc::new(file.into()),
            interpreter,
        })
    }
}

pub fn probe_seals() -> io::Result<()> {
    host::probe()
}

impl Policy {
    pub fn pin_images(self) -> Result<(BTreeMap<String, PinnedProfile>, Vec<String>), String> {
        let mut profiles = BTreeMap::new();
        let mut lines = Vec::new();
        for (name, spec) in self.profiles {
            let image =
                PinnedImage::open(&spec).map_err(|cause| format!("Exec[{name:?}]: {cause}"))?;
            let field = |s: &str| s.escape_default().to_string();
            lines.push(format!(
                "exec: {}: image {}, sha256 {}, interpreter {}, policy {}\n",
                field(&name),
                field(&spec.image),
                hex(&spec.pin),
                image.interpreter.as_deref().map_or_else(
                    || "none".to_owned(),
                    |path| path
                        .to_bytes()
                        .iter()
                        .flat_map(|&byte| std::ascii::escape_default(byte))
                        .map(char::from)
                        .collect(),
                ),
                hex(&self.digest),
            ));
            profiles.insert(name, PinnedProfile { spec, image });
        }
        Ok((profiles, lines))
    }
}

pub fn hex(bytes: &[u8; 32]) -> String {
    use std::fmt::Write;
    let mut text = String::with_capacity(64);
    for byte in bytes {
        write!(text, "{byte:02x}").unwrap();
    }
    text
}
