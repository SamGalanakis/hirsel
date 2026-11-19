use hirsel_core_layout::isa::{X86_64_LEVELS, detect_x86_64};
use std::sync::{
    Once,
    atomic::{AtomicU8, Ordering},
};

#[unsafe(no_mangle)]
pub static hirsel_core_isa_level: AtomicU8 = AtomicU8::new(X86_64_LEVELS[0].number);

/// Referenced by the executable initializer only when the program has SIMD versions.
#[unsafe(no_mangle)]
pub extern "C" fn hirsel_core_init_isa() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let cap = std::env::var("HIRSEL_ISA_CAP")
            .ok()
            .and_then(|text| text.parse::<u8>().ok())
            .unwrap_or(u8::MAX);
        let Some(level) = detect_x86_64(cap) else {
            eprintln!("not started: the processor supports no configured x86-64 SIMD level");
            std::process::exit(crate::process::NOT_STARTED.into());
        };
        hirsel_core_isa_level.store(level, Ordering::Relaxed);
    });
}
