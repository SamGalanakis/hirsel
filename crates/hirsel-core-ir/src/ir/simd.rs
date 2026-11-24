/// The vector value operations of SEM-11.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum SimdOp {
    WrappingAdd,
    WrappingSub,
    SaturatingAdd,
    SaturatingSub,
    Eq,
    Lt,
    Splat,
    From,
    Swizzle,
    Cast,
    Any,
    Bitmask,
    BitAnd,
    BitOr,
    BitXor,
    BitNot,
    Shl,
    Shr,
    Lanes,
    WrappingMul,
    MaddI16,
    MulhiI16,
    PackSaturating,
}
impl SimdOp {
    pub const ALL: [Self; 23] = [
        Self::WrappingAdd,
        Self::WrappingSub,
        Self::SaturatingAdd,
        Self::SaturatingSub,
        Self::Eq,
        Self::Lt,
        Self::Splat,
        Self::From,
        Self::Swizzle,
        Self::Cast,
        Self::Any,
        Self::Bitmask,
        Self::BitAnd,
        Self::BitOr,
        Self::BitXor,
        Self::BitNot,
        Self::Shl,
        Self::Shr,
        Self::Lanes,
        Self::WrappingMul,
        Self::MaddI16,
        Self::MulhiI16,
        Self::PackSaturating,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::WrappingAdd => "wrapping_add",
            Self::WrappingSub => "wrapping_sub",
            Self::SaturatingAdd => "saturating_add",
            Self::SaturatingSub => "saturating_sub",
            Self::Eq => "eq",
            Self::Lt => "lt",
            Self::Splat => "splat",
            Self::From => "from",
            Self::Swizzle => "swizzle",
            Self::Cast => "cast",
            Self::Any => "any",
            Self::Bitmask => "bitmask",
            Self::BitAnd => "bit_and",
            Self::BitOr => "bit_or",
            Self::BitXor => "bit_xor",
            Self::BitNot => "bit_not",
            Self::Shl => "shl",
            Self::Shr => "shr",
            Self::Lanes => "lanes",
            Self::WrappingMul => "wrapping_mul",
            Self::MaddI16 => "madd_i16",
            Self::MulhiI16 => "mulhi_i16",
            Self::PackSaturating => "pack_saturating",
        }
    }

    pub const fn binary(self) -> bool {
        matches!(
            self,
            Self::WrappingAdd
                | Self::WrappingSub
                | Self::SaturatingAdd
                | Self::SaturatingSub
                | Self::Eq
                | Self::Lt
                | Self::Swizzle
                | Self::BitAnd
                | Self::BitOr
                | Self::BitXor
                | Self::Shl
                | Self::Shr
                | Self::WrappingMul
                | Self::MaddI16
                | Self::MulhiI16
                | Self::PackSaturating
        )
    }

    pub const fn can_fail(self) -> bool {
        matches!(self, Self::Shl | Self::Shr)
    }
}
