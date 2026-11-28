//! Recover the recipe and image from launcher-owned records. The caller supplies
//! a restore receipt, which the Store helper checks against the stopped child.
use super::{objects::Records, session::Wire};
use crate::{AssemblyRecord, operator::Recovery, operator_cli::store_process};
use hirsel_core_ids::{ContentHash, hex};
use std::{
    fs::{File, OpenOptions},
    io,
    os::unix::fs::OpenOptionsExt,
    time::Instant,
};

pub(super) struct Verified {
    pub record: AssemblyRecord,
}

fn invalid(detail: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, detail)
}
pub(super) fn decode(bytes: &[u8]) -> io::Result<Recovery> {
    Recovery::decode(bytes)
}

fn image(bytes: &[u8]) -> io::Result<ContentHash> {
    let digest = bytes
        .strip_prefix(&32u32.to_le_bytes())
        .and_then(|v| v.get(..32))
        .ok_or_else(|| invalid("Describe has no executable digest"))?;
    Ok(ContentHash(digest.try_into().unwrap()))
}

pub(super) fn inspect(plan: &Recovery, records: &Records) -> io::Result<Verified> {
    let source = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(&plan.directory)?;
    let tmp = tempfile::tempdir()?;
    let scratch = File::open(tmp.path())?;
    let check = store_process::command("store_tool", &source, &scratch)
        .map_err(|e| io::Error::other(e.to_string()))?
        .args([
            "inspect-restored",
            "selected",
            &plan.branch.to_string(),
            &plan.boundary,
            &plan.attempt,
            &plan.cut.to_string(),
            &plan.at_ms.to_string(),
            &plan.main_version.to_string(),
            &plan.main_at_ms.to_string(),
            &plan.activation.to_string(),
            &plan.release_binding,
            &format!("{:016x}{:016x}", plan.child.hi, plan.child.lo),
        ])
        .output()?;
    if !check.status.success() {
        return Err(io::Error::other(
            String::from_utf8_lossy(&check.stderr).into_owned(),
        ));
    }
    let text = std::str::from_utf8(&check.stdout)
        .map_err(|_| invalid("invalid Store evidence"))?
        .trim();
    let saved = if text == "unactivated" {
        None
    } else {
        Some(hex::decode(text).ok_or_else(|| invalid("invalid saved Describe"))?)
    };
    let digest = saved.as_deref().map(image).transpose()?;
    let record = if let Some(pin) = &plan.assembly {
        records.assembly(ContentHash(
            pin.as_slice()
                .try_into()
                .map_err(|_| invalid("assembly identity length"))?,
        ))?
    } else {
        records
            .for_image(digest.ok_or_else(|| invalid("an unactivated child requires --assembly"))?)?
    };
    if digest.is_some_and(|value| value != record.digest) {
        return Err(invalid(
            "retained assembly differs from the restored release",
        ));
    }
    Ok(Verified { record })
}

pub(super) fn confirm(
    proof: &Verified,
    records: &Records,
    n_place: u64,
    deadline: Instant,
) -> io::Result<Vec<u8>> {
    let record = records.assembly(proof.record.identity)?;
    records.open_image(record.digest)?;
    let actual = crate::describe::describe_child(
        &records.image_path(record.digest),
        n_place,
        deadline.saturating_duration_since(Instant::now()),
    )
    .map_err(|e| io::Error::other(e.to_string()))?;
    // Genesis omits diagnostic tables from its saved Describe. The platform checks
    // the returned Describe against the Runner binding recorded at the proven cut.
    if image(&actual)? != record.digest {
        return Err(invalid(
            "retained executable Describe differs from the restored release",
        ));
    }
    Ok(actual)
}
