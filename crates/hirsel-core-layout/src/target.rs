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

    /// Keep LLVM's generic tuning. The named x86-64-v2 model splits unaligned
    /// AVX2 accesses in v3 functions even though their features allow them.
    pub const fn cpu(self) -> &'static str {
        match self {
            Self::X86_64Linux => "x86-64",
            Self::Aarch64Linux => "generic",
        }
    }

    /// LLVM features for all functions, before per-version SIMD attributes.
    pub fn llvm_features(self) -> String {
        match self {
            Self::X86_64Linux => crate::isa::X86_64_BASE.llvm_features(),
            Self::Aarch64Linux => String::new(),
        }
    }
}

/// A C calling convention, as far as it decides how narrow integers fill
/// their register or stack slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CAbi {
    /// System V x86-64 as clang implements it: the producer of a bool, 8-bit or
    /// 16-bit argument or result extends it to 32 bits; bits 32..63 are unset.
    SysV64,
    /// AAPCS64 (Linux): bits above the type width are unspecified, so the
    /// consumer extends.
    Aapcs64,
    /// Apple's arm64 convention: like `SysV64`, the caller extends arguments
    /// and the callee extends results to 32 bits.
    DarwinPcs,
}

/// What the producer of a C value guarantees about the bits above its width.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CExtension {
    None,
    Zero,
    Sign,
}

impl Target {
    pub const fn c_abi(self) -> CAbi {
        match self {
            Self::X86_64Linux => CAbi::SysV64,
            Self::Aarch64Linux => CAbi::Aapcs64,
        }
    }
}

/// The one rule for a C argument or result of type `ty` under `abi`: every
/// backend marks C signatures, and their callers and callees, with this.
pub fn c_extension(abi: CAbi, ty: crate::Ty) -> CExtension {
    use crate::Scalar as S;
    let crate::Ty::Scalar(scalar) = ty else {
        return CExtension::None;
    };
    match abi {
        CAbi::Aapcs64 => CExtension::None,
        CAbi::SysV64 | CAbi::DarwinPcs => match scalar {
            S::Bool | S::U8 | S::U16 => CExtension::Zero,
            S::I8 | S::I16 => CExtension::Sign,
            S::Char | S::I32 | S::I64 | S::U32 | S::U64 | S::F32 | S::F64 => CExtension::None,
        },
    }
}
