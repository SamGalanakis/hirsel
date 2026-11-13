//! Neutral identities shared by every compiler path: content hashes, package
//! identities and definition paths (PROG-1, PROG-5). Nothing here knows syntax,
//! types or lowered code, so old and new front ends, storage and native code can
//! name the same definition without depending on each other.

pub mod published;

#[cfg(test)]
hirsel_guard::install!();

use sha2::{Digest, Sha256};

/// Stable SHA-256 digest of domain-separated, length-framed inputs (PROG-5).
#[derive(
    serde::Serialize,
    serde::Deserialize,
    Clone,
    Copy,
    Debug,
    Default,
    Eq,
    PartialEq,
    Hash,
    Ord,
    PartialOrd,
)]
pub struct ContentHash(pub [u8; 32]);
impl ContentHash {
    pub fn of_bytes(bytes: &[u8]) -> Self {
        Self::of_parts("hirsel.text.v1", [bytes])
    }
    /// Hash one length-framed canonical stream without retaining encoded bytes.
    /// The caller supplies its exact length; a short or long stream is refused.
    pub fn of_stream(
        domain: &str,
        length: u64,
        emit: impl FnOnce(&mut dyn std::io::Write) -> std::io::Result<()>,
    ) -> std::io::Result<Self> {
        struct Stream {
            hash: Sha256,
            remaining: u64,
        }
        impl std::io::Write for Stream {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.remaining = self
                    .remaining
                    .checked_sub(bytes.len() as u64)
                    .ok_or_else(|| std::io::Error::other("hash stream exceeds its length"))?;
                self.hash.update(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        use hirsel_intern::timings::instructions::{self as counted, Owner, Volume};
        let _scope = counted::scope(Owner::Hash);
        counted::volume(Volume::Hashes, 1);
        counted::volume(Volume::HashedBytes, length);
        let mut hash = Sha256::new();
        hash.update((domain.len() as u64).to_le_bytes());
        hash.update(domain.as_bytes());
        hash.update(length.to_le_bytes());
        let mut stream = Stream {
            hash,
            remaining: length,
        };
        emit(&mut stream)?;
        if stream.remaining != 0 {
            return Err(std::io::Error::other(
                "hash stream is shorter than its length",
            ));
        }
        Ok(Self(stream.hash.finalize().into()))
    }
    pub fn of_parts<'a>(domain: &'a str, parts: impl IntoIterator<Item = &'a [u8]>) -> Self {
        use hirsel_intern::timings::instructions::{self as counted, Owner, Volume};
        let _scope = counted::scope(Owner::Hash);
        let mut bytes = 0;
        let mut hash = Sha256::new();
        for part in std::iter::once(domain.as_bytes()).chain(parts) {
            hash.update((part.len() as u64).to_le_bytes());
            hash.update(part);
            bytes += part.len() as u64;
        }
        counted::volume(Volume::Hashes, 1);
        counted::volume(Volume::HashedBytes, bytes);
        Self(hash.finalize().into())
    }
}
/// Package identity includes version; package names alone do not identify types (PROG-1).
#[derive(
    serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd,
)]
pub struct PackageId {
    pub name: String,
    pub version: String,
}
impl PackageId {
    /// Match only the package name, preserving callers that ignore versions.
    pub fn name_is(&self, name: &str) -> bool {
        self.name == name
    }

    /// Match the exact package name and version.
    pub fn package_is(&self, name: &str, version: &str) -> bool {
        self.name_is(name) && self.version == version
    }
}
/// Canonical name; module components come from the definition manifest, not files (PROG-1).
#[derive(
    serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd,
)]
pub struct ItemPath {
    pub package: PackageId,
    pub modules: Vec<String>,
    pub name: String,
}
/// Stable definition identity, unaffected by source positions, hashes or intern order (PROG-5).
#[derive(
    serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd,
)]
pub struct DefPath(pub ItemPath);
impl DefPath {
    /// The reserved core@1 identity, with dot-separated module and item names.
    pub fn core(name: &str) -> Self {
        let mut modules: Vec<_> = name.split('.').map(str::to_owned).collect();
        let name = modules.pop().expect("core name");
        Self(ItemPath {
            package: PackageId {
                name: "core".into(),
                version: "1".into(),
            },
            modules,
            name,
        })
    }
    /// Compare a core@1 identity without constructing or allocating a path.
    pub fn is_core(&self, name: &str) -> bool {
        self.package_is("core", "1")
            && self
                .0
                .modules
                .iter()
                .map(String::as_str)
                .chain(std::iter::once(self.0.name.as_str()))
                .eq(name.split('.'))
    }
    /// Match a complete package identity, independent of modules and item name.
    pub fn package_is(&self, name: &str, version: &str) -> bool {
        self.0.package.package_is(name, version)
    }
    /// Write the canonical spelling directly to an existing text buffer.
    pub fn write_to(&self, out: &mut impl std::fmt::Write) -> std::fmt::Result {
        let item = &self.0;
        out.write_str(&item.package.name)?;
        out.write_str("@")?;
        out.write_str(&item.package.version)?;
        for part in item.modules.iter().chain(std::iter::once(&item.name)) {
            out.write_str("/")?;
            out.write_str(part)?;
        }
        Ok(())
    }
    /// The path a full versioned spelling `package@version/module/.../name` names: the
    /// inverse of `Display`.
    pub fn from_full(path: &str) -> Option<Self> {
        let mut parts = path.split('/');
        let (name, version) = parts.next()?.split_once('@')?;
        let mut modules: Vec<String> = parts.map(str::to_owned).collect();
        let item = modules.pop()?;
        Some(Self(ItemPath {
            package: PackageId {
                name: name.into(),
                version: version.into(),
            },
            modules,
            name: item,
        }))
    }
    pub fn fingerprint(&self) -> ContentHash {
        let p = &self.0;
        let parts = std::iter::once(p.package.name.as_bytes())
            .chain(std::iter::once(p.package.version.as_bytes()))
            .chain(p.modules.iter().map(String::as_bytes))
            .chain(std::iter::once(p.name.as_bytes()));
        ContentHash::of_parts("hirsel.defpath.v1", parts)
    }
}
/// The canonical definition spelling used by semantic entries and proof exports.
impl std::fmt::Display for DefPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.write_to(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write;

    #[test]
    fn canonical_spelling_appends_and_propagates_sink_refusal() {
        let path = DefPath(ItemPath {
            package: PackageId {
                name: "app".into(),
                version: "2".into(),
            },
            modules: vec!["outer".into(), "inner".into()],
            name: "item".into(),
        });
        let expected = "app@2/outer/inner/item";
        let mut text = "prefix:".to_owned();
        path.write_to(&mut text).unwrap();
        assert_eq!(text, format!("prefix:{expected}"));
        assert_eq!(path.to_string(), expected);
        struct Refuse;
        impl Write for Refuse {
            fn write_str(&mut self, _: &str) -> std::fmt::Result {
                Err(std::fmt::Error)
            }
        }
        assert!(path.write_to(&mut Refuse).is_err());
        assert!(write!(&mut Refuse, "{path}").is_err());
    }
}
