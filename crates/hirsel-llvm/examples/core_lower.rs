//! Lower authored source outside the native-emission measurement window.
hirsel_guard::install!();
#[path = "support/front.rs"]
mod front;
use std::path::Path;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let [input, output] = args.as_slice() else {
        return Err("expected input.hirsel output.core".into());
    };
    let core = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../core/front/core");
    let (module, _) = front::lower(&std::fs::read_to_string(input)?, &core, "f")?;
    std::fs::write(output, hirsel_core_ir::text::print(&module))?;
    Ok(())
}
