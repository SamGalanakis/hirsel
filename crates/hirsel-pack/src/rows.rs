//! Where each runtime row is in this host image (PACK-2 imports, kind 0).

use std::sync::OnceLock;

/// The rows only a law or measurement build of the runtime defines. A pack host has
/// none of them, so a module that imports one is refused.
macro_rules! address {
    (hirsel_core_law_observe) => {
        None
    };
    (hirsel_core_law_witness) => {
        None
    };
    (hirsel_core_observe_charge) => {
        None
    };
    (hirsel_core_test_drop_trace) => {
        None
    };
    (hirsel_core_test_inline_admission) => {
        None
    };
    (hirsel_core_test_probe_admission) => {
        None
    };
    ($symbol:ident) => {{
        unsafe extern "C" {
            static $symbol: u8;
        }
        Some((&raw const $symbol).addr())
    }};
}

macro_rules! addresses {
    ($($symbol:ident $class:ident,)*) => {
        vec![$(address!($symbol)),*]
    };
}

/// The address of row `index` in this image, or `None` when the image lacks it.
pub fn address(index: u32) -> Option<usize> {
    static ROWS: OnceLock<Vec<Option<usize>>> = OnceLock::new();
    // The runtime is linked for its rows alone here, so name it.
    use hirsel_core_rt as _;
    ROWS.get_or_init(|| hirsel_mod::rows!(addresses))
        .get(index as usize)
        .copied()
        .flatten()
}
