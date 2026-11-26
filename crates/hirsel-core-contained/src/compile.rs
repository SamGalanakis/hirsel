mod admission;

pub use admission::{CheckedModule, Export, admit};
pub use wasmparser::ValType;

use hirsel_core_layout::{Arch, Target, isa::X86_64_BASE};
use wasmtime::{Engine, Result};

pub fn engine(target: Target) -> Result<Engine> {
    let mut config = super::engine_config();
    config
        .target(target.triple())?
        .cranelift_nan_canonicalization(true);
    match target.arch() {
        Arch::X86_64 => {
            for feature in X86_64_BASE.cranelift_features() {
                // SAFETY: These flags are the native backend's baseline for this target.
                unsafe { config.cranelift_flag_enable(feature) };
            }
        }
        Arch::Aarch64 => {}
    }
    Engine::new(&config)
}

pub fn precompile(engine: &Engine, module: CheckedModule<'_>) -> Result<Vec<u8>> {
    engine.precompile_module(module.bytes)
}
