//! The capped module worker receives its host's typed export selection on stdin.
use super::{CheckRequest, Driver, Meter, NativeOptions, Outcome, conclude, output};
use hirsel_core_ids::{DefPath, module_build::ModuleBuild};
use std::io::{self, BufRead};

pub(super) fn exports() -> io::Result<Vec<DefPath>> {
    if !super::launch::module_worker() {
        return Ok(Vec::new());
    }
    io::stdin()
        .lock()
        .lines()
        .map(|line| {
            let line = line?;
            DefPath::from_full(&line).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("invalid module export {line:?}"),
                )
            })
        })
        .collect()
}

pub(super) fn build(
    request: CheckRequest,
    exports: &[DefPath],
    meter: &mut Meter,
    options: &NativeOptions,
    sources: &crate::SourceFiles,
) -> Result<Outcome, Box<dyn std::error::Error>> {
    let product = Driver::default().check_pack_exports(request, exports, meter)?;
    let reply = if !product.accepted() {
        output::render_check_diagnostics(&product, sources)?;
        if product.resources.is_empty() && product.incomplete.is_empty() {
            ModuleBuild::Rejected
        } else {
            ModuleBuild::ToolRefusal
        }
    } else {
        match product.build_pack_module(exports, options) {
            Ok(bytes) => {
                std::fs::write(&options.output, bytes)?;
                ModuleBuild::Built
            }
            Err(crate::NativeStop::Module(reason) | crate::NativeStop::Entry(reason)) => {
                ModuleBuild::Unsupported(reason)
            }
            Err(crate::NativeStop::Resource(refusal)) => {
                let resources = product.resources_for([&refusal])?;
                super::launch::render_diagnostics(sources, &[], &[], &resources);
                ModuleBuild::ToolRefusal
            }
            Err(stop) => {
                eprintln!("{stop:?}");
                ModuleBuild::ToolRefusal
            }
        }
    };
    println!("{}", serde_json::to_string(&reply)?);
    conclude(Outcome::Exit(i32::from(!matches!(
        reply,
        ModuleBuild::Built
    ))))
}
