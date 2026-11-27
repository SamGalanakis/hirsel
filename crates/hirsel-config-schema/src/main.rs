//! Host bootstrap exporter. The caller pins the accepted root before opening the document.
hirsel_guard::install!();
use hirsel_data::{Limits, decode};
use std::io::{Read, Write};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let usage = "usage: hirsel-config-export MODULE.ROOT FILE";
    let root = args.next().ok_or(usage)?;
    let path = args.next().ok_or(usage)?;
    if args.next().is_some() {
        return Err(usage.into());
    }
    let schema =
        hirsel_config_schema::schema(root.to_str().ok_or(usage)?).ok_or("unknown schema root")?;
    let limits = Limits::default();
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(limits.input_bytes as u64 + 1)
        .read_to_end(&mut bytes)?;
    let document = decode(&bytes, &[schema], limits)?;
    let mut out = std::io::stdout().lock();
    serde_json::to_writer(&mut out, &document.projection())?;
    out.write_all(b"\n")?;
    Ok(())
}
