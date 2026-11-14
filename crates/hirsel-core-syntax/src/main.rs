//! Internal formatter entry used by the formatting gate; the user CLI lives in the driver.
hirsel_guard::install!();
use hirsel_core_syntax::{FormatAction, ParseMode, Symbols, format, format_files, parse_bytes};
use std::{io::Write, path::PathBuf, process::ExitCode};
fn main() -> Result<ExitCode, Box<dyn std::error::Error>> {
    let mut paths = vec![];
    let mut action = None;
    let mut mode = ParseMode::Source;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--signatures" => mode = ParseMode::Signatures,
            "--check" | "--write" => {
                if action.is_some() {
                    return Err("choose one of --check or --write".into());
                }
                action = Some(if arg == "--check" {
                    FormatAction::Check
                } else {
                    FormatAction::Write
                });
            }
            _ if arg.starts_with('-') => return Err(format!("unknown argument {arg}").into()),
            _ => paths.push(PathBuf::from(arg)),
        }
    }
    if paths.is_empty() || action.is_none() && paths.len() != 1 {
        return Err("usage: hirsel-core-syntax [--check|--write] [--signatures] FILE...".into());
    }
    if let Some(action) = action {
        let report = format_files(
            &paths.into_iter().map(|p| (p, mode)).collect::<Vec<_>>(),
            action,
        )?;
        for path in &report.differing {
            println!("{}", path.display());
        }
        for refusal in &report.refused {
            let bytes = std::fs::read(&refusal.path)?;
            for d in &refusal.diagnostics {
                let (line, column) = position(&bytes, d.primary.start as usize);
                eprintln!(
                    "{}:{line}:{column}: {} {}",
                    refusal.path.display(),
                    d.rule,
                    d.message
                );
            }
            if let Some(r) = &refusal.resource_refusal {
                eprintln!(
                    "{}: incomplete compiler outcome: {r}",
                    refusal.path.display()
                );
            }
        }
        return Ok(
            if !report.refused.is_empty()
                || action == FormatAction::Check && !report.differing.is_empty()
            {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            },
        );
    }
    let path = &paths[0];
    let bytes = std::fs::read(path)?;
    let parsed = parse_bytes(&bytes, mode, &mut Symbols::default());
    let Some(text) = format(&parsed) else {
        for d in &parsed.diagnostics {
            let (line, column) = position(&bytes, d.primary.start as usize);
            eprintln!(
                "{}:{line}:{column}: {} {}",
                path.display(),
                d.rule,
                d.message
            );
        }
        if let Some(r) = parsed.resource_refusal {
            eprintln!("{}: incomplete compiler outcome: {r}", path.display());
        }
        return Ok(ExitCode::FAILURE);
    };
    std::io::stdout().lock().write_all(text.as_bytes())?;
    Ok(ExitCode::SUCCESS)
}
fn position(bytes: &[u8], offset: usize) -> (usize, usize) {
    let prefix = &bytes[..offset.min(bytes.len())];
    let line = prefix.iter().filter(|&&b| b == b'\n').count() + 1;
    let start = prefix
        .iter()
        .rposition(|&b| b == b'\n')
        .map_or(0, |i| i + 1);
    (
        line,
        String::from_utf8_lossy(&prefix[start..]).chars().count() + 1,
    )
}
