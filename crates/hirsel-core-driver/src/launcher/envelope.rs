//! The host-declared admission envelope (launch.md "Starting the host").
use super::{DECODER_NESTING, limits};
use crate::code_admission::AdmissionLimits;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// The state file in which a running launcher publishes its admission envelope for
/// attested publishers.
pub(super) const ENVELOPE_FILE: &str = "admission";

fn parse_envelope(text: &str) -> Result<AdmissionLimits, String> {
    use hirsel_config_schema::{Config, host_v1::Admission};
    let v = Admission::read(text.as_bytes(), Default::default()).map_err(|e| e.to_string())?;
    let positive = |value: u64, field: &str| {
        usize::try_from(value)
            .ok()
            .filter(|n| *n > 0)
            .ok_or_else(|| format!("{field} must be a positive addressable size"))
    };
    let closure_objects = positive(v.closure_objects, "closure_objects")?;
    if v.object_bytes == 0 {
        return Err("object_bytes must be positive".into());
    }
    Ok(AdmissionLimits {
        recipe_bytes: positive(v.recipe_bytes, "recipe_bytes")?,
        closure_objects,
        object_bytes: v.object_bytes,
        decoded_bytes: positive(v.decoded_bytes, "decoded_bytes")?,
        members: positive(v.members, "members")?,
        edges: positive(v.edges, "edges")?,
        nesting: DECODER_NESTING,
        outstanding: closure_objects,
        timeout: None,
    })
}

pub(crate) fn envelope_text(limits: &AdmissionLimits) -> String {
    format!(
        "host_v1.Admission(recipe_bytes: {}, closure_objects: {}, object_bytes: {}, decoded_bytes: {}, members: {}, edges: {})\n",
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
            return Err("--admission requires --operator-listen".into());
        }
        return Ok(limits());
    }
    let file = file.ok_or("--admission FILE is required with a platform host")?;
    let text = fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
    parse_envelope(&text).map_err(|why| format!("{}: {why}", file.display()).into())
}
