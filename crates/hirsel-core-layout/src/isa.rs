//! The x86-64 psABI levels used for SEM-11. The compiler only enables
//! features in the same row that the runtime tests.

#[derive(Clone, Copy, Debug)]
enum Word {
    BasicC,
    BasicD,
    ExtendedC,
    ExtendedD,
    FeaturesB,
    AvxState,
}

#[derive(Clone, Copy, Debug)]
struct Feature {
    word: Word,
    bit: u32,
    llvm: Option<&'static str>,
    cranelift: Option<&'static str>,
}

const fn cpu(word: Word, bit: u32, llvm: &'static str) -> Feature {
    Feature {
        word,
        bit,
        llvm: Some(llvm),
        cranelift: None,
    }
}

impl Feature {
    const fn with_cranelift(mut self, flag: &'static str) -> Self {
        self.cranelift = Some(flag);
        self
    }
}

use Word::*;

const BASE: &[Feature] = &[
    cpu(BasicD, 0, "x87"),
    cpu(BasicD, 8, "cx8"),
    cpu(BasicD, 15, "cmov"),
    cpu(BasicD, 23, "mmx"),
    // Linux x86-64 already runs with CR4.OSFXSR. CPUID also advertises FXSR.
    cpu(BasicD, 24, "fxsr"),
    cpu(BasicD, 25, "sse"),
    cpu(BasicD, 26, "sse2"),
    Feature {
        word: ExtendedD,
        bit: 11,
        llvm: None,
        cranelift: None,
    }, // SYSCALL/SYSRET
];
const SSE4: &[Feature] = &[
    cpu(BasicC, 0, "sse3").with_cranelift("has_sse3"),
    cpu(BasicC, 9, "ssse3").with_cranelift("has_ssse3"),
    cpu(BasicC, 13, "cx16").with_cranelift("has_cmpxchg16b"),
    cpu(BasicC, 19, "sse4.1").with_cranelift("has_sse41"),
    cpu(BasicC, 20, "sse4.2").with_cranelift("has_sse42"),
    // LLVM tracks CRC32 separately, but SSE4.2's CPUID bit covers the instruction.
    cpu(BasicC, 20, "crc32"),
    cpu(BasicC, 23, "popcnt").with_cranelift("has_popcnt"),
    cpu(ExtendedC, 0, "sahf"),
];
const AVX2: &[Feature] = &[
    cpu(BasicC, 12, "fma"),
    cpu(BasicC, 22, "movbe"),
    cpu(BasicC, 26, "xsave"),
    cpu(BasicC, 28, "avx"),
    cpu(BasicC, 29, "f16c"),
    cpu(ExtendedC, 5, "lzcnt"),
    cpu(FeaturesB, 3, "bmi"),
    cpu(FeaturesB, 5, "avx2"),
    cpu(FeaturesB, 8, "bmi2"),
    Feature {
        word: AvxState,
        bit: 0,
        llvm: None,
        cranelift: None,
    }, // OSXSAVE, XMM and YMM state
];

#[derive(Clone, Copy, Debug)]
pub struct Level {
    pub number: u8,
    pub name: &'static str,
    features: &'static [&'static [Feature]],
}

pub const X86_64_LEVELS: &[Level] = &[
    Level {
        number: 2,
        name: "x86-64-v2",
        features: &[BASE, SSE4],
    },
    Level {
        number: 3,
        name: "x86-64-v3",
        features: &[BASE, SSE4, AVX2],
    },
];

pub const X86_64_BASE: Level = X86_64_LEVELS[0];

impl Level {
    fn features(self) -> impl Iterator<Item = Feature> {
        self.features.iter().flat_map(|group| group.iter().copied())
    }

    pub fn llvm_features(self) -> String {
        let mut flags = String::new();
        for name in self.features().filter_map(|feature| feature.llvm) {
            if !flags.is_empty() {
                flags.push(',');
            }
            flags.push('+');
            flags.push_str(name);
        }
        flags
    }

    pub fn cranelift_features(self) -> impl Iterator<Item = &'static str> {
        self.features().filter_map(|feature| feature.cranelift)
    }
}

#[cfg(any(target_arch = "x86_64", test))]
#[derive(Default)]
struct Registers {
    basic_c: u32,
    basic_d: u32,
    extended_c: u32,
    extended_d: u32,
    features_b: u32,
    avx_state: bool,
}
#[cfg(any(target_arch = "x86_64", test))]
impl Registers {
    fn supports(&self, feature: Feature) -> bool {
        let value = match feature.word {
            BasicC => self.basic_c,
            BasicD => self.basic_d,
            ExtendedC => self.extended_c,
            ExtendedD => self.extended_d,
            FeaturesB => self.features_b,
            AvxState => u32::from(self.avx_state),
        };
        value & (1 << feature.bit) != 0
    }

    fn level(&self, cap: u8) -> Option<u8> {
        let cap = cap.max(X86_64_BASE.number);
        X86_64_LEVELS
            .iter()
            .rev()
            .find(|level| {
                level.number <= cap && level.features().all(|feature| self.supports(feature))
            })
            .map(|level| level.number)
    }
}

#[cfg(target_arch = "x86_64")]
pub fn detect_x86_64(cap: u8) -> Option<u8> {
    use std::arch::x86_64::{__cpuid, __cpuid_count, _xgetbv};

    let maximum = __cpuid(0).eax;
    let extended = __cpuid(0x8000_0000).eax;
    let basic = __cpuid(1);
    // SAFETY: the OSXSAVE bit means the OS has enabled the XGETBV instruction.
    let avx_state = basic.ecx & (1 << 27) != 0 && unsafe { _xgetbv(0) } & 6 == 6;
    let mut registers = Registers {
        basic_c: basic.ecx,
        basic_d: basic.edx,
        avx_state,
        ..Registers::default()
    };
    if maximum >= 7 {
        registers.features_b = __cpuid_count(7, 0).ebx;
    }
    if extended >= 0x8000_0001 {
        let values = __cpuid(0x8000_0001);
        registers.extended_c = values.ecx;
        registers.extended_d = values.edx;
    }
    registers.level(cap)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn x86_64_detection_and_emission_share_psabi_levels() {
        // Columns from the x86-64 psABI, using LLVM's spelling. Linux owns
        // OSFXSR and SYSCALL/SYSRET. The AVX OS-state check has no LLVM flag.
        let expected = [
            "cmov crc32 cx16 cx8 fxsr mmx popcnt sahf sse sse2 sse3 sse4.1 sse4.2 ssse3 x87",
            "avx avx2 bmi bmi2 cmov crc32 cx16 cx8 f16c fma fxsr lzcnt mmx movbe popcnt sahf sse sse2 sse3 sse4.1 sse4.2 ssse3 x87 xsave",
        ];
        let ids: Vec<_> = X86_64_LEVELS.iter().map(|row| row.number).collect();
        assert_eq!(ids, [2, 3]);
        assert_eq!(X86_64_LEVELS.len(), expected.len());
        for (level, expected) in X86_64_LEVELS.iter().zip(expected) {
            let flags = level.llvm_features();
            let mut flags: Vec<_> = flags
                .split(',')
                .map(|flag| flag.strip_prefix('+').unwrap())
                .collect();
            flags.sort_unstable();
            assert_eq!(flags.join(" "), expected);

            let full = || Registers {
                basic_c: u32::MAX,
                basic_d: u32::MAX,
                extended_c: u32::MAX,
                extended_d: u32::MAX,
                features_b: u32::MAX,
                avx_state: true,
            };
            assert_eq!(full().level(level.number), Some(level.number));
            for feature in level.features() {
                let mut missing = full();
                let word = match feature.word {
                    BasicC => &mut missing.basic_c,
                    BasicD => &mut missing.basic_d,
                    ExtendedC => &mut missing.extended_c,
                    ExtendedD => &mut missing.extended_d,
                    FeaturesB => &mut missing.features_b,
                    AvxState => {
                        missing.avx_state = false;
                        assert!(
                            missing
                                .level(u8::MAX)
                                .is_none_or(|actual| actual < level.number)
                        );
                        continue;
                    }
                };
                *word &= !(1 << feature.bit);
                assert!(!missing.supports(feature));
                assert!(
                    missing
                        .level(u8::MAX)
                        .is_none_or(|actual| actual < level.number)
                );
            }
        }
    }
}
