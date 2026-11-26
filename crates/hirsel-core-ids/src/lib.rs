//! Neutral identities shared by every compiler path: content hashes, package
//! identities and definition paths (PROG-1, PROG-5). Nothing here knows syntax,
//! types or lowered code, so old and new front ends, storage and native code can
//! name the same definition without depending on each other.

pub mod dir_port;
pub mod encoding;
pub mod host_op;
pub mod launch;
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
    /// Hash length-framed parts supplied directly to one streaming hasher.
    ///
    /// `part_with` is for a logical part whose bytes are naturally produced in
    /// several borrowed chunks. It keeps the same framing as `of_parts` without
    /// requiring a temporary concatenation buffer.
    #[inline]
    pub fn of_parts_stream(
        domain: &str,
        emit: impl FnOnce(&mut PartsHasher) -> std::io::Result<()>,
    ) -> std::io::Result<Self> {
        let mut hasher = PartsHasher {
            hash: Sha256::new(),
        };
        hasher.part(domain.as_bytes());
        emit(&mut hasher)?;
        Ok(Self(hasher.hash.finalize().into()))
    }

    pub fn of_parts<'a>(domain: &'a str, parts: impl IntoIterator<Item = &'a [u8]>) -> Self {
        Self::of_parts_stream(domain, |hasher| {
            for part in parts {
                hasher.part(part);
            }
            Ok(())
        })
        .expect("content hash stream")
    }

    /// The hash of `value`'s encoding (the encoding the store hashes IR with) as
    /// the one part under `domain`.
    pub fn of_encoded(domain: &str, value: &(impl serde::Serialize + ?Sized)) -> Self {
        let mut bytes = Vec::with_capacity(64);
        encoding::write_fixed(&mut bytes, value).expect("content hash encoding");
        Self::of_parts(domain, [&bytes[..]])
    }
}

/// Builder for the length-framed parts consumed by [`ContentHash::of_parts_stream`].
pub struct PartsHasher {
    hash: Sha256,
}

impl PartsHasher {
    #[inline]
    fn part(&mut self, bytes: &[u8]) {
        self.hash.update((bytes.len() as u64).to_le_bytes());
        self.hash.update(bytes);
    }

    #[inline]
    pub fn part_byte_slice(&mut self, first: u8, rest: &[u8]) {
        let length = (rest.len() + 1) as u64;
        self.hash.update(length.to_le_bytes());
        self.hash.update([first]);
        self.hash.update(rest);
    }

    /// Append parts already framed as [`Self::part_byte_slice`] frames them:
    /// each a little-endian `u64` length, a first byte and the rest. A syntax
    /// tree's canonical token stream is framed this way.
    #[inline]
    pub fn framed_parts(&mut self, bytes: &[u8]) {
        self.hash.update(bytes);
    }

    #[inline]
    pub fn part_with(
        &mut self,
        length: u64,
        emit: impl FnOnce(&mut dyn std::io::Write) -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        self.hash.update(length.to_le_bytes());
        let mut writer = PartWriter {
            hash: &mut self.hash,
            remaining: length,
        };
        emit(&mut writer)?;
        if writer.remaining != 0 {
            return Err(std::io::Error::other("content hash part is short"));
        }
        Ok(())
    }
}

struct PartWriter<'a> {
    hash: &'a mut Sha256,
    remaining: u64,
}

impl std::io::Write for PartWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.remaining = self
            .remaining
            .checked_sub(bytes.len() as u64)
            .ok_or_else(|| std::io::Error::other("content hash part is long"))?;
        self.hash.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
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

    /// The toolchain's runner package, which alone binds the private launch
    /// transport and reads the place-only launch words (launch.md, "Private
    /// prefix"). `supplied_by_toolchain` is where the package came from: a
    /// package a manifest or archive names `runner@1` is not the runner.
    pub fn is_toolchain_runner(&self, supplied_by_toolchain: bool) -> bool {
        supplied_by_toolchain && self.package_is("runner", "1")
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
    /// PROG-9's definition text, shared by PROG-10's instance encoding.
    pub fn definition_identity(&self) -> String {
        use std::fmt::Write;
        fn escape(part: &str, out: &mut String) {
            for &byte in part.as_bytes() {
                if byte.is_ascii_alphanumeric() || byte == b'_' {
                    out.push(byte as char);
                } else {
                    write!(out, "%{byte:02X}").expect("writing to a String");
                }
            }
        }
        let path = &self.0;
        let mut text = String::new();
        escape(&path.package.name, &mut text);
        text.push('@');
        text.push_str(&path.package.version);
        for component in path.modules.iter().chain(std::iter::once(&path.name)) {
            text.push('/');
            escape(component, &mut text);
        }
        text
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

    #[test]
    fn legacy_record_wire_vectors() {
        use serde::{Deserialize, Serialize, de::DeserializeOwned};

        fn expect<T: Serialize + DeserializeOwned + std::fmt::Debug + PartialEq>(
            value: T,
            fixed: &[u8],
            compact: &[u8],
        ) {
            let mut out = Vec::new();
            encoding::write_fixed(&mut out, &value).unwrap();
            assert_eq!(out, fixed);
            assert_eq!(encoding::fixed_size(&value).unwrap(), fixed.len() as u64);
            assert_eq!(encoding::read_fixed::<T>(fixed).unwrap(), value);
            out.clear();
            encoding::write_compact(&mut out, &value).unwrap();
            assert_eq!(out, compact);
            assert_eq!(encoding::read_compact::<T>(compact).unwrap(), value);
        }

        // Values through 250 are their own byte. Tags 251 through 254 name
        // the width of the little-endian payload: 16, 32, 64 or 128 bits.
        for (value, compact) in [
            (250u128, &[250][..]),
            (251, &[251, 251, 0]),
            (65_535, &[251, 255, 255]),
            (65_536, &[252, 0, 0, 1, 0]),
            (4_294_967_295, &[252, 255, 255, 255, 255]),
            (4_294_967_296, &[253, 0, 0, 0, 0, 1, 0, 0, 0]),
            (
                18_446_744_073_709_551_616,
                &[254, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0],
            ),
        ] {
            expect(value, &value.to_le_bytes(), compact);
        }
        // Single-byte integers are raw. Wider signed integers use zigzag in compact mode.
        expect(
            (255u8, -1i8, -126i16),
            &[255, 255, 130, 255],
            &[255, 255, 251, 251, 0],
        );

        #[derive(Debug, PartialEq, Serialize, Deserialize)]
        enum Field {
            Empty,
            Number(u16),
        }
        // Option uses a one-byte tag. The enum index is u32, and Vec length is u64.
        expect(None::<Field>, &[0], &[0]);
        expect(
            Some(Field::Number(251)),
            &[1, 1, 0, 0, 0, 251, 0],
            &[1, 1, 251, 251, 0],
        );
        expect(vec![4u8, 5], &[2, 0, 0, 0, 0, 0, 0, 0, 4, 5], &[2, 4, 5]);

        assert!(encoding::read_compact::<u64>(&[255]).is_err());
        assert!(encoding::read_compact::<u64>(&[251, 251]).is_err());
        assert!(encoding::read_compact::<u16>(&[252, 0, 0, 1, 0]).is_err());
        assert!(encoding::read_fixed::<serde::de::IgnoredAny>(&[]).is_err());
        for error in [
            encoding::read_fixed::<u8>(&[42, 0]).unwrap_err(),
            encoding::read_compact::<u8>(&[42, 0]).unwrap_err(),
        ] {
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
            assert_eq!(error.to_string(), "trailing record bytes");
        }
    }
}
