//! Object ids (GIT-9 step 2). Every id the tool computes goes through [`Hasher`], so the
//! SHA-1 collision detection of Git's own `sha1dc` applies to every object.

use std::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Format {
    Sha1,
    Sha256,
}

impl Format {
    /// The format a full commit id names: 40 hex digits for SHA-1, 64 for SHA-256.
    pub(crate) fn of_rev(rev: &str) -> Option<Self> {
        match rev.len() {
            40 => Some(Self::Sha1),
            64 => Some(Self::Sha256),
            _ => None,
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Sha1 => "sha1",
            Self::Sha256 => "sha256",
        }
    }

    pub(crate) fn len(self) -> usize {
        match self {
            Self::Sha1 => 20,
            Self::Sha256 => 32,
        }
    }
}

#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct Oid {
    bytes: [u8; 32],
    len: u8,
}

impl Oid {
    pub(crate) fn from_bytes(bytes: &[u8]) -> Self {
        let mut id = Self {
            bytes: [0; 32],
            len: bytes.len() as u8,
        };
        id.bytes[..bytes.len()].copy_from_slice(bytes);
        id
    }

    /// A full lower-case hex id of either format.
    pub(crate) fn parse(text: &str) -> Option<Self> {
        if Format::of_rev(text).is_none()
            || !text
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return None;
        }
        let bytes: Vec<u8> = (0..text.len() / 2)
            .map(|i| u8::from_str_radix(&text[2 * i..2 * i + 2], 16).unwrap())
            .collect();
        Some(Self::from_bytes(&bytes))
    }

    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len as usize]
    }
}

impl fmt::Display for Oid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.as_bytes() {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for Oid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// The one hash function every object id and pack checksum goes through.
pub(crate) enum Hasher {
    Sha1(Box<sha1_checked::Sha1>),
    Sha256(sha2::Sha256),
}

/// A digest whose input the SHA-1 collision detector flagged as an attack.
pub(crate) struct Collision(pub(crate) Oid);

impl Hasher {
    pub(crate) fn new(format: Format) -> Self {
        use sha2::Digest;
        match format {
            Format::Sha1 => Self::Sha1(Box::default()),
            Format::Sha256 => Self::Sha256(sha2::Sha256::new()),
        }
    }

    pub(crate) fn object(format: Format, kind: &str, size: u64) -> Self {
        let mut hasher = Self::new(format);
        hasher.update(format!("{kind} {size}\0").as_bytes());
        hasher
    }

    pub(crate) fn update(&mut self, bytes: &[u8]) {
        match self {
            Self::Sha1(hash) => sha1_checked::Digest::update(hash.as_mut(), bytes),
            Self::Sha256(hash) => sha2::Digest::update(hash, bytes),
        }
    }

    pub(crate) fn finish(self) -> Result<Oid, Collision> {
        match self {
            Self::Sha1(hash) => {
                let result = hash.try_finalize();
                let id = Oid::from_bytes(result.hash().as_slice());
                if result.has_collision() {
                    Err(Collision(id))
                } else {
                    Ok(id)
                }
            }
            Self::Sha256(hash) => Ok(Oid::from_bytes(sha2::Digest::finalize(hash).as_slice())),
        }
    }
}

/// A Git object's id: the hash of `<kind> <size>\0` and its bytes.
pub(crate) fn object_id(format: Format, kind: &str, bytes: &[u8]) -> Result<Oid, Collision> {
    let mut hasher = Hasher::object(format, kind, bytes.len() as u64);
    hasher.update(bytes);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::{Format, Hasher};

    /// The first 320 bytes of the published `shattered-1.pdf` (whole file SHA-1
    /// 38762cf7f55934b34d179ae6a4c80cadccbb7f0a), which hold its two collision blocks.
    const SHATTERED_PREFIX: &str = concat!(
        "255044462d312e330a25e2e3cfd30a0a0a312030206f626a0a3c3c2f5769",
        "6474682032203020522f4865696768742033203020522f54797065203420",
        "3020522f537562747970652035203020522f46696c746572203620302052",
        "2f436f6c6f7253706163652037203020522f4c656e677468203820302052",
        "2f42697473506572436f6d706f6e656e7420383e3e0a73747265616d0aff",
        "d8fffe00245348412d3120697320646561642121212121852fec09233975",
        "9c39b1a1c63c4c97e1fffe017346dc9166b67e118f029ab621b2560ff9ca",
        "67cca8c7f85ba84c79030c2b3de218f86db3a90901d5df45c14f26fedfb3",
        "dc38e96ac22fe7bd728f0e45bce046d23c570feb141398bb552ef5a0a82b",
        "e331fea48037b8b5d71f0e332edf93ac3500eb4ddc0decc1a864790c782c",
        "76215660dd309791d06bd0af3f98cda4bc4629b1",
    );

    // bug: an id computed without collision detection, so a SHAttered pair is two objects
    // under one id. oracle: the published SHAttered attack (Stevens et al. 2017).
    #[test]
    fn package_git_hash_flags_shattered() {
        let bytes: Vec<u8> = (0..SHATTERED_PREFIX.len() / 2)
            .map(|i| u8::from_str_radix(&SHATTERED_PREFIX[2 * i..2 * i + 2], 16).unwrap())
            .collect();
        assert_eq!(bytes.len(), 320);
        let mut hasher = Hasher::new(Format::Sha1);
        hasher.update(&bytes);
        assert!(hasher.finish().is_err(), "shattered-1.pdf was not flagged");
    }
}
