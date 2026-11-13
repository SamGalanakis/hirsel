//! Emit a CoreIR object for code-generation cost measurements and C ABI experiments.
hirsel_guard::install!();

use hirsel_cranelift::core::{Options, emit_object};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    let source = std::fs::read_to_string(
        args.get(1)
            .ok_or("expected input.core output.o [--speed]")?,
    )?;
    let module = hirsel_core_ir::text::parse(&source)
        .map_err(|e| format!("line {}: {}", e.line, e.message))?;
    let object = emit_object(
        &module,
        &Options {
            target: "x86_64-unknown-linux-gnu",
            optimize: args.iter().any(|a| a == "--speed"),
        },
    )?;
    std::fs::write(args.get(2).ok_or("expected output.o")?, object.bytes)?;
    Ok(())
}
