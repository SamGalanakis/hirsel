mod admission;

pub use admission::{CheckedModule, Export, admit};
pub use wasmparser::ValType;

use wasmtime::{Engine, Result};

/// Configure the precompiler for a target and its Cranelift ISA baseline.
///
/// # Safety
/// The supplied flags must be valid for the target and supported by every host
/// that will run artifacts built by this engine.
pub unsafe fn engine(
    target: &str,
    cranelift_features: impl IntoIterator<Item = &'static str>,
) -> Result<Engine> {
    let mut config = super::engine_config();
    config.target(target)?.cranelift_nan_canonicalization(true);
    for feature in cranelift_features {
        // SAFETY: The caller guarantees the flags are valid for the target.
        unsafe { config.cranelift_flag_enable(feature) };
    }
    Engine::new(&config)
}

pub fn precompile(engine: &Engine, module: CheckedModule<'_>) -> Result<Vec<u8>> {
    engine.precompile_module(module.bytes)
}
