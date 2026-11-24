use std::fmt::{self, Write};

/// A function-local name. Numbered families share one prefix each.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(in crate::core) enum Local {
    Temp(usize),
    Value(usize),
    Address(usize),
    Slot(usize),
    FrameCharge(usize),
    FrameActive(usize),
    Param(usize),
    ParamField(usize, usize),
    Named(&'static str),
}
impl Local {
    pub const CONTEXT: Self = Self::Named("context");
    pub const DEST: Self = Self::Named("dest");
    /// A `Result` register body's slot for its other case (abi::Return::ok).
    pub const COLD: Self = Self::Named("cold");
    pub const CALLER_SITE: Self = Self::Named("caller_site");
    pub const SRET: Self = Self::Named("sret");
    pub const PAYLOAD: Self = Self::Named("payload");
    pub const CURSOR: Self = Self::Named("cursor");
    pub const WORKLIST: Self = Self::Named("worklist");
    pub const INPUT: Self = Self::Named("input");
    pub const RESULT: Self = Self::Named("result");
    pub fn write_name(self, out: &mut impl Write) -> fmt::Result {
        match self {
            Self::Temp(i) => write!(out, "t{i}"),
            Self::Value(i) => write!(out, "v{i}"),
            Self::Address(i) => write!(out, "a{i}"),
            Self::Slot(i) => write!(out, "s{i}"),
            Self::FrameCharge(i) => write!(out, "frame_charge{i}"),
            Self::FrameActive(i) => write!(out, "frame_active{i}"),
            Self::Param(i) => write!(out, "p{i}"),
            Self::ParamField(i, j) => write!(out, "p{i}_{j}"),
            Self::Named(name) => out.write_str(name),
        }
    }
}
impl fmt::Display for Local {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_char('%')?;
        self.write_name(f)
    }
}
