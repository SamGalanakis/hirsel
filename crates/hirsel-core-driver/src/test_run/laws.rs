use std::{
    io,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Debug, Default)]
pub(crate) enum LawReport {
    #[default]
    Unobserved,
    Foreign,
    Observed(String),
}

impl LawReport {
    pub(crate) fn unobserved(&self) -> &'static str {
        if matches!(self, Self::Foreign) {
            "unobserved (FFI-8)"
        } else {
            "unobserved"
        }
    }
}

pub(super) fn law_file(command: &mut Command) -> io::Result<(tempfile::TempDir, PathBuf)> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("laws");
    command.env("HIRSEL_LAWS", &path);
    Ok((directory, path))
}

pub(super) fn law_report(path: &Path) -> LawReport {
    match std::fs::read_to_string(path) {
        Ok(summary)
            if summary.starts_with("hirsel-laws-observation 1\n")
                && summary.ends_with("\nend\n") =>
        {
            LawReport::Observed(summary)
        }
        _ => LawReport::Unobserved,
    }
}
