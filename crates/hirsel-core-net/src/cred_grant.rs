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
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CredSource {
    /// IMDSv2 at this address: `imds` is `169.254.169.254:80`.
    Imds(SocketAddrV4),
    /// STS `AssumeRoleWithWebIdentity`.
    Sts(StsSource),
}

/// `sts:<role-arn> [at:<name>:<port>] [roots:<pem-path>] file:<token-path>`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StsSource {
    pub role_arn: String,
    /// The endpoint's DNS name, which the TLS certificate must match.
    pub host: String,
    pub port: u16,
    /// A PEM file of trusted roots; `None` trusts the host's roots.
    pub roots: Option<String>,
    /// The web identity token file, read at each fetch.
    pub token_path: String,
}

impl StsSource {
    pub const HOST: &str = "sts.amazonaws.com";
    pub const PORT: u16 = 443;

    /// The one spelling of this source, so a grant record is canonical.
    fn text(&self) -> String {
        let mut text = format!("sts:{}", self.role_arn);
        if (self.host.as_str(), self.port) != (Self::HOST, Self::PORT) {
            text += &format!(" at:{}:{}", self.host, self.port);
        }
        if let Some(roots) = &self.roots {
            text += &format!(" roots:{roots}");
        }
        text + " file:" + &self.token_path
    }

    fn parse(text: &str) -> Option<Self> {
        let (role_arn, mut rest) = text.strip_prefix("sts:")?.split_once(' ')?;
        if !role_arn.starts_with("arn:") || !role_arn.bytes().all(|b| b.is_ascii_graphic()) {
            return None;
        }
        let mut source = Self {
            role_arn: role_arn.to_owned(),
            host: Self::HOST.to_owned(),
            port: Self::PORT,
            roots: None,
            token_path: String::new(),
        };
        if let Some(at) = rest.strip_prefix("at:") {
            let (endpoint, after) = at.split_once(' ')?;
            let (host, port) = endpoint.rsplit_once(':')?;
            let valid_label = |label: &str| {
                !label.is_empty()
                    && label.len() <= 63
                    && !label.starts_with('-')
                    && !label.ends_with('-')
                    && label
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            };
            if host.len() > 253 || !host.split('.').all(valid_label) {
                return None;
            }
            source.host = host.to_ascii_lowercase();
            source.port = port.parse().ok().filter(|&p| p != 0)?;
            rest = after;
        }
        if let Some(roots) = rest.strip_prefix("roots:") {
            let (roots, after) = roots.split_once(' ')?;
            if roots.is_empty() {
                return None;
            }
            source.roots = Some(roots.to_owned());
            rest = after;
        }
        source.token_path = rest
            .strip_prefix("file:")
            .filter(|p| !p.is_empty())?
            .to_owned();
        (source.text() == text).then_some(source)
    }
}

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
        StsSource::parse(source)
            .map(Self::Sts)
            .ok_or(CredGrant::SYNTAX)
    }
}

impl CredGrant {
    pub const SYNTAX: &str = "expected --grant 'Cred:<parameter>=<source> <uses>', <source> being imds, imds:<ipv4>:<port> or sts:<role-arn> [at:<name>:<port>] [roots:<pem-path>] file:<token-path>, and <uses> the policy of each fetched secret key in a Secret grant's spelling";

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
