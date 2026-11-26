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

/// The size, in Core instructions, above which a release unit is lowered and
/// optimised in partitions (`emit_release_unit`), each of about this many,
/// with the large pipeline, which keeps call boundaries. A Core instruction
/// lowers to 3.4 to 3.9 LLVM instructions (notes' 16 partitions), so a unit
/// within the bound holds under about a million LLVM instructions, the most
/// the full pipeline optimises in one module within the memory budget: its
/// inliner and path-sensitive analyses can multiply a module that already
/// occupies hundreds of MiB. [`PARTITION_VARIABLE`] sets another bound, so
/// that a test runs a small unit partitioned. Drivers include it in the
/// release unit's key, and the variable is on the driver's keyed environment
/// list.
pub fn partition_instructions() -> usize {
    static BOUND: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *BOUND.get_or_init(|| {
        std::env::var(PARTITION_VARIABLE)
            .ok()
            .and_then(|bound| bound.parse().ok())
            .filter(|&bound| bound > 0)
            .unwrap_or(250_000)
    })
}
/// The switch [`partition_instructions`] reads.
pub const PARTITION_VARIABLE: &str = "HIRSEL_LLVM_PARTITION_INSTRUCTIONS";

/// Whether this engine implements the target's ABI and object format: every
/// Hirsel target, ELF and Mach-O alike.
pub fn supports_target(triple: &str) -> bool {
    Target::from_triple(triple).is_some()
}
