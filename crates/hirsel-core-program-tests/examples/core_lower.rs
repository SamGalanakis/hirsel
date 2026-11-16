//! Lower authored source outside the native-emission measurement window.
hirsel_guard::install!();
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let [input, output] = args.as_slice() else {
        return Err("expected input.hirsel output.core".into());
    };
    let (module, _) = hirsel_core_program_tests::SourceProgram::lower(
        &std::fs::read_to_string(input)?,
        Some("f"),
    );
    std::fs::create_dir_all(output)?;
    for (i, unit) in module.modules.iter().enumerate() {
        std::fs::write(
            std::path::Path::new(output).join(format!("unit-{i}.core")),
            hirsel_core_ir::text::print(unit),
        )?;
    }
    Ok(())
}
