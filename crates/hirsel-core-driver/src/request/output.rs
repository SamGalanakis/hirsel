use hirsel_core_layout::{OutputKind, Target};

pub(super) fn output_kind(command: &str, emit: Option<&str>) -> Result<OutputKind, String> {
    if command == "sdk swift" && emit.is_some() {
        return Err("sdk swift refuses --emit".into());
    }
    let output = emit.map(str::parse).transpose()?.unwrap_or_default();
    if output == OutputKind::StaticLibrary && command != "build" {
        return Err(format!(
            "{command}: --emit staticlib is only supported by build"
        ));
    }
    Ok(output)
}

pub(super) fn admit_target(target: &str, output: OutputKind) -> Result<(), String> {
    let target =
        Target::from_triple(target).ok_or_else(|| format!("unsupported target {target}"))?;
    target
        .admits(output)
        .map_err(|reason| format!("{}: {reason}", target.triple()))
}

pub(super) fn default_path(kind: OutputKind) -> std::path::PathBuf {
    match kind {
        OutputKind::Executable => "program",
        OutputKind::StaticLibrary => "libprogram.a",
    }
    .into()
}

pub(super) fn validate_archive_flags(
    recording: bool,
    manifest: bool,
    charge: bool,
) -> Result<(), String> {
    for (enabled, flag) in [
        (recording, "--recording"),
        (manifest, "--manifest"),
        (charge, "--logical-charge-report"),
    ] {
        if enabled {
            return Err(format!(
                "build: --emit staticlib cannot be combined with {flag}"
            ));
        }
    }
    Ok(())
}
