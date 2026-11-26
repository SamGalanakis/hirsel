//! The printed-module oracle (ORACLE.md).

use super::*;

/// The printed-module oracle (ORACLE.md), a check that never changes emission:
/// the raw native module must print exactly as the text producer's module
/// parsed in a fresh context. Each compared unit appends a row to `units.tsv`;
/// a mismatch keeps its text, both prints and the first differing line, and
/// fails this unit's compilation so no later object equality can hide it.
#[cfg(hirsel_lazy_core)]
pub(super) fn printed_oracle(
    dir: &std::path::Path,
    module: &Module,
    native: &str,
    anonymous: bool,
    mut text: sink::TextSink,
    produce: impl FnOnce(&mut sink::TextSink) -> Result<()>,
) -> Result<()> {
    use std::io::Write as _;
    produce(&mut text)?;
    let text = text.finish();
    let parsed = crate::construction::print_text(&text, anonymous).map_err(Error::Llvm)?;
    let identity = module
        .instances
        .first()
        .map_or_else(String::new, |i| i.definition.to_string());
    let digest = hirsel_core_ids::ContentHash::of_bytes(text.as_bytes()).0[..8]
        .iter()
        .fold(String::new(), |mut s, b| {
            write!(s, "{b:02x}").unwrap();
            s
        });
    let difference = native
        .lines()
        .zip(parsed.lines())
        .position(|(a, b)| a != b)
        .or_else(|| (native.lines().count() != parsed.lines().count()).then_some(0));
    let io = |e: std::io::Error| Error::Llvm(format!("printed oracle {}: {e}", dir.display()));
    std::fs::create_dir_all(dir).map_err(io)?;
    let row = format!(
        "{}\t{digest}\t{identity}\t{}\t{}\n",
        if difference.is_some() { "DIFF" } else { "SAME" },
        native.lines().count(),
        difference.map_or(String::from("-"), |l| (l + 1).to_string()),
    );
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("units.tsv"))
        .and_then(|mut f| f.write_all(row.as_bytes()))
        .map_err(io)?;
    let Some(line) = difference else {
        return Ok(());
    };
    for (suffix, body) in [
        ("ll", &text),
        ("native.ll", &native.to_owned()),
        ("parsed.ll", &parsed),
    ] {
        std::fs::write(dir.join(format!("{digest}.{suffix}")), body).map_err(io)?;
    }
    Err(Error::Llvm(format!(
        "printed oracle: unit {identity} ({digest}) differs at printed line {}",
        line + 1
    )))
}
