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

    pub(crate) fn print(summary: &str) {
        for line in summary
            .lines()
            .skip(1)
            .filter(|line| *line != "end" && !line.starts_with('\t'))
        {
            if let Some(witness) = line.strip_prefix("witness\t") {
                let Some((count, label)) = witness.split_once('\t') else {
                    continue;
                };
                let Some(label) = witness_label(label) else {
                    continue;
                };
                if label.is_empty() || label.chars().any(|c| c.is_whitespace() || c.is_control()) {
                    println!("  sometimes {label:?} {count}");
                } else {
                    println!("  sometimes {label} {count}");
                }
            } else {
                println!("  {line}");
            }
        }
    }
}

fn witness_label(hex: &str) -> Option<String> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    let bytes = (0..hex.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(hex.get(at..at + 2)?, 16).ok())
        .collect::<Option<Vec<_>>>()?;
    String::from_utf8(bytes).ok()
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
