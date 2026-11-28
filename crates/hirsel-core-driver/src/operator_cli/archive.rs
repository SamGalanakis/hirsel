//! An archive locator contains public S3 coordinates. The read and expiry
//! roles supply separate credentials to the child through its Secret grant.
use super::{
    finite::{Envelope, Failure},
    store_process,
};
use crate::cli::{Request, StoreOption as O};
use hirsel_core_ids::hex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    env,
    fs::{File, OpenOptions},
    os::unix::fs::OpenOptionsExt,
    path::Path,
};

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Coordinates {
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    pub prefix: String,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Archive {
    #[serde(flatten)]
    pub location: Coordinates,
    pub parent: String,
}

impl Archive {
    pub(super) fn read(path: &Path) -> Result<Self, Failure> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .map_err(Failure::io)?;
        if !file.metadata().map_err(Failure::io)?.is_file() {
            return Err(Failure::input(
                "archive",
                "expected an archive descriptor file",
            ));
        }
        let a: Self = serde_json::from_reader(file).map_err(|e| Failure::input("archive", e))?;
        a.location.validate()?;
        if !hex::decode(&a.parent).is_some_and(|b| b.len() == 16) {
            return Err(Failure::input("archive", "expected a 16-byte parent ID"));
        }
        Ok(a)
    }

    pub(super) fn invoke(
        &self,
        scratch: &File,
        verb: &str,
        args: &[String],
        expiry: bool,
    ) -> Result<Value, Failure> {
        let role = if expiry {
            "HIRSEL_ARCHIVE_EXPIRY"
        } else {
            "HIRSEL_ARCHIVE_READ"
        };
        self.invoke_role(scratch, verb, args, role)
    }

    pub(super) fn invoke_role(
        &self,
        work: &File,
        verb: &str,
        args: &[String],
        role: &str,
    ) -> Result<Value, Failure> {
        let mut cmd = store_process::command("archive_tool", work, work)
            .map_err(|e| Failure::blocked("Unavailable", &e.to_string()))?;
        self.configure(&mut cmd, role)?;
        let bytes = answer(cmd.arg(verb).args(args))?;
        serde_json::from_slice(&bytes).map_err(Failure::damaged)
    }

    pub(super) fn configure(
        &self,
        cmd: &mut std::process::Command,
        role: &str,
    ) -> Result<(), Failure> {
        self.location.configure(cmd, "HIRSEL_ARCHIVE");
        cmd.env("HIRSEL_ARCHIVE_PARENT", &self.parent);
        credentials(cmd, "HIRSEL_ARCHIVE", role)
    }
    pub(super) fn query(
        &self,
        verb: &str,
        args: &[String],
        expiry: bool,
    ) -> Result<Value, Failure> {
        let tmp = tempfile::tempdir().map_err(Failure::io)?;
        let work = File::open(tmp.path()).map_err(Failure::io)?;
        self.invoke(&work, verb, args, expiry)
    }
}
pub(crate) fn run(req: Request<O>) -> i32 {
    let machine = req.options.contains(&O::Json);
    let mut env = Envelope::read(req.command.name);
    if let Err(f) = review(req, &mut env) {
        env.fail(f)
    }
    env.render(machine)
}

fn review(req: Request<O>, env: &mut Envelope<'_>) -> Result<(), Failure> {
    let input = req
        .positional
        .as_deref()
        .ok_or_else(|| Failure::input("archive", "expected an archive descriptor"))?;
    let path = Path::new(input.strip_prefix('@').unwrap_or(input));
    let archive = Archive::read(path)?;
    env.target = json!({"kind":"archive","archive":archive});
    let mut deep = false;
    let mut cutoff = None;
    for opt in req.options {
        match opt {
            O::Json => (),
            O::Deep => deep = true,
            O::KeepFrom(n) if cutoff.is_none() => cutoff = Some(("base", n)),
            O::ArchiveAt(n) if cutoff.is_none() => cutoff = Some(("at", n)),
            _ => {
                return Err(Failure::input(
                    "options",
                    "duplicate or unrelated archive option",
                ));
            }
        }
    }
    let verb = match req.command.name {
        "store archive describe" => "describe",
        "store archive verify" if deep => "deep",
        "store archive verify" => "verify",
        "store archive expire" => {
            let (kind, n) = cutoff
                .ok_or_else(|| Failure::input("cutoff", "expected --keep-from BASE or --at MS"))?;
            let cutoff = n
                .parse::<u64>()
                .map_err(|_| Failure::input("cutoff", "expected unsigned decimal"))?;
            return super::archive_decision::plan(archive, kind, cutoff, env);
        }
        _ => return Err(Failure::input("command", "unknown archive command")),
    };
    env.result = archive.query(verb, &[], false)?;
    if req.command.name == "store archive verify" {
        env.result["verification"] = json!(if deep { "store_graph" } else { "references" });
    }
    Ok(())
}

impl Coordinates {
    pub(super) fn validate(&self) -> Result<(), Failure> {
        if self.region.is_empty()
            || self.bucket.is_empty()
            || canonical_endpoint(&self.endpoint).is_none()
        {
            return Err(Failure::input(
                "bucket",
                "expected region, bucket and an HTTPS or loopback HTTP endpoint",
            ));
        }
        Ok(())
    }

    pub(super) fn configure(&self, cmd: &mut std::process::Command, prefix: &str) {
        // Namespace comparisons use these coordinates too. Equivalent endpoint
        // spellings must identify the same bucket before any child reservation.
        if let Some(endpoint) = canonical_endpoint(&self.endpoint) {
            cmd.env(format!("{prefix}_ENDPOINT"), endpoint);
        }
        for (key, value) in [
            ("REGION", &self.region),
            ("BUCKET", &self.bucket),
            ("PREFIX", &self.prefix),
        ] {
            cmd.env(format!("{prefix}_{key}"), value);
        }
    }
}
fn canonical_endpoint(text: &str) -> Option<String> {
    let (scheme, authority) = text.strip_suffix('/').unwrap_or(text).split_once("://")?;
    let default = match scheme {
        "https" => 443,
        "http" => 80,
        _ => return None,
    };
    let (host, port) = match authority.split_once(':') {
        Some((host, port)) => (host, port.parse::<u16>().ok()?),
        None => (authority, default),
    };
    let host = host.strip_suffix('.').unwrap_or(host).to_ascii_lowercase();
    if port == 0
        || host.is_empty()
        || !host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'))
        || (scheme == "http" && host != "127.0.0.1")
    {
        return None;
    }
    Some(if port == default {
        format!("{scheme}://{host}")
    } else {
        format!("{scheme}://{host}:{port}")
    })
}
pub(super) fn credentials(
    cmd: &mut std::process::Command,
    target: &str,
    role: &str,
) -> Result<(), Failure> {
    for (field, name) in [
        ("SECRET_ACCESS_KEY", "SECRET"),
        ("ACCESS_KEY_ID", "ACCESS"),
        ("SESSION_TOKEN", "TOKEN"),
    ] {
        let value = env::var_os(format!("{role}_{field}")).unwrap_or_default();
        if value.is_empty() && field != "SESSION_TOKEN" {
            return Err(Failure::blocked(
                "CredentialsRequired",
                &format!("missing {role}_{field}"),
            ));
        }
        cmd.env(format!("{target}_{name}"), value);
    }
    Ok(())
}
pub(super) fn answer(cmd: &mut std::process::Command) -> Result<Vec<u8>, Failure> {
    let reply = cmd.output().map_err(Failure::io)?;
    if !reply.status.success() {
        let detail = String::from_utf8_lossy(&reply.stderr);
        return Err(match reply.status.code() {
            Some(75 | 103) => Failure::blocked("ObjectStoreUnavailable", &detail),
            Some(2) => Failure::input("object store", detail),
            _ => Failure::authority(crate::operator::Class::Store, detail.into_owned()),
        });
    }
    Ok(reply.stdout)
}
