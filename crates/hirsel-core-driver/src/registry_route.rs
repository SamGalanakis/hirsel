//! Local trust and longest-match routing for scoped packages.

use crate::package_manifest::name::scoped;
use crate::registry_verify::notes::{Log, Trust, Witnesses};
use crate::registry_verify::{Hash, decode_base64};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use ureq::http::Uri;

#[cfg(test)]
pub(crate) mod laws;

/// An HTTPS origin, with the same authority spelling used in checkpoint origins.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct Origin(String);

impl Origin {
    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        let invalid = || format!("{text:?} is not an HTTPS registry origin");
        let uri = text.parse::<Uri>().map_err(|_| invalid())?;
        let authority = uri.authority().ok_or_else(invalid)?;
        let host = authority.host().to_ascii_lowercase();
        let valid_host = if let Some(literal) = host.strip_prefix('[') {
            literal
                .strip_suffix(']')
                .is_some_and(|ip| ip.parse::<std::net::Ipv6Addr>().is_ok())
        } else {
            !host.is_empty()
                && host.len() <= 253
                && host.split('.').all(|label| {
                    !label.is_empty()
                        && label.len() <= 63
                        && !label.starts_with('-')
                        && !label.ends_with('-')
                        && label
                            .bytes()
                            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                })
        };
        let port = authority.port_u16();
        if uri.scheme_str() != Some("https")
            || uri.path() != "/"
            || uri.query().is_some()
            || text.contains(['@', '#', '\\'])
            || !valid_host
            || port == Some(0)
            || (authority.as_str().len() != host.len() && port.is_none())
        {
            return Err(invalid());
        }
        let authority = match port {
            None | Some(443) => host,
            Some(port) => format!("{host}:{port}"),
        };
        Ok(Self(format!("https://{authority}")))
    }

    fn authority(&self) -> &str {
        &self.0["https://".len()..]
    }
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone)]
pub(crate) struct Route {
    origin: Origin,
    release: Trust,
    status: Trust,
    legal: Hash,
    digest: Hash,
    preview: bool,
}

impl Route {
    pub(crate) fn origin(&self) -> &Origin {
        &self.origin
    }

    pub(crate) fn trust(&self, log: Log) -> &Trust {
        match log {
            Log::Release => &self.release,
            Log::Status => &self.status,
        }
    }

    pub(crate) fn legal_key(&self) -> Hash {
        self.legal
    }

    pub(crate) fn trust_digest(&self) -> Hash {
        self.digest
    }

    pub(crate) fn preview(&self) -> bool {
        self.preview
    }
}

#[derive(Deserialize, Default)]
struct Config {
    #[serde(default)]
    registry: Registry,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Registry {
    #[serde(default)]
    route: Vec<Pin>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct Pin {
    prefix: String,
    url: String,
    release_key: String,
    status_key: String,
    legal_key: String,
    witness_keys: Vec<String>,
    quorum: u8,
}

const PUBLIC_ORIGIN: &str = "https://pkg.hirsel.ai";

impl Pin {
    fn trust(&mut self, origin: Origin) -> Result<Route, String> {
        if origin.0 == PUBLIC_ORIGIN && self.quorum == 0 {
            return Err(format!(
                "{PUBLIC_ORIGIN} requires two independent witness operators; configure quorum-zero preview on a separate registry origin"
            ));
        }
        self.witness_keys.sort_unstable();
        let operator = |key: &str| {
            key.split_once('+')
                .map_or("", |(name, _)| name.split('/').next().unwrap_or(""))
                .to_ascii_lowercase()
        };
        let operators = match (self.quorum, self.witness_keys.as_slice()) {
            (0, []) => None,
            (2, [a, b]) => Some([(operator(a), a), (operator(b), b)]),
            _ => {
                return Err(
                    "expected quorum 2 with two witnesses, or preview quorum 0 with none".into(),
                );
            }
        };
        let trust = |log, key| {
            let witnesses = match &operators {
                None => Witnesses::Preview,
                Some([(a, a_key), (b, b_key)]) => Witnesses::Pinned([(a, a_key), (b, b_key)]),
            };
            Trust::new(origin.authority(), log, key, witnesses).map_err(|error| error.to_string())
        };
        let release = trust(Log::Release, &self.release_key)?;
        let status = trust(Log::Status, &self.status_key)?;
        let legal = decode_base64(&self.legal_key)
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or("legal-key must be a base64 Ed25519 public key")?;
        let mut digest = Sha256::new();
        digest.update(b"hirsel.registry-trust.v1");
        for field in [&self.release_key, &self.status_key, &self.legal_key]
            .into_iter()
            .chain(&self.witness_keys)
        {
            digest.update((field.len() as u64).to_be_bytes());
            digest.update(field);
        }
        digest.update([self.quorum]);
        let digest = digest.finalize().into();
        Ok(Route {
            origin,
            release: release.in_namespace(digest),
            status: status.in_namespace(digest),
            legal,
            digest,
            preview: self.quorum == 0,
        })
    }
}

#[derive(Default)]
pub(crate) struct Routes(BTreeMap<String, Arc<Route>>);

impl Routes {
    pub(crate) fn load() -> Result<Self, String> {
        let directory = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .filter(|path| path.is_absolute())
                    .map(|path| path.join(".config"))
            });
        directory.map_or_else(
            || Ok(Self::default()),
            |dir| Self::read(&dir.join("hirsel/config.toml")),
        )
    }

    fn read(path: &Path) -> Result<Self, String> {
        let data = match std::fs::read_to_string(path) {
            Ok(data) => data,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(format!("{}: {error}", path.display())),
        };
        Self::parse(&data).map_err(|error| format!("{}: {error}", path.display()))
    }

    pub(crate) fn parse(input: &str) -> Result<Self, String> {
        let config: Config = toml::from_str(input).map_err(|error| error.to_string())?;
        let mut routes = BTreeMap::new();
        let mut origins = BTreeMap::<Origin, Arc<Route>>::new();
        for mut pin in config.registry.route {
            let prefix = if pin.prefix == "*" {
                ""
            } else {
                pin.prefix
                    .strip_suffix("/*")
                    .filter(|scope| scoped(&format!("{scope}/x")))
                    .ok_or_else(|| format!("invalid registry prefix {:?}", pin.prefix))?
            }
            .to_owned();
            if routes.contains_key(&prefix) {
                return Err(format!("duplicate registry prefix {:?}", pin.prefix));
            }
            let route = pin
                .trust(Origin::parse(&pin.url)?)
                .map_err(|reason| format!("registry route {:?}: {reason}", pin.prefix))?;
            let route = match origins.entry(route.origin.clone()) {
                std::collections::btree_map::Entry::Occupied(entry) => {
                    if entry.get().digest != route.digest {
                        return Err(format!("contradictory registry trust for {}", route.origin));
                    }
                    entry.get().clone()
                }
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(Arc::new(route)).clone()
                }
            };
            routes.insert(prefix, route);
        }
        Ok(Self(routes))
    }

    pub(crate) fn for_name(&self, name: &str) -> Result<&Route, String> {
        if !scoped(name) {
            return Err(format!("{name:?} is not a canonical scoped package name"));
        }
        let (scope, _) = name.rsplit_once('/').unwrap();
        let host = scope.split('/').next().unwrap();
        self.0
            .get(scope)
            .or_else(|| self.0.get(host))
            .or_else(|| self.0.get(""))
            .map(AsRef::as_ref)
            .ok_or_else(|| format!("no registry route for {name}: {PUBLIC_ORIGIN} has no shipped independent witness pins yet; configure [[registry.route]] in hirsel/config.toml for your registry"))
    }

    pub(crate) fn for_pin(&self, name: &str, origin: &Origin) -> Result<&Route, String> {
        let selected = self.for_name(name)?;
        if selected.origin() != origin {
            return Err(format!(
                "{name}: registry source is pinned to {origin}, but the user route selects {}; change the source explicitly",
                selected.origin()
            ));
        }
        Ok(selected)
    }
}
