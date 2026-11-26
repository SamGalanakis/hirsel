//! Wasmtime configuration shared by the compiler and the image runtime.

use wasmtime::WasmFeatures;
#[cfg(any(feature = "compile", feature = "load"))]
use wasmtime::{Config, OperatorCost};

#[cfg(test)]
hirsel_guard::install!();

#[cfg(feature = "compile")]
pub mod compile;
#[cfg(feature = "load")]
pub mod load;
pub use wasmtime;

pub const ADMITTED_FEATURES: WasmFeatures = WasmFeatures::WASM2
    .union(WasmFeatures::TAIL_CALL)
    .union(WasmFeatures::EXTENDED_CONST)
    .difference(WasmFeatures::GC_TYPES);

#[cfg(any(feature = "compile", feature = "load"))]
fn engine_config() -> Config {
    let mut config = Config::new();
    config
        .wasm_features(WasmFeatures::all(), false)
        .wasm_features(ADMITTED_FEATURES, true)
        .consume_fuel(true)
        .operator_cost(OperatorCost::new())
        .epoch_interruption(false)
        .memory_init_cow(false)
        .signals_based_traps(true)
        .memory_reservation(1 << 32)
        .memory_guard_size(32 << 20)
        .max_wasm_stack(1 << 20)
        .async_stack_size(2 << 20);
    config
}
