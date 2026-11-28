//! A Preview keeps the archive admission identity independently of the child
//! Store. Releasing the hold remains possible if the mount has been removed.
use super::{
    archive::Archive,
    bucket::Destination,
    finite::{Envelope, Failure},
    pitr::{Point, attempt, selector},
    pitr_decision, receipt, wait,
};
use crate::cli::{Request, StoreOption};
use hirsel_core_ids::hex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::{self, File},
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const READ: &str = "HIRSEL_ARCHIVE_READ";
const HOLD: &str = "HIRSEL_ARCHIVE_HOLD";

#[derive(Serialize, Deserialize)]
struct Admission {
    archive: Archive,
    point: Point,
    incarnation: String,
    stamp: String,
    bucket: Option<Destination>,
}

impl Admission {
    fn args(&self, child: &str) -> Vec<String> {
        let mut args = self.point.pinned();
        args.extend([
            child.to_owned(),
            self.incarnation.clone(),
            self.stamp.clone(),
        ]);
        args
    }
}

fn paths(child: &str) -> Result<(PathBuf, PathBuf), Failure> {
    if !hex::decode(child).is_some_and(|bytes| bytes.len() == 16 && hex::encode(&bytes) == child) {
        return Err(Failure::input(
            "child",
            "expected the 32-digit child ID printed by preview create",
        ));
    }
    let base = receipt::operations_dir()?.join(format!("preview-{child}"));
    Ok((base.with_extension("json"), base))
}

fn expiry(ttl: &str) -> Result<u64, Failure> {
    let duration = wait::duration(ttl)
        .map_err(|_| Failure::input("ttl", "expected a duration in ms, s, m or h"))?;
    if duration.is_zero() {
        return Err(Failure::input("ttl", "expected a positive duration"));
    }
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|now| now.checked_add(duration))
        .and_then(|until| u64::try_from(until.as_millis()).ok())
        .ok_or_else(|| Failure::input("ttl", "expiry exceeds the archive timestamp range"))
}

fn answer(env: &mut Envelope<'_>, child: &str, local: &Path, admission: &Admission, status: Value) {
    env.result = status;
    let point = &admission.point;
    env.result["mount"] = match &admission.bucket {
        Some(dest) => json!(dest.location),
        None => json!(local),
    };
    env.result["child"] = json!(child);
    env.result["parent"] = json!(point.parent);
    env.result["version"] = json!(point.version);
    env.result["boundary"] = json!(point.boundary);
    env.next = vec![
        json!({"method":"preview release","child":child,"command":format!("hirsel preview release {child}")}),
    ];
}

fn create(
    place: &str,
    at: &str,
    ttl: &str,
    destination: Option<&str>,
    work: &File,
    env: &mut Envelope<'_>,
) -> Result<(), Failure> {
    let until = expiry(ttl)?;
    let bucket = destination
        .map(|profile| {
            let path = profile
                .strip_prefix('@')
                .ok_or_else(|| Failure::input("to", "expected @BUCKET_PROFILE"))?;
            Destination::read(&std::path::absolute(path).map_err(Failure::io)?)
        })
        .transpose()?;
    let target = if let Ok(ms) = at.parse::<u64>() {
        vec!["at".into(), "0".into(), ms.to_string()]
    } else {
        selector(at)?
    };
    let archive = Archive::read(Path::new(place.strip_prefix('@').unwrap_or(place)))?;
    let point: Point =
        serde_json::from_value(archive.invoke_role(work, "preview-select", &target, READ)?)
            .map_err(Failure::damaged)?;
    let child = attempt()?;
    let (path, mount) = paths(&child)?;
    env.target = json!({"child":child,"mount":mount});
    let mut args = point.pinned();
    args.extend([child.clone(), until.to_string()]);
    let status = archive.invoke_role(work, "preview-admit", &args, HOLD)?;
    env.outcome = json!({"kind":"Admitted","effect":"held"});
    let field = |name| {
        status
            .get(name)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| Failure::damaged("invalid admission identity"))
    };
    let admitted = Admission {
        archive,
        bucket,
        point,
        incarnation: field("incarnation")?,
        stamp: field("stamp")?,
    };
    answer(env, &child, &mount, &admitted, status);
    let mounted = (|| {
        receipt::persist(&path, &admitted).map_err(Failure::io)?;
        let mut args = admitted.args(&child);
        args.push(attempt()?);
        let point = if let Some(dest) = &admitted.bucket {
            let response =
                dest.run(None, Some(&admitted.archive), "preview-mount-bucket", &args)?;
            serde_json::from_slice(&response).map_err(Failure::damaged)?
        } else {
            fs::DirBuilder::new()
                .mode(0o700)
                .create(&mount)
                .map_err(Failure::io)?;
            admitted.archive.invoke_role(
                &File::open(&mount).map_err(Failure::io)?,
                "preview-mount",
                &args,
                READ,
            )?
        };
        if point["child"] != child || point["boundary"] != admitted.point.boundary {
            return Err(Failure::damaged(
                "mounted child differs from its archive admission",
            ));
        }
        Ok(())
    })();
    if let Err(problem) = mounted {
        let released =
            admitted
                .archive
                .invoke_role(work, "preview-release", &admitted.args(&child), HOLD)?;
        answer(env, &child, &mount, &admitted, released);
        env.outcome = json!({"kind":"Refused","effect":"released"});
        return Err(problem);
    }
    env.outcome = json!({"kind":"Completed","effect":"mounted"});
    Ok(())
}

fn execute(req: Request<StoreOption>, env: &mut Envelope<'_>) -> Result<(), Failure> {
    let subject = req.positional.ok_or_else(|| {
        Failure::input("target", "expected a Place archive descriptor or child ID")
    })?;
    let (mut at, mut ttl, mut destination) = (None, None, None);
    for opt in req.options {
        match opt {
            StoreOption::PreviewAt(s) => at = Some(s),
            StoreOption::Ttl(s) => ttl = Some(s),
            StoreOption::RestoreTo(s) => destination = Some(s),
            StoreOption::Json => (),
            _ => return Err(Failure::input("options", "unexpected preview option")),
        }
    }
    let tmp = tempfile::tempdir().map_err(Failure::io)?;
    let work = File::open(tmp.path()).map_err(Failure::io)?;
    if req.command.name == "preview create" {
        return create(
            &subject,
            at.as_deref().unwrap_or("latest"),
            ttl.as_deref().unwrap_or("1h"),
            destination.as_deref(),
            &work,
            env,
        );
    }
    let (path, mount) = paths(&subject)?;
    let admitted: Admission = pitr_decision::read(&path)?
        .ok_or_else(|| Failure::input("child", "unknown Preview child"))?;
    let mut args = admitted.args(&subject);
    let (verb, role, effect) = match req.command.name {
        "preview extend" => {
            let ttl = ttl.ok_or_else(|| Failure::input("ttl", "required for extend"))?;
            args.push(expiry(&ttl)?.to_string());
            ("preview-extend", HOLD, "extended")
        }
        "preview release" => ("preview-release", HOLD, "released"),
        "preview status" => ("preview-status", READ, "none"),
        _ => return Err(Failure::input("command", "unknown preview command")),
    };
    let status = admitted.archive.invoke_role(&work, verb, &args, role)?;
    answer(env, &subject, &mount, &admitted, status);
    env.outcome = json!({"kind":"Completed","effect":effect});
    Ok(())
}

pub(crate) fn run(req: Request<StoreOption>) -> i32 {
    let machine = req.options.contains(&StoreOption::Json);
    let mut env = Envelope::read(req.command.name);
    if req.command.name != "preview status" {
        env.contract = "execute";
    }
    if let Err(problem) = execute(req, &mut env) {
        env.fail(problem);
    }
    env.render(machine)
}
