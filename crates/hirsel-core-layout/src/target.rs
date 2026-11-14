//! Native targets sharing FORM's little-endian, 64-bit layout recipes.

/// The supported Linux C ABIs. Layout recipes are identical on these targets;
/// scalar and aggregate calling conventions are selected by the backend.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Target {
    X86_64Linux,
    Aarch64Linux,
}

impl Target {
    /// Select a supported target from its compiler triple.
    pub fn from_triple(triple: &str) -> Option<Self> {
        match triple {
            "x86_64-unknown-linux-gnu" => Some(Self::X86_64Linux),
            "aarch64-unknown-linux-gnu" => Some(Self::Aarch64Linux),
            _ => None,
        }
    }

    pub const fn triple(self) -> &'static str {
        match self {
            Self::X86_64Linux => "x86_64-unknown-linux-gnu",
            Self::Aarch64Linux => "aarch64-unknown-linux-gnu",
        }
    }

    /// Baseline LLVM CPU, without host-specific features.
    pub const fn cpu(self) -> &'static str {
        match self {
            Self::X86_64Linux => "x86-64",
            Self::Aarch64Linux => "generic",
        }
    }
}
