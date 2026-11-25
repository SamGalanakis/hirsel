//! What a driver asks of this engine before emitting: the targets it
//! implements and whether it uses the producer's optimization facts.
use hirsel_core_layout::Target;

/// Whether to use the producer's optimization facts. Setting the diagnostic
/// switch suppresses every emitter-supplied fact, while retaining ABI attributes
/// and stack probes. Drivers include this choice in native object cache keys,
/// and the variable is on the driver's keyed environment list.
pub fn facts_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os(NO_FACTS_VARIABLE).is_none())
}
/// The diagnostic switch [`facts_enabled`] reads.
pub const NO_FACTS_VARIABLE: &str = "HIRSEL_LLVM_NO_FACTS";

/// Whether this engine implements the target's ABI and object format: every
/// Hirsel target, ELF and Mach-O alike.
pub fn supports_target(triple: &str) -> bool {
    Target::from_triple(triple).is_some()
}
