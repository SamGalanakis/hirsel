//! Format new-language source to stdout without changing the input file.
hirsel_guard::install!();
use hirsel_core_syntax::{ParseMode, Symbols, format, parse_bytes};
use std::{io::Write, process::ExitCode};
fn main() -> Result<ExitCode, Box<dyn std::error::Error>> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let path = args
        .first()
        .ok_or("usage: hirsel-core-syntax FILE [--signatures]")?;
    let bytes = std::fs::read(path)?;
    let mode = if args.iter().any(|a| a == "--signatures") {
        ParseMode::Signatures
    } else {
        ParseMode::Source
    };
    let parsed = parse_bytes(&bytes, mode, &mut Symbols::default());
    let Some(text) = format(&parsed) else {
        for d in &parsed.diagnostics {
            let offset = d.primary.start as usize;
            let line = bytes[..offset].iter().filter(|&&b| b == b'\n').count() + 1;
            eprintln!("{path}:{line}: {} {}", d.rule, d.message);
        }
        if let Some(r) = parsed.resource_refusal {
            eprintln!("{path}: incomplete compiler outcome: {r:?}");
        }
        return Ok(ExitCode::FAILURE);
    };
    std::io::stdout().lock().write_all(text.as_bytes())?;
    Ok(ExitCode::SUCCESS)
}
