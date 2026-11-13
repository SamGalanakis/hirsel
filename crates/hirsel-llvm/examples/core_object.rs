//! Developer entry for the CoreIR object-cost protocol.
hirsel_guard::install!();
use hirsel_intern::instructions::InstructionCounter;
use hirsel_llvm::core::{Optimization, emit_object_observed};
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
    let counter = InstructionCounter::open()?;
    let mut previous = counter.read()?;
    let source = std::fs::read_to_string(source)?;
    let module = hirsel_core_ir::text::parse(&source).map_err(|e| format!("{e:?}"))?;
    let now = counter.read()?;
    let mut rows = vec![("InputReadAndCoreParse".to_string(), now - previous)];
    previous = now;
    let mut owner = None;
    let bytes = emit_object_observed(&module, opt, |stage| {
        let now = counter.read().expect("exclusive instruction counter");
        if let Some(owner) = owner.take() {
            rows.push((owner, now - previous));
        }
        previous = now;
        owner = Some(format!("{stage:?}"));
    })?;
    let now = counter.read()?;
    rows.push((owner.unwrap(), now - previous));
    std::fs::write(output, bytes)?;
    for (stage, count) in rows {
        println!("{stage}\t{count}");
    }
    Ok(())
}
