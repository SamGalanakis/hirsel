//! Developer entry for the CoreIR object-cost protocol. No old compiler image is loaded.
hirsel_guard::install!();

use hirsel_llvm::core::{Optimization, emit_object};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let source = args
        .next()
        .ok_or("expected input.core output.o [--speed]")?;
    let output = args.next().ok_or("expected output.o")?;
    let opt = if args.next().as_deref() == Some(std::ffi::OsStr::new("--speed")) {
        Optimization::Speed
    } else {
        Optimization::None
    };
    let module = hirsel_core_ir::text::parse(&std::fs::read_to_string(source)?)
        .map_err(|e| format!("{e:?}"))?;
    std::fs::write(output, emit_object(&module, opt)?)?;
    Ok(())
}
