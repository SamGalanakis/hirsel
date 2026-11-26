//! The host-declared admission envelope (launch.md "Starting the host").
use super::{DECODER_NESTING, limits};
use crate::code_admission::AdmissionLimits;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// The state file in which a running launcher publishes its admission envelope for
/// attested publishers.
pub(super) const ENVELOPE_FILE: &str = "admission";

fn envelope_value(
    lines: &std::collections::BTreeMap<String, String>,
    key: &str,
) -> Result<String, String> {
    lines
        .get(key)
        .cloned()
        .ok_or_else(|| format!("missing {key}"))
}

/// Parse the host's declared admission envelope. `outstanding` derives from the closure
/// bound because both count objects; the decoder nesting ceiling is independent of host
/// sizing. The wire deadline is not part of the envelope: one admission deadline governs it.
pub(super) fn parse_envelope(text: &str) -> Result<AdmissionLimits, String> {
    let mut lines = std::collections::BTreeMap::new();
    for (n, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(format!("line {} is not key=value", n + 1));
        };
        if lines
            .insert(key.trim().to_owned(), value.trim().to_owned())
            .is_some()
        {
            return Err(format!("{key} repeats"));
        }
    }
    let usize_value = |key: &str| -> Result<usize, String> {
        envelope_value(&lines, key)?
            .parse::<usize>()
            .ok()
            .filter(|&n| n != 0)
            .ok_or_else(|| format!("{key} is not a positive decimal"))
    };
    let object_bytes = envelope_value(&lines, "object_bytes")?
        .parse::<u64>()
        .ok()
        .filter(|&n| n != 0)
        .ok_or_else(|| "object_bytes is not a positive decimal".to_owned())?;
    let closure_objects = usize_value("closure_objects")?;
    let known = [
        "recipe_bytes",
        "closure_objects",
        "object_bytes",
        "decoded_bytes",
        "members",
        "edges",
    ];
    if let Some(key) = lines.keys().find(|key| !known.contains(&key.as_str())) {
        return Err(format!("unknown key {key}"));
    }
    Ok(AdmissionLimits {
        recipe_bytes: usize_value("recipe_bytes")?,
        closure_objects,
        object_bytes,
        decoded_bytes: usize_value("decoded_bytes")?,
        members: usize_value("members")?,
        edges: usize_value("edges")?,
        nesting: DECODER_NESTING,
        outstanding: closure_objects,
        timeout: None,
    })
}

pub(crate) fn envelope_text(limits: &AdmissionLimits) -> String {
    format!(
        "recipe_bytes={}\nclosure_objects={}\nobject_bytes={}\ndecoded_bytes={}\nmembers={}\nedges={}\n",
        limits.recipe_bytes,
        limits.closure_objects,
        limits.object_bytes,
        limits.decoded_bytes,
        limits.members,
        limits.edges
    )
}

pub(crate) fn write_envelope(state: &Path, limits: &AdmissionLimits) -> io::Result<()> {
    let mut file = tempfile::NamedTempFile::new_in(state)?;
    file.write_all(envelope_text(limits).as_bytes())?;
    file.persist(state.join(ENVELOPE_FILE))?;
    Ok(())
}

pub(crate) fn read_envelope(state: &Path) -> io::Result<AdmissionLimits> {
    crate::operator::check_state(state)?;
    let path = state.join(ENVELOPE_FILE);
    let text = fs::read_to_string(&path)?;
    parse_envelope(&text).map_err(|why| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{}: {why}", path.display()),
        )
    })
}

/// The envelope for this launcher invocation. A launcher with the platform host requires
/// the operator's explicit file. A control-only launcher keeps the fixture bounds.
pub(crate) fn launcher_envelope(
    production: bool,
    file: Option<PathBuf>,
) -> Result<AdmissionLimits, Box<dyn std::error::Error>> {
    if !production {
        if file.is_some() {
            return Err("--admission requires --operator-listen and --platform-host".into());
        }
        return Ok(limits());
    }
    let file = file.ok_or("--admission FILE is required with a platform host")?;
    let text = fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
    parse_envelope(&text).map_err(|why| format!("{}: {why}", file.display()).into())
}
