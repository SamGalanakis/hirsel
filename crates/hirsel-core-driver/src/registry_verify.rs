//! Offline authentication of registry log objects. Transport and persistence live with the caller.

// Slices H/I supply the production callers.
#![allow(dead_code)]

pub(crate) mod notes;
pub(crate) mod status;
mod tiles;

#[cfg(test)]
mod laws;

use crate::package_content::{Id, Root, parse_digest};
use crate::package_manifest::name::{Version, scoped};
use notes::{Checkpoint, Log};
use serde_json::Value;
use std::fmt;

pub(crate) type Hash = [u8; 32];

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Error {
    Trust,
    Note,
    Origin,
    Signature,
    WitnessQuorum,
    WitnessTime,
    Conflict { prior: Vec<u8>, candidate: Vec<u8> },
    MissingObject,
    Tile,
    Bundle,
    MerkleRoot,
    Entry,
    ReleaseClaim,
    StatusTransition,
    MissingEvidence,
    Advisory,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Trust => "registry trust policy mismatch",
            Self::Note => "invalid registry checkpoint note",
            Self::Origin => "registry checkpoint origin mismatch",
            Self::Signature => "invalid registry checkpoint signature",
            Self::WitnessQuorum => "registry checkpoint is missing a pinned signature",
            Self::WitnessTime => {
                "registry checkpoint witness timestamp outside the freshness window"
            }
            Self::Conflict { .. } => "registry checkpoint does not extend the trusted head",
            Self::MissingObject => "required registry log object unavailable",
            Self::Tile => "invalid registry hash tile",
            Self::Bundle => "invalid registry entry bundle",
            Self::MerkleRoot => "registry objects do not match the checkpoint root",
            Self::Entry => "invalid registry log entry",
            Self::ReleaseClaim => "registry release entry does not match the request",
            Self::StatusTransition => "invalid registry status transition",
            Self::MissingEvidence => "required registry status evidence unavailable",
            Self::Advisory => "invalid registry advisory evidence",
        })
    }
}

impl std::error::Error for Error {}

#[derive(Clone, Copy)]
pub(crate) struct ReleaseClaim<'a> {
    pub(crate) identity: &'a str,
    pub(crate) version: &'a str,
    pub(crate) root: Root,
    pub(crate) facts: Option<Hash>,
}

#[derive(Debug)]
pub(crate) struct Release {
    authority: String,
    identity: Id,
    version: Version,
    root: Root,
    facts: Hash,
    time: u64,
}

impl Release {
    pub(crate) fn id(&self) -> &Id {
        &self.identity
    }

    pub(crate) fn version(&self) -> &Version {
        &self.version
    }

    pub(crate) fn root(&self) -> Root {
        self.root
    }

    pub(crate) fn facts(&self) -> Hash {
        self.facts
    }

    pub(crate) fn time(&self) -> u64 {
        self.time
    }
}

impl Checkpoint {
    /// Reads a release bundle and its proof from paths relative to the trusted log.
    pub(crate) fn release(
        &self,
        index: u64,
        claim: ReleaseClaim<'_>,
        mut read: impl FnMut(&str) -> Result<Vec<u8>, Error>,
    ) -> Result<Release, Error> {
        if self.log() != Log::Release {
            return Err(Error::Origin);
        }
        let entry = tiles::included_entry(self.size(), self.root(), index, &mut read)?;
        let row = entry_json(&entry)?;
        let [
            tag,
            identity,
            version,
            root,
            description,
            source,
            time,
            generation,
            compiler,
            facts,
            auth,
        ] = row.as_array().map(Vec::as_slice).ok_or(Error::Entry)?
        else {
            return Err(Error::Entry);
        };
        let identity = identity.as_str().ok_or(Error::Entry)?;
        let (name, major) = identity.rsplit_once('@').ok_or(Error::Entry)?;
        let version_text = version.as_str().ok_or(Error::Entry)?;
        let version = Version::parse(version_text).ok_or(Error::Entry)?;
        let root = Root(json_digest(root).ok_or(Error::Entry)?);
        let facts = json_digest(facts).ok_or(Error::Entry)?;
        let time = time.as_str().and_then(decimal).ok_or(Error::Entry)?;
        if tag != "release"
            || !scoped(name)
            || decimal(major) != Some(u64::from(version.major))
            || !description.is_string()
            || !(source.is_null() || source.as_str().is_some_and(git_object_name))
            || generation.as_str().and_then(decimal).is_none_or(|n| n == 0)
            || json_digest(compiler).is_none()
            || !valid_auth(auth)
        {
            return Err(Error::Entry);
        }
        if claim.identity != identity
            || claim.version != version_text
            || claim.root != root
            || claim.facts.is_some_and(|expected| expected != facts)
        {
            return Err(Error::ReleaseClaim);
        }
        Ok(Release {
            authority: self.authority().into(),
            identity: Id {
                name: name.into(),
                major: version.major,
            },
            version,
            root,
            facts,
            time,
        })
    }
}

fn git_object_name(text: &str) -> bool {
    matches!(text.len(), 40 | 64)
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
}

fn decimal(text: &str) -> Option<u64> {
    if text.is_empty()
        || (text.len() > 1 && text.starts_with('0'))
        || !text.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    text.parse().ok()
}

fn json_digest(value: &Value) -> Option<Hash> {
    parse_digest(value.as_str()?)
}

fn entry_json(bytes: &[u8]) -> Result<Value, Error> {
    let entry: Value = serde_json::from_slice(bytes).map_err(|_| Error::Entry)?;
    // Log rows and their nested auth fields contain only arrays, strings and null.
    // For these types serde_json's compact UTF-8 is the RFC 8785 representation.
    if serde_json::to_vec(&entry).map_err(|_| Error::Entry)? != bytes {
        return Err(Error::Entry);
    }
    Ok(entry)
}

fn valid_auth(auth: &Value) -> bool {
    let Some([subject, intent, token, signature]) = auth.as_array().map(Vec::as_slice) else {
        return false;
    };
    if json_digest(intent).is_none() {
        return false;
    }
    match subject.as_array().map(Vec::as_slice) {
        Some([kind, key]) if kind == "ed25519" => {
            canonical_base64(key, 32) && token.is_null() && canonical_base64(signature, 64)
        }
        Some([kind, issuer, id, selector]) if kind == "oidc" => {
            issuer.as_str().is_some_and(|s| s.starts_with("https://"))
                && id.as_str().is_some_and(|s| {
                    s.strip_prefix("owner/")
                        .or_else(|| s.strip_prefix("repo/"))
                        .and_then(decimal)
                        .is_some_and(|id| id > 0)
                })
                && selector.as_str().is_some_and(|s| !s.is_empty())
                && json_digest(token).is_some()
                && signature.is_null()
        }
        _ => false,
    }
}

fn canonical_base64(value: &Value, length: usize) -> bool {
    value
        .as_str()
        .and_then(decode_base64)
        .is_some_and(|bytes| bytes.len() == length)
}

fn decode_base64(text: &str) -> Option<Vec<u8>> {
    fn digit(byte: u8) -> Option<u32> {
        Some(u32::from(match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        }))
    }

    let blocks = text.as_bytes().as_chunks::<4>();
    if !blocks.1.is_empty() {
        return None;
    }
    let mut output = Vec::with_capacity(blocks.0.len() * 3);
    for (i, &[a, b, c, d]) in blocks.0.iter().enumerate() {
        let leading = digit(a)? << 18 | digit(b)? << 12;
        let value = match (c, d) {
            (b'=', b'=') if i + 1 == blocks.0.len() && leading & 0xffff == 0 => {
                output.push((leading >> 16) as u8);
                continue;
            }
            (c, b'=') if i + 1 == blocks.0.len() => {
                let value = leading | digit(c)? << 6;
                if value & 0xff != 0 {
                    return None;
                }
                output.extend_from_slice(&value.to_be_bytes()[1..3]);
                continue;
            }
            (c, d) => leading | digit(c)? << 6 | digit(d)?,
        };
        output.extend_from_slice(&value.to_be_bytes()[1..]);
    }
    Some(output)
}
