//! Native targets sharing FORM's little-endian, 64-bit layout recipes.

/// The supported C ABIs: Linux on x86-64 and AArch64, and macOS on AArch64.
/// Layout recipes are identical on these targets; scalar and aggregate
/// calling conventions are selected by the backend.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Target {
    X86_64Linux,
    Aarch64Linux,
    Aarch64Darwin,
}

impl Target {
    /// Select a supported target from its compiler triple.
    pub fn from_triple(triple: &str) -> Option<Self> {
        match triple {
            "x86_64-unknown-linux-gnu" => Some(Self::X86_64Linux),
            "aarch64-unknown-linux-gnu" => Some(Self::Aarch64Linux),
            "aarch64-apple-darwin" => Some(Self::Aarch64Darwin),
            _ => None,
        }
    }

    pub const fn triple(self) -> &'static str {
        match self {
            Self::X86_64Linux => "x86_64-unknown-linux-gnu",
            Self::Aarch64Linux => "aarch64-unknown-linux-gnu",
            Self::Aarch64Darwin => "aarch64-apple-darwin",
        }
    }

    /// Keep LLVM's generic tuning. The named x86-64-v2 model splits unaligned
    /// AVX2 accesses in v3 functions even though their features allow them.
    pub const fn cpu(self) -> &'static str {
        match self {
            Self::X86_64Linux => "x86-64",
            Self::Aarch64Linux => "generic",
            Self::Aarch64Darwin => "apple-m1",
        }
    }

    /// LLVM features for all functions, before per-version SIMD attributes.
    pub fn llvm_features(self) -> String {
        match self {
            Self::X86_64Linux => crate::isa::X86_64_BASE.llvm_features(),
            Self::Aarch64Linux | Self::Aarch64Darwin => String::new(),
        }
    }
}

impl Target {
    /// The native target a request builds for when it names none: macOS on
    /// Apple silicon builds for itself, and every other host for x86-64
    /// Linux, the release target.
    pub fn default_native() -> Self {
        if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            Self::Aarch64Darwin
        } else {
            Self::X86_64Linux
        }
    }

    pub const fn is_aarch64(self) -> bool {
        matches!(self, Self::Aarch64Linux | Self::Aarch64Darwin)
    }

    pub const fn is_darwin(self) -> bool {
        matches!(self, Self::Aarch64Darwin)
    }
}
