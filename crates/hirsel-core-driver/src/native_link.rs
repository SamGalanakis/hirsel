//! Final ELF link of the checked native inputs.
use super::{NativeOptions, NativeStop, key};
use hirsel_core_ids::ContentHash;
use std::path::{Path, PathBuf};
use std::process::Command;

pub(crate) fn command(command: &mut Command) -> Result<Vec<u8>, NativeStop> {
    let result = command
        .output()
        .map_err(|e| NativeStop::Link(format!("launch {command:?}: {e}")))?;
    if !result.status.success() {
        return Err(NativeStop::Link(format!(
            "{command:?}: {}\n{}",
            result.status,
            String::from_utf8_lossy(&result.stderr)
        )));
    }
    Ok(result.stdout)
}

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
            crate::compiler_identity(),
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

pub(crate) fn direct_link(
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
    let format = hirsel_core_layout::Target::from_triple(&recipe.target)
        .ok_or_else(|| NativeStop::Link(format!("no link target {}", recipe.target)))?
        .object_format();
    match format {
        hirsel_core_layout::ObjectFormat::Elf => {}
        hirsel_core_layout::ObjectFormat::MachO => {
            return macho_link(process, directory, recipe, inputs, whole, output, optimize);
        }
    }
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

/// The Mach-O link: ld64 takes its flavour first, the objects through a file
/// list, the bundle with `-force_load`, and dead-strips per atom. It writes no
/// debug map (`-S`): the detached debug file holds the image's DWARF. The
/// fixed name "program" is also the code signature's identifier, so a cross
/// and a native link sign alike. Every link keeps ld64's `LC_UUID`, a digest
/// of the image: dyld refuses an arm64 executable without one ("missing
/// LC_UUID load command", macOS 26), so a dev link's debug file is its dSYM,
/// as a release link's is.
fn macho_link(
    mut process: std::process::Command,
    directory: tempfile::TempDir,
    recipe: &crate::tool_inputs::LinkRecipe,
    inputs: &[PathBuf],
    whole: Option<&Path>,
    output: &Path,
    optimize: bool,
) -> Result<(), NativeStop> {
    let mut inserted = false;
    for argument in &recipe.args {
        if argument == crate::tool_inputs::INPUT_MARKER {
            let mut list = String::new();
            for input in inputs {
                // The bundle of the link's objects loads every member.
                let bundle = whole == Some(input.as_path());
                let input = std::path::absolute(input)?;
                if bundle {
                    process.arg("-force_load").arg(&input);
                } else {
                    let name = input.to_str().ok_or_else(|| {
                        NativeStop::Link(format!("non-UTF-8 link input {}", input.display()))
                    })?;
                    list.push_str(name);
                    list.push('\n');
                }
            }
            let path = directory.path().join("inputs");
            std::fs::write(&path, list)?;
            process.arg("-filelist").arg(path);
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
    if optimize {
        process.arg("-dead_strip");
    }
    process.arg("-S");
    process.arg("-o").arg("program");
    command(&mut process)?;
    std::fs::rename(directory.path().join("program"), output)?;
    Ok(())
}

/// Merge compiled objects, foreign archives, and the runtime into a single
/// static archive (`.a`) for embedding into an iOS/macOS host app.
pub(crate) fn static_link(inputs: &[PathBuf], output: &Path) -> Result<(), NativeStop> {
    let parent = output.parent().filter(|p| !p.as_os_str().is_empty());
    let directory = tempfile::tempdir_in(parent.unwrap_or(Path::new(".")))?;
    let mut process = Command::new("/usr/bin/libtool");
    process.current_dir(directory.path());
    process.args(["-static", "-no_warning_for_no_symbols", "-o", "libprogram.a"]);
    for input in inputs {
        process.arg(std::path::absolute(input)?);
    }
    command(&mut process)?;
    std::fs::rename(directory.path().join("libprogram.a"), output)?;
    Ok(())
}
