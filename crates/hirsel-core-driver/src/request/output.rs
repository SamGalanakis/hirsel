use super::flags::CompilerFlags;
use super::{Meter, Outcome, Output, Path, PathBuf, ReachableOutput};
use crate::installation::Flavour;
use hirsel_core_layout::{OutputKind, Target};

pub(super) fn render_check_diagnostics(
    product: &crate::CheckProduct,
    sources: &crate::SourceFiles,
) -> std::io::Result<()> {
    super::launch::render_diagnostics(
        sources,
        &product.machine_diagnostics()?,
        &product.machine_incomplete()?,
        &product.machine_resources()?,
    );
    Ok(())
}

pub(super) fn check_result(
    checked: std::io::Result<crate::Checked>,
    sources: &crate::SourceFiles,
    render: bool,
) -> std::io::Result<crate::Checked> {
    if render
        && let Err(error) = &checked
        && let Some(refusal) = error
            .get_ref()
            .and_then(|cause| cause.downcast_ref::<crate::RejectedCheck>())
    {
        render_check_diagnostics(&refusal.product, sources)?;
    }
    checked
}

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

/// Answer a request from its record: render the value for this request,
/// write a build's stored executable to its output, and finish as a checked
/// request does.
#[allow(clippy::too_many_arguments)]
pub(super) fn answer_hit(
    hit: &crate::request_record::Hit,
    command: &str,
    json: bool,
    sources: &crate::SourceFiles,
    output_path: Option<&Path>,
    runner: &crate::Runner,
    run_args: Vec<String>,
    record_path: Option<&str>,
    replay_outputs: Option<&str>,
    profile: Option<&crate::prof_command::Run>,
    run_output: Option<crate::store::Scratch>,
    meter: &mut Meter,
    metrics: Option<PathBuf>,
    remote: Option<&crate::store::remote::Remote>,
) -> Result<Outcome, Box<dyn std::error::Error>> {
    let rendered = hit.render();
    let mut stopped = None;
    let native = match (output_path, &hit.executable) {
        (Some(path), Some(binary)) => {
            let product = hit.build(path).expect("a stored executable has its build");
            match binary.deliver(path) {
                Ok(()) => Some(product),
                Err(error) => {
                    stopped = Some(crate::executable::output_stop(&error));
                    None
                }
            }
        }
        _ => None,
    };
    meter.finish("request record/deliver");
    let reachable = rendered
        .reach
        .as_ref()
        .map(
            |(bodies, declarations, census, first_stops)| ReachableOutput {
                bodies: *bodies,
                declarations: *declarations,
                census: census.clone(),
                first_stops,
            },
        );
    let output = Output {
        accepted: rendered.accepted,
        root: if json {
            rendered.root.to_string()
        } else {
            String::new()
        },
        files: rendered.files,
        checked_definitions: rendered.checked_definitions,
        rechecked: 0,
        source_bytes: rendered.source_bytes,
        diagnostics: rendered.diagnostics,
        incomplete: rendered.incomplete,
        resources: rendered.resources,
        image: None,
        native,
        stopped,
        reachable,
    };
    super::finish(
        &output,
        command,
        json,
        sources,
        runner,
        run_args,
        record_path,
        replay_outputs,
        profile,
        run_output,
        meter,
        metrics,
        remote,
    )
}
