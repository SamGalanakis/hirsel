use super::flags::CompilerFlags;
use crate::installation::Flavour;
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

pub(super) fn validate_target_and_flags(
    command: &str,
    output: OutputKind,
    flags: &CompilerFlags,
) -> Result<(), String> {
    let kind = if matches!(command, "core-product" | "std-product")
        && !flags.explicit_runtime
        && flags.additional_runtimes.contains_key(&Flavour::EMBEDDED)
    {
        OutputKind::StaticLibrary
    } else {
        output
    };
    admit_target(&flags.target, kind)?;
    if flags.link_kit.is_some() && flags.install_root.is_none() {
        return Err("--link-kit requires --install".into());
    }
    if flags.exec_helper.is_some() && (flags.install_root.is_none() || flags.image_path.is_some()) {
        return Err("--exec-helper requires --install without --image".into());
    }
    if output != OutputKind::StaticLibrary {
        return Ok(());
    }
    for (enabled, flag) in [
        (flags.recording_build, "--recording"),
        (flags.release_manifest.is_some(), "--manifest"),
        (
            flags.logical_charge_report.is_some(),
            "--logical-charge-report",
        ),
    ] {
        if enabled {
            return Err(format!(
                "build: --emit staticlib cannot be combined with {flag}"
            ));
        }
    }
    Ok(())
}
