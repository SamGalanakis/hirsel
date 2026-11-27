use super::flags::CompilerFlags;
use crate::installation::Flavour;
use hirsel_core_layout::{OutputKind, Target};

pub(super) fn output_kind(command: &str, emit: Option<&str>) -> Result<OutputKind, String> {
    if command == "sdk swift" && emit.is_some() {
        return Err("sdk swift refuses --emit".into());
    }
    let output = emit.map(str::parse).transpose()?.unwrap_or_default();
    let name = match output {
        OutputKind::Executable => return Ok(output),
        OutputKind::StaticLibrary => "staticlib",
        OutputKind::Module => "module",
    };
    if command != "build" {
        return Err(format!(
            "{command}: --emit {name} is only supported by build"
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
        OutputKind::Module => "program.hirsel-mod",
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
    let name = match output {
        OutputKind::Executable => return Ok(()),
        OutputKind::StaticLibrary => "staticlib",
        OutputKind::Module => "module",
    };
    for (enabled, flag) in [
        (flags.recording_build, "--recording"),
        (flags.release_manifest.is_some(), "--manifest"),
        (
            flags.logical_charge_report.is_some(),
            "--logical-charge-report",
        ),
        // A module is its development units; the release recipe is not one yet.
        (output == OutputKind::Module && flags.release, "--release"),
    ] {
        if enabled {
            return Err(format!(
                "build: --emit {name} cannot be combined with {flag}"
            ));
        }
    }
    Ok(())
}
