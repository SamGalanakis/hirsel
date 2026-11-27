//! The native targets. Every platform decision in the compiler derives from
//! one of these facts; matches on them are exhaustive, so a new target is a
//! compile error at each decision site.

use crate::apple_records::{IOS, IOS_SIMULATOR, MACOS};

/// EMB-2's deployment and toolchain values for one Apple target.
pub struct AppleRecord {
    pub family: &'static str,
    pub triple: &'static str,
    pub floor: &'static str,
    pub macho_platform: u32,
    pub platform_version_name: &'static str,
    pub llvm_os: &'static str,
    pub llvm_environment: &'static str,
    pub deployment_env: &'static str,
    pub swiftpm_platform: &'static str,
    pub xcframework_slice: &'static str,
    pub cpu: &'static str,
    pub cranelift_features: &'static [&'static str],
    pub llvm_triple: &'static str,
    pub cranelift_triple: &'static str,
    pub macho_minos: u32,
}

impl AppleRecord {
    pub const fn platform_version_args(&self) -> [&'static str; 4] {
        [
            "-platform_version",
            self.platform_version_name,
            self.floor,
            self.floor,
        ]
    }
}

/// The native product a build emits.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash, serde::Serialize)]
pub enum OutputKind {
    #[default]
    Executable,
    StaticLibrary,
}

impl std::str::FromStr for OutputKind {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "exe" => Ok(Self::Executable),
            "staticlib" => Ok(Self::StaticLibrary),
            _ => Err("--emit expects exe or staticlib"),
        }
    }
}

/// The named targets. They share FORM's little-endian, 64-bit layout
/// recipes; calling conventions, object format and OS facts derive from them.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub enum Target {
    X86_64Linux,
    Aarch64Linux,
    Aarch64Darwin,
    Aarch64Ios,
    Aarch64IosSim,
}

/// The instruction set a target runs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Arch {
    X86_64,
    Aarch64,
}

/// The operating system a target's images run on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Os {
    Linux,
    Macos,
    Ios,
}

/// The container a target's objects and images use.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObjectFormat {
    Elf,
    MachO,
}

impl Target {
    pub const ALL: [Self; 5] = [
        Self::X86_64Linux,
        Self::Aarch64Linux,
        Self::Aarch64Darwin,
        Self::Aarch64Ios,
        Self::Aarch64IosSim,
    ];

    /// The target's position in [`Self::ALL`], as a targeted selection numbers it (PROG-6).
    pub const fn index(self) -> usize {
        self as usize
    }

    /// Select a named target from its compiler triple.
    pub fn from_triple(triple: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|target| target.triple() == triple)
    }

    /// Whether this target builds the requested native product.
    pub fn admits(self, output: OutputKind) -> Result<(), &'static str> {
        match (self, output) {
            (
                Self::X86_64Linux | Self::Aarch64Linux | Self::Aarch64Darwin,
                OutputKind::Executable | OutputKind::StaticLibrary,
            ) => Ok(()),
            (Self::Aarch64Ios | Self::Aarch64IosSim, OutputKind::Executable) => {
                Err("iOS executables require an Xcode-signed app bundle (docs/spec/embed.md)")
            }
            (Self::Aarch64Ios | Self::Aarch64IosSim, OutputKind::StaticLibrary) => {
                Err("iOS static libraries land with slice 6 (docs/spec/embed.md)")
            }
        }
    }

    pub const fn triple(self) -> &'static str {
        match self {
            Self::X86_64Linux => "x86_64-unknown-linux-gnu",
            Self::Aarch64Linux => "aarch64-unknown-linux-gnu",
            Self::Aarch64Darwin => MACOS.triple,
            Self::Aarch64Ios => IOS.triple,
            Self::Aarch64IosSim => IOS_SIMULATOR.triple,
        }
    }

    pub const fn apple(self) -> Option<&'static AppleRecord> {
        match self {
            Self::X86_64Linux | Self::Aarch64Linux => None,
            Self::Aarch64Darwin => Some(&MACOS),
            Self::Aarch64Ios => Some(&IOS),
            Self::Aarch64IosSim => Some(&IOS_SIMULATOR),
        }
    }

    pub const fn llvm_triple(self) -> &'static str {
        match self.apple() {
            Some(apple) => apple.llvm_triple,
            None => self.triple(),
        }
    }

    pub const fn cranelift_triple(self) -> &'static str {
        match self.apple() {
            Some(apple) => apple.cranelift_triple,
            None => self.triple(),
        }
    }

    pub const fn arch(self) -> Arch {
        match self {
            Self::X86_64Linux => Arch::X86_64,
            Self::Aarch64Linux | Self::Aarch64Darwin | Self::Aarch64Ios | Self::Aarch64IosSim => {
                Arch::Aarch64
            }
        }
    }

    pub const fn os(self) -> Os {
        match self {
            Self::X86_64Linux | Self::Aarch64Linux => Os::Linux,
            Self::Aarch64Darwin => Os::Macos,
            Self::Aarch64Ios | Self::Aarch64IosSim => Os::Ios,
        }
    }

    pub const fn object_format(self) -> ObjectFormat {
        match self.os() {
            Os::Linux => ObjectFormat::Elf,
            Os::Macos | Os::Ios => ObjectFormat::MachO,
        }
    }

    /// The prefix the object format puts before a C symbol name.
    pub const fn symbol_prefix(self) -> &'static str {
        match self.object_format() {
            ObjectFormat::Elf => "",
            ObjectFormat::MachO => "_",
        }
    }

    /// log2 of the page size images are laid out for: 16 KiB on Apple Silicon.
    pub const fn page_log2(self) -> u8 {
        match self {
            Self::X86_64Linux | Self::Aarch64Linux => 12,
            Self::Aarch64Darwin | Self::Aarch64Ios | Self::Aarch64IosSim => 14,
        }
    }

    /// Keep LLVM's generic tuning. The named x86-64-v2 model splits unaligned
    /// AVX2 accesses in v3 functions even though their features allow them.
    /// The Apple CPU model implies the baseline LLVM features for the floor.
    pub const fn cpu(self) -> &'static str {
        match self {
            Self::X86_64Linux => "x86-64",
            Self::Aarch64Linux => "generic",
            Self::Aarch64Darwin => MACOS.cpu,
            Self::Aarch64Ios => IOS.cpu,
            Self::Aarch64IosSim => IOS_SIMULATOR.cpu,
        }
    }

    /// LLVM features for all functions, before per-version SIMD attributes.
    pub fn llvm_features(self) -> String {
        match self {
            Self::X86_64Linux => crate::isa::X86_64_BASE.llvm_features(),
            Self::Aarch64Linux => String::new(),
            // LLVM 23's AArch64CodeLayoutOpt enables pair alignment in a
            // process-wide option when the first subtarget opts in. Even a
            // whole image must opt out, or later split parts can align to 64.
            Self::Aarch64Darwin | Self::Aarch64Ios | Self::Aarch64IosSim => {
                "-align-cmp-csel-pairs".into()
            }
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
            Self::Aarch64Darwin | Self::Aarch64Ios | Self::Aarch64IosSim => CAbi::DarwinPcs,
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
