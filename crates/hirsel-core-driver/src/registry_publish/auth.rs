use super::{Failure, METADATA_LIMIT, decimal, hex};
use crate::call::base64;
use crate::registry_verify::decode_base64;
use ring::signature::{Ed25519KeyPair, KeyPair};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::Read;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use ureq::http::Uri;

pub(super) struct Authorizer {
    pub(super) subject: Value,
    proof: Provider,
}

enum Provider {
    Key(Ed25519KeyPair),
    Workload(PathBuf),
}

impl Authorizer {
    pub(super) fn load(
        key: Option<String>,
        workload: Option<String>,
        token_command: Option<String>,
    ) -> Result<Self, Failure> {
        match (key, workload, token_command) {
            (Some(key), None, None) => {
                let pkcs8 = read_file(Path::new(&key), 4096)?;
                let key = Ed25519KeyPair::from_pkcs8_maybe_unchecked(&pkcs8)
                    .map_err(|_| "invalid PKCS#8 Ed25519 signing key")?;
                Ok(Self {
                    subject: json!(["ed25519", base64(key.public_key().as_ref())]),
                    proof: Provider::Key(key),
                })
            }
            (None, Some(workload), Some(command)) => {
                let subject: Value = read_json(&workload)?;
                if !valid_subject(&subject) || subject[0] != "oidc" {
                    return Err("invalid OIDC workload subject".into());
                }
                let command = Path::new(&command).canonicalize()?;
                let meta = command.metadata()?;
                if !meta.is_file() || meta.permissions().mode() & 0o111 == 0 {
                    return Err("the OIDC token command must be an executable file".into());
                }
                Ok(Self {
                    subject,
                    proof: Provider::Workload(command),
                })
            }
            _ => Err(
                "give --signing-key FILE or both --oidc-subject FILE and --oidc-token-command FILE"
                    .into(),
            ),
        }
    }

    pub(super) fn prove(&self, origin: &str, root: &str, intent: &[u8]) -> Result<String, Failure> {
        match &self.proof {
            Provider::Key(key) => Ok(base64(key.sign(intent).as_ref())),
            Provider::Workload(command) => {
                let digest = hex(&Sha256::digest(intent));
                let audience = format!("{origin}/intent/publish/{root}/{digest}");
                workload_token(command, &audience)
            }
        }
    }
}

fn workload_token(command: &Path, audience: &str) -> Result<String, Failure> {
    let failure = || "OIDC token command failed to supply a compact JWS";
    let mut child = Command::new(command)
        .arg(audience)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| failure())?;
    let mut token = String::new();
    // Closing stdout before wait bounds the read even if the provider overproduces.
    let read = child
        .stdout
        .take()
        .expect("piped stdout")
        .take(METADATA_LIMIT as u64 + 1)
        .read_to_string(&mut token);
    let status = child.wait().map_err(|_| failure())?;
    let compact = token.trim_end_matches(['\r', '\n']);
    if read.is_err()
        || !status.success()
        || token.len() > METADATA_LIMIT
        || compact.split('.').count() != 3
        || !compact.split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        })
    {
        return Err(failure().into());
    }
    token.truncate(compact.len());
    Ok(token)
}

fn read_file(path: &Path, limit: usize) -> Result<Vec<u8>, Failure> {
    let mut file = File::options()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(format!("{} must be a regular file", path.display()).into());
    }
    let mut bytes = Vec::new();
    (&mut file).take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(format!("{} exceeds {limit} bytes", path.display()).into());
    }
    Ok(bytes)
}

fn read_json<T: serde::de::DeserializeOwned>(path: &str) -> Result<T, Failure> {
    serde_json::from_slice(&read_file(Path::new(path), METADATA_LIMIT)?)
        .map_err(|error| format!("{path}: {error}").into())
}

pub(super) fn enrollment(path: &str, subject: &Value) -> Result<Vec<Value>, Failure> {
    let subjects: Vec<Value> = read_json(path)?;
    if subjects.is_empty() || !subjects.contains(subject) || !subjects.iter().all(valid_subject) {
        return Err(
            "the initial subject set must contain the active publisher and valid subjects".into(),
        );
    }
    let ordered = subjects
        .iter()
        .map(serde_json::to_vec)
        .collect::<Result<Vec<_>, _>>()?;
    if !ordered.windows(2).all(|pair| pair[0] < pair[1]) {
        return Err("the initial subject set must be distinct and in canonical byte order".into());
    }
    Ok(subjects)
}

fn valid_subject(subject: &Value) -> bool {
    let Some(fields) = subject.as_array().map(Vec::as_slice) else {
        return false;
    };
    let positive =
        |s: &str| !s.is_empty() && !s.starts_with('0') && s.bytes().all(|b| b.is_ascii_digit());
    match fields {
        [tag, key] if tag == "ed25519" => key
            .as_str()
            .and_then(decode_base64)
            .is_some_and(|key| key.len() == 32),
        [tag, issuer, id, workflow] if tag == "oidc" => {
            let uri = issuer.as_str().and_then(|s| s.parse::<Uri>().ok());
            let id = id.as_str().and_then(|value| value.split_once('/'));
            let workflow = workflow.as_str().and_then(|value| value.split_once('/'));
            uri.is_some_and(|uri| {
                uri.scheme_str() == Some("https")
                    && uri.authority().is_some_and(|a| !a.as_str().contains('@'))
            }) && id.is_some_and(|(kind, n)| matches!(kind, "owner" | "repo") && positive(n))
                && workflow.is_some_and(|(repo, selector)| {
                    positive(repo)
                        && selector
                            .split_once("@refs/")
                            .is_some_and(|(path, git_ref)| {
                                !path.is_empty() && !path.starts_with('/') && !git_ref.is_empty()
                            })
                })
        }
        _ => false,
    }
}

pub(super) fn generation(text: Option<&str>, enrollment: bool) -> Result<u64, Failure> {
    match (text, enrollment) {
        (None, true) => Ok(0),
        (None, false) => Ok(1),
        (Some(text), false) => decimal(text)
            .filter(|value| *value > 0)
            .ok_or_else(|| "--generation requires a positive decimal generation".into()),
        (Some(_), true) => Err("--enroll starts at generation zero; omit --generation".into()),
    }
}
