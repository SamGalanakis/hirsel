use crate::SecretPolicy;
use std::net::{Ipv4Addr, SocketAddrV4};

/// The public part of a `Cred` grant (TLS-7): the `main` parameter it names,
/// its credential source and the canonical policy of every key fetched from it.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct CredGrant {
    pub parameter: String,
    pub source: String,
    /// The canonical policy text.
    pub policy: String,
}

/// A credential source the runtime can contact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CredSource {
    /// IMDSv2 at this address: `imds` is `169.254.169.254:80`.
    Imds(SocketAddrV4),
}

/// The grant names a source that has no provider yet.
pub const STS_UNBUILT: &str = "a Cred grant's sts source is not built yet; use imds";

impl CredSource {
    pub fn parse(source: &str) -> Result<Self, &'static str> {
        if source == "imds" {
            return Ok(Self::Imds(SocketAddrV4::new(
                Ipv4Addr::new(169, 254, 169, 254),
                80,
            )));
        }
        if let Some(at) = source.strip_prefix("imds:") {
            let (ip, port) = at.rsplit_once(':').ok_or(CredGrant::SYNTAX)?;
            let ip: Ipv4Addr = ip.parse().map_err(|_| CredGrant::SYNTAX)?;
            let port: u16 = port.parse().map_err(|_| CredGrant::SYNTAX)?;
            // One spelling per address, so the grant record is canonical.
            if port == 0 || format!("{ip}:{port}") != at {
                return Err(CredGrant::SYNTAX);
            }
            return Ok(Self::Imds(SocketAddrV4::new(ip, port)));
        }
        if source.starts_with("sts:") {
            return Err(STS_UNBUILT);
        }
        Err(CredGrant::SYNTAX)
    }
}

impl CredGrant {
    pub const SYNTAX: &str = "expected --grant 'Cred:<parameter>=imds|imds:<ipv4>:<port> <uses>', <uses> being the policy of each fetched secret key in a Secret grant's spelling";

    /// `Cred:<parameter>=<source> <policy>`: the policy is the longest
    /// space-separated suffix that parses, as for a Secret grant.
    pub fn parse(spec: &str) -> Result<Self, &'static str> {
        let (parameter, value) = spec
            .strip_prefix("Cred:")
            .and_then(|rest| rest.split_once('='))
            .ok_or(Self::SYNTAX)?;
        let (source, policy) = value
            .match_indices(' ')
            .find_map(|(at, _)| Some((&value[..at], SecretPolicy::cli(&value[at + 1..])?)))
            .ok_or(Self::SYNTAX)?;
        Self::new(parameter, source, &policy.text())
    }

    pub fn new(parameter: &str, source: &str, policy: &str) -> Result<Self, &'static str> {
        if parameter.is_empty() {
            return Err(Self::SYNTAX);
        }
        CredSource::parse(source)?;
        Ok(Self {
            parameter: parameter.to_owned(),
            source: source.to_owned(),
            policy: SecretPolicy::parse(policy).ok_or(Self::SYNTAX)?.text(),
        })
    }
}
