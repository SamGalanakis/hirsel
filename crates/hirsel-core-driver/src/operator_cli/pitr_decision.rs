//! The caller keeps review coordinates outside either Store. One immutable vote
//! survives process loss. Publication is idempotent at the exact Store boundary.
use super::{
    config, decision,
    finite::{Envelope, Failure},
    pitr::RestorePlan,
    receipt,
    wait::Wait,
};
use crate::cli::{DecideOption, OpGetOption, Request};
use hirsel_config_schema::host_v1::{Decision, Verdict};
use hirsel_core_ids::hex;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::json;
use std::{
    fs::{File, OpenOptions},
    io::{self, BufReader, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

pub(super) use super::receipt::operations_dir;

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
pub(super) enum StorePlan {
    Restore(RestorePlan),
    Bucket(super::bucket::BucketPlan),
    Expiry(super::archive_decision::Expiry),
}
impl StorePlan {
    fn attempt(&self) -> &str {
        match self {
            Self::Restore(p) => &p.attempt,
            Self::Bucket(p) => &p.attempt,
            Self::Expiry(p) => &p.attempt,
        }
    }
    fn digest(&self) -> String {
        use sha2::{Digest, Sha256};
        hex::encode(&Sha256::digest(
            serde_json::to_vec(self).expect("serializable store decision"),
        ))
    }
    pub(super) fn held(&self, env: &mut Envelope<'_>) {
        match self {
            Self::Restore(p) => p.held(&self.digest(), env),
            Self::Bucket(p) => p.held(&self.digest(), env),
            Self::Expiry(p) => p.held(&self.digest(), env),
        }
    }
    fn publish(&self, path: &Path) -> Result<serde_json::Value, Failure> {
        match self {
            Self::Restore(p) => p.publish(path),
            Self::Bucket(p) => p.publish(),
            Self::Expiry(p) => p.apply(),
        }
    }
}
pub(crate) fn request_id(op: &str) -> Result<&str, String> {
    op.strip_prefix("store/")
        .filter(|s| {
            s.len() == 32
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
        .ok_or_else(|| "expected store/ATTEMPT with a 32-digit hexadecimal attempt".into())
}
fn path(id: &str) -> Result<PathBuf, Failure> {
    Ok(operations_dir()?.join(format!("store-{id}.json")))
}
pub(super) fn save_plan(plan: &StorePlan) -> Result<(), Failure> {
    receipt::persist(&path(plan.attempt())?, plan).map_err(Failure::io)
}
pub(super) fn read<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, Failure> {
    let f = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Failure::io(e)),
    };
    let m = f.metadata().map_err(Failure::io)?;
    if !m.is_file() || m.uid() != crate::operator::operator_uid() || m.mode() & 0o077 != 0 {
        return Err(Failure::input("receipt", "untrusted operation receipt"));
    }
    serde_json::from_reader(f)
        .map(Some)
        .map_err(Failure::damaged)
}
fn load(op: &str) -> Result<(PathBuf, StorePlan), Failure> {
    let id = request_id(op).map_err(|e| Failure::input("operation", e))?;
    let path = path(id)?;
    let p: StorePlan =
        read(&path)?.ok_or_else(|| Failure::input("operation", "unknown store operation"))?;
    if p.attempt() != id {
        return Err(Failure::damaged("receipt names another attempt"));
    }
    Ok((path, p))
}
pub(super) fn once<T: Serialize + DeserializeOwned + PartialEq>(
    p: &Path,
    v: &T,
) -> Result<(), Failure> {
    match receipt::persist(p, v) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists && read::<T>(p)?.as_ref() == Some(v) => {
            Ok(())
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(Failure::input(
            "decision",
            "a different decision already exists",
        )),
        Err(e) => Err(Failure::io(e)),
    }
}
fn observe(path: &Path, plan: &StorePlan, env: &mut Envelope<'_>) -> Result<bool, Failure> {
    plan.held(env);
    if let Some(approved) = read::<bool>(&path.with_extension("decision.json"))? {
        if approved {
            env.outcome = json!({"kind":"Undecided","effect":"unknown"});
            let previous = read::<serde_json::Value>(&path.with_extension("published.json"))?;
            let (live, effect) = match plan {
                StorePlan::Restore(p) => (
                    p.observe(&path.with_extension("directory.json"))?,
                    "published",
                ),
                StorePlan::Bucket(p) => (p.observe()?, "published"),
                StorePlan::Expiry(_) => (previous.clone(), "expired"),
            };
            if previous.is_some() && previous != live {
                return Err(Failure::damaged(
                    "published child no longer matches the destination",
                ));
            }
            if let Some(done) = live {
                env.result = done;
                env.outcome = json!({"kind":"Completed","effect":effect});
                env.next.clear();
                return Ok(true);
            }
        }
        env.outcome = if approved {
            json!({"kind":"Undecided","effect":"unknown"})
        } else {
            json!({"kind":"Refused","effect":"none"})
        };
        if !approved {
            env.next.clear()
        }
        return Ok(!approved);
    }
    Ok(false)
}
pub(super) fn get(req: Request<OpGetOption>, env: &mut Envelope<'_>) -> Result<(), Failure> {
    let mut wait = None;
    for o in req.options {
        match o {
            OpGetOption::Json => (),
            OpGetOption::Wait(w) if wait.is_none() => wait = Some(Wait::start(w)?),
            _ => {
                return Err(Failure::input(
                    "options",
                    "a store operation takes --json or --wait",
                ));
            }
        }
    }
    let (path, p) = load(req.positional.as_deref().unwrap_or_default())?;
    let terminal = observe(&path, &p, env)?;
    if wait.is_some() {
        env.exit = if terminal {
            if env.outcome["kind"] == "Refused" {
                1
            } else {
                0
            }
        } else {
            75
        };
    }
    Ok(())
}
fn read_decision(
    file: Option<String>,
    p: &StorePlan,
    env: &Envelope<'_>,
) -> Result<Decision, Failure> {
    match file.as_deref() {
        Some("-") => config::decode(io::stdin().lock()),
        Some(v) => config::decode(File::open(v).map_err(Failure::io)?),
        None => {
            let mut tty = OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/tty")
                .map_err(|_| {
                    Failure::blocked(
                        "DecisionInputRequired",
                        "supply --decision FILE or use a controlling terminal",
                    )
                })?;
            writeln!(
                tty,
                "{}",
                serde_json::to_string_pretty(&env.result).unwrap()
            )
            .map_err(Failure::io)?;
            let mut reader = BufReader::new(tty.try_clone().map_err(Failure::io)?);
            let verdict = decision::read_verdict(&mut reader, &mut tty)?;
            Ok(Decision {
                plan_digest: p.digest(),
                verdict,
                all: None,
                entries: Vec::new(),
            })
        }
    }
}
pub(super) fn decide(req: Request<DecideOption>, env: &mut Envelope<'_>) -> Result<(), Failure> {
    let mut file = None;
    let mut wait = None;
    for o in req.options {
        match o {
            DecideOption::DecisionFile(f) if file.is_none() => file = Some(f),
            DecideOption::Json => (),
            DecideOption::Wait(w) if wait.is_none() => {
                wait = Some(Wait::start(w)?);
                env.contract = "wait"
            }
            _ => {
                return Err(Failure::input(
                    "options",
                    "unexpected or duplicate store decision option",
                ));
            }
        }
    }
    let (path, p) = load(req.positional.as_deref().unwrap_or_default())?;
    observe(&path, &p, env)?;
    let d = read_decision(file, &p, env)?;
    if hex::decode(&d.plan_digest) != hex::decode(&p.digest())
        || d.all.is_some()
        || !d.entries.is_empty()
    {
        return Err(Failure::input(
            "decision",
            "expected the exact restore plan digest and no entry fates",
        ));
    }
    let approved = d.verdict == Verdict::Approve;
    if let Some(w) = wait.as_ref() {
        w.remaining()?;
    }
    once(&path.with_extension("decision.json"), &approved)?;
    if approved && !observe(&path, &p, env)? {
        let done = p.publish(&path.with_extension("directory.json"))?;
        once(&path.with_extension("published.json"), &done)?;
    }
    observe(&path, &p, env)?;
    if wait.is_some() && !approved {
        env.exit = 1;
    }
    Ok(())
}
