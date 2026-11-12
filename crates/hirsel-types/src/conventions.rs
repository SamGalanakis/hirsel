//! Conventions: the std@1 definitions the compiler demands without the
//! program naming them (f-string steps, SEM-3). This is the one place their
//! paths are spelled. The checker demands them only through `Convention`, and
//! an image's K section exports them at construction, each checked against
//! the image's name index (N), so a request can plan for them before any
//! body is checked.
use crate::Primitive;
use hirsel_syntax::{DefPath, ItemPath, PackageId};

/// One implicit demand. Construct appends with `append` and `append_spec`,
/// which admit only the primitives f-strings format.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Convention {
    /// Append a primitive's value to a builder.
    Append(FormatPrimitive),
    /// Append under a written format spec; never char.
    AppendSpec(FormatPrimitive),
    /// Pad a hole to its width; numbers pad on the right.
    Pad { numeric: bool },
    /// Finish a builder whose bytes are not known to be valid UTF-8.
    FinishValid,
}

/// The primitives an f-string hole formats directly.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize)]
pub struct FormatPrimitive(Primitive);

impl FormatPrimitive {
    pub fn new(primitive: Primitive) -> Option<Self> {
        use Primitive::*;
        match primitive {
            String | Bool | U8 | U16 | U32 | U64 | U128 | I8 | I16 | I32 | I64 | I128 | F32
            | F64 | Char => Some(Self(primitive)),
            Bytes | Never => None,
        }
    }
    pub fn primitive(self) -> Primitive {
        self.0
    }
    /// The std@1 module holding this primitive's operations.
    fn module(self) -> &'static str {
        use Primitive::*;
        match self.0 {
            String => "text",
            Bool => "bool",
            U8 => "u8",
            U16 => "u16",
            U32 => "u32",
            U64 => "u64",
            U128 => "u128",
            I8 => "i8",
            I16 => "i16",
            I32 => "i32",
            I64 => "i64",
            I128 => "i128",
            F32 => "f32",
            F64 => "f64",
            Char => "char",
            Bytes | Never => unreachable!("FormatPrimitive::new admits neither"),
        }
    }
    const ALL: [Primitive; 15] = {
        use Primitive::*;
        [
            String, Bool, U8, U16, U32, U64, U128, I8, I16, I32, I64, I128, F32, F64, Char,
        ]
    };
}

impl Convention {
    pub fn append(primitive: Primitive) -> Option<Self> {
        FormatPrimitive::new(primitive).map(Self::Append)
    }
    /// A char hole takes no spec (SEM-3), so char has no spec append.
    pub fn append_spec(primitive: Primitive) -> Option<Self> {
        FormatPrimitive::new(primitive)
            .filter(|p| p.0 != Primitive::Char)
            .map(Self::AppendSpec)
    }

    /// The package every convention lives in.
    pub fn package() -> PackageId {
        PackageId {
            name: "std".into(),
            version: "1".into(),
        }
    }

    /// The definition this convention demands.
    pub fn path(self) -> DefPath {
        let (module, name) = match self {
            Self::Append(p) if p.0 == Primitive::String => ("text", "append"),
            Self::Append(p) => (p.module(), "format_into"),
            Self::AppendSpec(p) => (p.module(), "format_into_spec"),
            Self::Pad { numeric: true } => ("text", "pad_number_from"),
            Self::Pad { numeric: false } => ("text", "pad_from"),
            Self::FinishValid => ("text", "finish_valid"),
        };
        DefPath(ItemPath {
            package: Self::package(),
            modules: vec![module.into()],
            name: name.into(),
        })
    }

    /// The written `(value, builder)` argument ordinals of an append without
    /// a spec: text's append takes the builder first.
    pub fn append_ordinals(primitive: FormatPrimitive) -> [usize; 2] {
        if primitive.0 == Primitive::String {
            [1, 0]
        } else {
            [0, 1]
        }
    }

    /// Every convention, for K's export at construction.
    pub fn all() -> impl Iterator<Item = Self> {
        FormatPrimitive::ALL
            .into_iter()
            .map(FormatPrimitive)
            .flat_map(|p| {
                [Some(Self::Append(p)), Self::append_spec(p.0)]
                    .into_iter()
                    .flatten()
            })
            .chain([
                Self::Pad { numeric: true },
                Self::Pad { numeric: false },
                Self::FinishValid,
            ])
    }
}
