//! Emit a CoreIR object for code-generation cost measurements and C ABI experiments.
hirsel_guard::install!();

use hirsel_cranelift::core::{Options, emit_object_with_layouts};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    let source = std::fs::read_to_string(
        args.get(1)
            .ok_or("expected input.core output.o [--speed]")?,
    )?;
    let modules = if let Some(i) = args.iter().position(|a| a == "--front") {
        let _fixture_root = args.get(i + 1).ok_or("--front requires the fixture root")?;
        hirsel_core_program_tests::SourceProgram::lower(&source, None)
            .0
            .modules
    } else {
        vec![
            hirsel_core_ir::text::parse(&source)
                .map_err(|e| format!("line {}: {}", e.line, e.message))?,
        ]
    };
    let instructions: usize = modules
        .iter()
        .flat_map(|m| &m.functions)
        .filter_map(|f| f.body.as_ref())
        .map(|b| b.insts.len())
        .sum();
    let terms: usize = modules
        .iter()
        .flat_map(|m| &m.functions)
        .filter_map(|f| f.body.as_ref())
        .map(|b| b.terminators.len())
        .sum();
    println!(
        "instructions={instructions} terminators={terms} functions={}",
        modules.iter().map(|m| m.functions.len()).sum::<usize>()
    );
    if args.iter().any(|a| a == "--no-emit") {
        return Ok(());
    }
    for (unit, module) in modules.iter().enumerate() {
        let layouts =
            hirsel_core_layout::compute(&module.types).map_err(|e| format!("layout: {e:?}"))?;
        let object = emit_object_with_layouts(
            &module,
            &layouts,
            &Options {
                target: "x86_64-unknown-linux-gnu",
                optimize: args.iter().any(|a| a == "--speed"),
                debug_lines: false,
            },
            hirsel_core_ids::ContentHash([0x90; 32]),
        )?;
        std::fs::write(
            format!("{}.{}", args.get(2).ok_or("expected output.o")?, unit),
            object.bytes,
        )?;
    }
    Ok(())
}
