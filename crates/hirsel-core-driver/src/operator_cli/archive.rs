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
pub(super) struct Archive {
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    pub prefix: String,
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
        if a.region.is_empty()
            || a.bucket.is_empty()
            || !hex::decode(&a.parent).is_some_and(|b| b.len() == 16)
        {
            return Err(Failure::input(
                "archive",
                "expected region, bucket and 16-byte parent ID",
            ));
        }
        let local = a
            .endpoint
            .strip_prefix("http://127.0.0.1:")
            .is_some_and(|p| p.parse::<u16>().is_ok());
        if !local && !a.endpoint.starts_with("https://") {
            return Err(Failure::input(
                "archive",
                "expected HTTPS endpoint or loopback HTTP",
            ));
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
        let mut cmd = store_process::command("archive_tool", scratch, scratch)
            .map_err(|e| Failure::blocked("Unavailable", &e.to_string()))?;
        let role = if expiry {
            "HIRSEL_ARCHIVE_EXPIRY"
        } else {
            "HIRSEL_ARCHIVE_READ"
        };
        let credential = |suffix: &str| {
            env::var_os(format!("{role}_{suffix}"))
                .filter(|v| !v.is_empty())
                .ok_or_else(|| {
                    Failure::blocked("CredentialsRequired", &format!("missing {role}_{suffix}"))
                })
        };
        for prefix in ["HIRSEL_ARCHIVE_READ", "HIRSEL_ARCHIVE_EXPIRY"] {
            for field in ["ACCESS_KEY_ID", "SECRET_ACCESS_KEY", "SESSION_TOKEN"] {
                cmd.env_remove(format!("{prefix}_{field}"));
            }
        }
        cmd.env("HIRSEL_ARCHIVE_SECRET", credential("SECRET_ACCESS_KEY")?)
            .env("HIRSEL_ARCHIVE_ACCESS", credential("ACCESS_KEY_ID")?)
            .env(
                "HIRSEL_ARCHIVE_TOKEN",
                env::var_os(format!("{role}_SESSION_TOKEN")).unwrap_or_default(),
            )
            .env("HIRSEL_ARCHIVE_ENDPOINT", &self.endpoint)
            .env("HIRSEL_ARCHIVE_REGION", &self.region)
            .env("HIRSEL_ARCHIVE_BUCKET", &self.bucket)
            .env("HIRSEL_ARCHIVE_PREFIX", &self.prefix)
            .env("HIRSEL_ARCHIVE_PARENT", &self.parent);
        let reply = cmd.arg(verb).args(args).output().map_err(Failure::io)?;
        if !reply.status.success() {
            let detail = String::from_utf8_lossy(&reply.stderr);
            return Err(match reply.status.code() {
                Some(75 | 103) => Failure::blocked("ArchiveUnavailable", &detail),
                Some(2) => Failure::input("archive", detail),
                _ => Failure::authority(crate::operator::Class::Store, detail.into_owned()),
            });
        }
        serde_json::from_slice(&reply.stdout).map_err(Failure::damaged)
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
            O::Deep if !deep => deep = true,
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
