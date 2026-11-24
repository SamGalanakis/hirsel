//! Final ELF link of the checked native inputs.
use super::{NativeOptions, NativeStop, command, key};
use hirsel_core_ids::ContentHash;
use std::path::{Path, PathBuf};

/// The build a recording and its replay share: the executable key without
/// the entry plan, whose launch policy (grants, directories, replay plan) is
/// the only difference between the two links of one build. The image objects
/// are linked as their image unit; the source map enters by its digest, the
/// runtime headers by the compiler identity, which embeds them.
pub(super) fn build_key(
    (toolchain, recipe, objects, archives): (
        ContentHash,
        ContentHash,
        &[ContentHash],
        &[ContentHash],
    ),
    sources: ContentHash,
    options: &NativeOptions,
    link_mode: &str,
) -> std::io::Result<ContentHash> {
    key(
        "hirsel.core-build.v1",
        &(
            crate::compiler_identity()?,
            toolchain,
            recipe,
            objects,
            archives,
            sources,
            &options.target,
            options.backend,
            options.optimize,
            options.debug_lines,
            link_mode,
        ),
    )
}

pub(super) fn direct_link(
    recipe: &crate::tool_inputs::LinkRecipe,
    inputs: &[PathBuf],
    whole: Option<&Path>,
    output: &Path,
    optimize: bool,
    debug_lines: bool,
    emit_relocations: bool,
) -> Result<(), NativeStop> {
    // The editor runs under an empty environment, in an empty directory of
    // its own: a script's relative name finds nothing there
    // (crate::link_closure). It writes a fixed relative name there, since
    // the output's name enters the executable (a version script's base
    // definition), and the executable is then moved to `output`.
    let parent = output.parent().filter(|p| !p.as_os_str().is_empty());
    let directory = tempfile::tempdir_in(parent.unwrap_or(Path::new(".")))?;
    let mut process = crate::tool_inputs::tool_command(&recipe.linker);
    process.current_dir(directory.path());
    let _layout = if optimize {
        // Keep the executable's code revision even when no tape reads it.
        process.args([
            "--gc-sections",
            "--undefined=hirsel_code_revision",
            "--icf=safe",
            "-z",
            "keep-text-section-prefix",
        ]);
        // Put the failure-only site table after the unwind metadata so it can
        // share a page with that metadata without padding the file.
        let layout = tempfile::NamedTempFile::new_in(parent.unwrap_or(Path::new(".")))?;
        std::fs::write(
            layout.path(),
            "SECTIONS {\n\
                .rodata.hirsel_sites : { *(.rodata.hirsel_sites) }\n\
             } INSERT AFTER .eh_frame;\n",
        )?;
        process.arg("-T").arg(std::path::absolute(layout.path())?);
        Some(layout)
    } else {
        None
    };
    if !debug_lines {
        process.arg("--strip-debug");
    }
    if emit_relocations {
        process.arg("--emit-relocs");
    }
    let mut inserted = false;
    for argument in &recipe.args {
        if argument == crate::tool_inputs::INPUT_MARKER {
            for input in inputs {
                // The bundle of the link's objects loads every member.
                if whole == Some(input.as_path()) {
                    process
                        .args(["--push-state", "--whole-archive"])
                        .arg(input)
                        .arg("--pop-state");
                } else {
                    process.arg(std::path::absolute(input)?);
                }
            }
            inserted = true;
        } else {
            process.arg(argument);
        }
    }
    if !inserted {
        return Err(NativeStop::Link(
            "direct link recipe has no input marker".into(),
        ));
    }
    if !optimize {
        process.arg("-O0");
    }
    process.arg("-o").arg("program");
    command(&mut process)?;
    std::fs::rename(directory.path().join("program"), output)?;
    Ok(())
}
