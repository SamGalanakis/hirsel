//! Final ELF link of the checked native inputs.
use super::{NativeStop, command};
use std::path::{Path, PathBuf};

pub(super) fn direct_link(
    recipe: &crate::tool_inputs::LinkRecipe,
    inputs: &[PathBuf],
    support_libs: &[&str],
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
        // The startup data stays at the front of read-only data. Align the
        // source table separately so ordinary reads do not share its pages.
        let layout = tempfile::NamedTempFile::new_in(parent.unwrap_or(Path::new(".")))?;
        std::fs::write(
            layout.path(),
            "SECTIONS {\n\
                .rodata.hirsel_sites : ALIGN(CONSTANT(MAXPAGESIZE)) { *(.rodata.hirsel_sites) }\n\
             } INSERT AFTER .rodata;\n",
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
                process.arg(std::path::absolute(input)?);
            }
            for library in support_libs {
                process.arg(library);
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
