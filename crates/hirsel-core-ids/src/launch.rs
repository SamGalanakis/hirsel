//! Owned, immutable metadata for one launch (docs/spec/launch.md).
use sha2::{Digest, Sha256};
use std::io::{self, Read};
use std::path::Path;

pub const DEFAULT_N_PLACE: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Executable {
    Digest([u8; 32]),
    Unavailable(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Metadata {
    executable: Executable,
    target: String,
    n_place: u64,
    describe: bool,
}

impl Metadata {
    pub fn native(target: &str, n_place: u64, describe: bool) -> Self {
        Self::artifact(Path::new("/proc/self/exe"), target, n_place, describe)
    }

    pub fn artifact(path: &Path, target: &str, n_place: u64, describe: bool) -> Self {
        let executable = match std::fs::File::open(path).and_then(hash) {
            Ok(digest) => Executable::Digest(digest),
            Err(error) => Executable::Unavailable(error.to_string()),
        };
        Self {
            executable,
            target: target.into(),
            n_place,
            describe,
        }
    }

    /// The evaluator already owns the exact image it loaded; no pathname reopen.
    pub fn loaded(image: &[u8], target: &str, n_place: u64, describe: bool) -> Self {
        Self::embedded(
            Some(Sha256::digest(image).into()),
            target,
            n_place,
            describe,
        )
    }

    pub fn embedded(digest: Option<[u8; 32]>, target: &str, n_place: u64, describe: bool) -> Self {
        Self {
            executable: digest
                .map(Executable::Digest)
                .unwrap_or_else(|| Executable::Unavailable("no executable image supplied".into())),
            target: target.into(),
            n_place,
            describe,
        }
    }

    pub fn source(target: &str, n_place: u64, describe: bool) -> Self {
        Self::embedded(None, target, n_place, describe)
    }

    pub fn digest(&self) -> &[u8] {
        match &self.executable {
            Executable::Digest(bytes) => bytes,
            Executable::Unavailable(_) => &[],
        }
    }
    pub fn unavailable(&self) -> Option<&str> {
        match &self.executable {
            Executable::Digest(_) => None,
            Executable::Unavailable(reason) => Some(reason),
        }
    }
    pub fn target(&self) -> &str {
        &self.target
    }
    pub fn n_place(&self) -> u64 {
        self.n_place
    }
    pub fn is_describe(&self) -> bool {
        self.describe
    }

    /// Private runner transport: scalars and bytes, without program allocation.
    pub fn word(&self, field: u64, index: u64) -> u64 {
        let byte = |bytes: &[u8]| {
            usize::try_from(index)
                .ok()
                .and_then(|i| bytes.get(i))
                .copied()
                .map(u64::from)
                .unwrap_or(0)
        };
        match field {
            0 => self.n_place,
            1 => self.digest().len() as u64,
            2 => byte(self.digest()),
            3 => self.target.len() as u64,
            4 => byte(self.target.as_bytes()),
            5 => u64::from(self.describe),
            _ => 0,
        }
    }
}

fn hash(mut image: std::fs::File) -> io::Result<[u8; 32]> {
    let mut hash = Sha256::new();
    let mut scratch = [0; 8192];
    loop {
        let count = image.read(&mut scratch)?;
        if count == 0 {
            return Ok(hash.finalize().into());
        }
        hash.update(&scratch[..count]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identity_unavailable() {
        let missing = std::env::temp_dir().join(format!("hirsel-no-image-{}", std::process::id()));
        let launch = Metadata::artifact(&missing, "aarch64-unknown-linux-gnu", 73, false);
        assert!(launch.digest().is_empty());
        assert!(launch.unavailable().is_some());
        assert_eq!(launch.n_place(), 73);
        assert_eq!(launch.target(), "aarch64-unknown-linux-gnu");
        assert!(Metadata::source("test", 1, true).digest().is_empty());
        assert!(
            Metadata::embedded(None, "test", 1, false)
                .digest()
                .is_empty()
        );
    }

    #[test]
    fn artifact_identity_and_embedder_inputs_are_owned() {
        let path = std::env::temp_dir().join(format!("hirsel-launch-image-{}", std::process::id()));
        std::fs::write(&path, b"abc").unwrap();
        let launch = Metadata::artifact(&path, "target", 419, true);
        // FIPS 180-4 / NIST SHA-256 abc vector, no identity-domain framing.
        assert_eq!(
            launch.digest(),
            &[
                0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae,
                0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61,
                0xf2, 0x00, 0x15, 0xad
            ]
        );
        std::fs::write(&path, b"changed").unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(launch.digest()[0], 0xba);
        let mut digest = [7; 32];
        let mut target = "target".to_owned();
        let embedded = Metadata::embedded(Some(digest), &target, 419, true);
        digest.fill(8);
        target.clear();
        assert_eq!(embedded.digest(), &[7; 32]);
        assert_eq!(embedded.target(), "target");
        assert_eq!(embedded.word(0, 0), 419);
        assert_eq!(embedded.word(5, 0), 1);
    }
}
