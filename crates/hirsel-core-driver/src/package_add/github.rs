//! `github:OWNER/REPO[/PACKAGE][@TAG]`: a release asset named by tag.

use super::Failure;
use crate::package_store::{fetch_https, https_agent};

const HOST: &str = "https://github.com";

pub(super) struct Reference {
    owner: String,
    repo: String,
    package: String,
    tag: Option<String>,
}

fn name(part: &str) -> bool {
    !part.is_empty()
        && part != "."
        && part != ".."
        && part
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

/// One path component: a non-empty tag that is not a dot segment and holds no `/`.
fn component(tag: &str) -> bool {
    !tag.is_empty() && tag != "." && tag != ".." && !tag.contains('/')
}

fn encode(tag: &str) -> String {
    let mut out = String::new();
    for byte in tag.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = std::str::from_utf8(bytes.get(index + 1..index + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

impl Reference {
    pub(super) fn parse(text: &str) -> Result<Self, Failure> {
        let usage = || -> Failure {
            format!("expected github:OWNER/REPO[/PACKAGE][@TAG], found github:{text}").into()
        };
        let (path, tag) = match text.split_once('@') {
            Some((path, tag)) => (path, Some(tag)),
            None => (text, None),
        };
        let parts: Vec<&str> = path.split('/').collect();
        let (owner, repo, package) = match parts[..] {
            [owner, repo] => (owner, repo, repo),
            [owner, repo, package] => (owner, repo, package),
            _ => return Err(usage()),
        };
        if !(name(owner) && name(repo) && name(package)) {
            return Err(usage());
        }
        if let Some(tag) = tag
            && !component(tag)
        {
            return Err(format!("github: tag {tag:?} must be one non-empty path component").into());
        }
        Ok(Self {
            owner: owner.into(),
            repo: repo.into(),
            package: package.into(),
            tag: tag.map(Into::into),
        })
    }

    fn download(&self, tag: &str) -> String {
        format!(
            "{HOST}/{}/{}/releases/download/{}/{}.hpkg",
            self.owner,
            self.repo,
            encode(tag),
            self.package
        )
    }

    /// The canonical locator: the tagged download URL, looking up `latest` when untagged.
    pub(super) fn locator(&self) -> Result<String, Failure> {
        if let Some(tag) = &self.tag {
            return Ok(self.download(tag));
        }
        let latest = format!("{HOST}/{}/{}/releases/latest", self.owner, self.repo);
        let response = https_agent(false)
            .get(&latest)
            .call()
            .map_err(|error| format!("no latest release at {latest}: {error}"))?;
        let location = response
            .headers()
            .get("location")
            .filter(|_| response.status().is_redirection())
            .ok_or_else(|| {
                format!(
                    "no latest release at {latest}: http status {}",
                    response.status().as_u16()
                )
            })?;
        let location = String::from_utf8_lossy(location.as_bytes()).into_owned();
        let prefix = format!("{HOST}/{}/{}/releases/tag/", self.owner, self.repo);
        location
            .strip_prefix(&prefix)
            .filter(|rest| !rest.contains(['/', '?', '#']))
            .and_then(decode)
            .filter(|tag| component(tag))
            .map(|tag| self.download(&tag))
            .ok_or_else(|| format!("unexpected latest redirect {location}").into())
    }

    pub(super) fn fetch(&self, url: &str) -> Result<Vec<u8>, Failure> {
        fetch_https(url).map_err(|error| {
            if matches!(
                error.downcast_ref::<ureq::Error>(),
                Some(ureq::Error::StatusCode(404))
            ) {
                format!("no release asset {}.hpkg at {url}", self.package).into()
            } else {
                format!("fetching {url} failed: {error}").into()
            }
        })
    }
}
