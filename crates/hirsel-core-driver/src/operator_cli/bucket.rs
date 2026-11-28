//! Standalone Bucket restore. The operator's destination profile identifies
//! the source Place and host stream namespaces, including explicit absence.
use super::{
    archive::{self, Archive, Coordinates},
    finite::{Envelope, Failure},
    pitr::{self, DirectoryId, Point},
    store_process,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::{File, OpenOptions},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Destination {
    pub(super) location: Coordinates,
    level: String,
    #[serde(deserialize_with = "Deserialize::deserialize")]
    source_place: Option<Coordinates>,
    #[serde(deserialize_with = "Deserialize::deserialize")]
    host_stream: Option<Coordinates>,
}

impl Destination {
    pub(super) fn read(path: &Path) -> Result<Self, Failure> {
        super::context::trusted_path(path, crate::operator::operator_uid())?;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .map_err(Failure::io)?;
        let m = file.metadata().map_err(Failure::io)?;
        if !m.is_file()
            || (m.uid() != 0 && m.uid() != crate::operator::operator_uid())
            || m.mode() & 0o022 != 0
        {
            return Err(Failure::input(
                "to",
                "expected an operator-owned Bucket configuration",
            ));
        }
        let dest: Self = serde_json::from_reader(file).map_err(|e| Failure::input("to", e))?;
        if dest.level.is_empty() || dest.location.prefix.is_empty() {
            return Err(Failure::input(
                "to",
                "expected a level name and exclusive Bucket prefix",
            ));
        }
        dest.location.validate()?;
        for reserved in [&dest.source_place, &dest.host_stream]
            .into_iter()
            .flatten()
        {
            reserved.validate()?;
        }
        Ok(dest)
    }

    pub(super) fn run(
        &self,
        source: Option<&File>,
        src: Option<&Archive>,
        verb: &str,
        args: &[String],
    ) -> Result<Vec<u8>, Failure> {
        let tmp = tempfile::tempdir().map_err(Failure::io)?;
        let scratch = File::open(tmp.path()).map_err(Failure::io)?;
        // Archive coordinates are passed separately. Only stamped backups supply a Directory.
        let backup = source.filter(|_| src.is_none()).unwrap_or(&scratch);
        let mut cmd = store_process::command("bucket_tool", backup, &scratch)
            .map_err(|e| Failure::blocked("Unavailable", &e.to_string()))?;
        self.location.configure(&mut cmd, "HIRSEL_CHILD");
        archive::credentials(&mut cmd, "HIRSEL_CHILD", "HIRSEL_RESTORE_BUCKET")?;
        cmd.env("HIRSEL_CHILD_LEVEL", &self.level);
        for (prefix, coord) in [
            ("HIRSEL_SOURCE_PLACE", &self.source_place),
            ("HIRSEL_HOST_STREAM", &self.host_stream),
        ] {
            if let Some(c) = coord {
                c.configure(&mut cmd, prefix);
            } else {
                cmd.env(format!("{prefix}_ENDPOINT"), "");
            }
        }
        if let Some(a) = src {
            a.configure(&mut cmd, "HIRSEL_ARCHIVE_READ")?;
        } else {
            // The entry takes two sealed grants. A backup or observation needs
            // no archive credential and constructs no source client.
            cmd.env("HIRSEL_ARCHIVE_PARENT", "")
                .env("HIRSEL_ARCHIVE_SECRET", "unused");
        }
        archive::answer(cmd.arg(verb).args(args))
    }
}

#[derive(Serialize, Deserialize)]
pub(super) struct BucketPlan {
    source: PathBuf,
    source_id: DirectoryId,
    archive: Option<Archive>,
    destination: Destination,
    point: Point,
    pub(super) attempt: String,
}

pub(super) fn plan(
    source: &Path,
    archive: Option<Archive>,
    destination: &Path,
    target: &[String],
) -> Result<BucketPlan, Failure> {
    let profile = std::path::absolute(destination).map_err(Failure::io)?;
    pitr::check_evidence_paths(source, &profile)?;
    let dest = Destination::read(&profile)?;
    let from = pitr::source_handle(source, archive.is_some())?;
    let bytes = dest.run(Some(&from), archive.as_ref(), "plan-bucket", target)?;
    Ok(BucketPlan {
        source: source.into(),
        source_id: DirectoryId::read(&from)?,
        archive,
        destination: dest,
        point: serde_json::from_slice(&bytes).map_err(Failure::damaged)?,
        attempt: pitr::attempt()?,
    })
}

impl BucketPlan {
    pub(super) fn held(&self, digest: &str, env: &mut Envelope<'_>) {
        let id = format!("store/{}", self.attempt);
        env.target = json!({"kind":"bucket","destination":self.destination.location});
        env.operation = json!({"kind":"store","id":id,"request_id":self.attempt});
        env.result = json!({"plan_digest":digest,"source":self.source,"destination":self.destination,"attempt":self.attempt,"point":self.point});
        env.next = vec![json!({"method":"op decide","operation":id})];
        env.outcome = json!({"kind":"AwaitDecision","effect":"none"});
    }

    pub(super) fn observe(&self) -> Result<Option<Value>, Failure> {
        let mut args = self.point.pinned();
        args.extend([
            self.attempt.clone(),
            self.point.version.clone(),
            self.point.at_ms.clone(),
        ]);
        let bytes = self.destination.run(None, None, "observe-bucket", &args)?;
        let child = std::str::from_utf8(&bytes)
            .map_err(Failure::damaged)?
            .trim();
        if child == "pending" {
            return Ok(None);
        }
        Ok(Some(self.receipt(child)?))
    }
    fn receipt(&self, child: &str) -> Result<Value, Failure> {
        let mut receipt = self
            .point
            .receipt(child, &self.attempt, &self.destination.location)?;
        receipt["level"] = json!(self.destination.level);
        Ok(receipt)
    }
    pub(super) fn publish(&self) -> Result<Value, Failure> {
        let from = pitr::source_handle(&self.source, self.archive.is_some())?;
        if self.source_id != DirectoryId::read(&from)? {
            return Err(Failure::input("from", "source identity changed"));
        }
        if let Some(a) = &self.archive {
            if &Archive::read(&self.source)? != a {
                return Err(Failure::input("from", "archive descriptor changed"));
            }
        }
        let mut args = self.point.pinned();
        args.push(self.attempt.clone());
        let bytes =
            self.destination
                .run(Some(&from), self.archive.as_ref(), "publish-bucket", &args)?;
        let mut actual: Point = serde_json::from_slice(&bytes).map_err(Failure::damaged)?;
        let child = actual
            .child
            .take()
            .ok_or_else(|| Failure::damaged("Bucket publication returned no child ID"))?;
        if actual != self.point {
            return Err(Failure::damaged(
                "published Bucket boundary differs from approval",
            ));
        }
        self.receipt(&child)
    }
}
