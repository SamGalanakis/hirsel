//! `.hirsel-mod` v1 (PACK-2): the pointer-free release image a pack host installs.
//!
//! [`Image`] is what a compiler writes and [`Module`] is what a host reads. A module
//! arrives from a content store or a socket, so [`Module::parse`] checks every length,
//! offset, count, kind and identifier before a loader uses it.

use hirsel_core_ids::ContentHash;
use hirsel_core_layout::{Arch, Target};

mod read;
mod rows;
mod write;

pub use read::{Malformed, Module};
pub use rows::ROWS;

#[cfg(test)]
hirsel_guard::install!();
#[cfg(test)]
mod tests;

pub const MAGIC: [u8; 8] = *b"HIRMOD\0\x01";
pub const HEADER: usize = 232;
pub const SECTIONS: usize = 13;
/// Policy bits version 1 defines: tape recording, speculative index masking.
pub const POLICY: u64 = 0b11;
pub const NO_DESCRIPTOR: u32 = u32::MAX;
/// The fixed words of a PROG-9 descriptor in a module's data: its code address, the
/// length of its identity bytes and its revision. The identity bytes follow them.
pub const DESCRIPTOR: usize = 24;

pub(crate) const IMPORT: usize = 40;
pub(crate) const FIXUP: usize = 16;
pub(crate) const ENTRY: usize = 80;
pub(crate) const SPAN: usize = 8;

/// An import row's class. A loader's mask is a set of these (PACK-7).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Class {
    Pure = 0,
    Metered = 1,
    Task = 2,
    Provider = 3,
    Foreign = 4,
    Exec = 5,
    Launch = 6,
}

/// A set of import classes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Mask(u8);

impl Mask {
    /// The mask of a pure-Hirsel pool.
    pub const PURE: Self = Self::of(&[Class::Pure, Class::Metered, Class::Task]);
    pub const fn of(classes: &[Class]) -> Self {
        let mut bits = 0;
        let mut i = 0;
        while i < classes.len() {
            bits |= 1 << classes[i] as u8;
            i += 1;
        }
        Self(bits)
    }
    pub const fn admits(self, class: Class) -> bool {
        self.0 & (1 << class as u8) != 0
    }
}

/// The row a symbol names.
pub fn row(symbol: &str) -> Option<u32> {
    ROWS.binary_search_by(|(name, _)| (*name).cmp(symbol))
        .ok()
        .map(|i| i as u32)
}

/// The digest of the row table, one input of a runtime ABI digest.
pub fn rows_digest() -> ContentHash {
    let classes: Vec<[u8; 1]> = ROWS.iter().map(|(_, c)| [*c as u8]).collect();
    let parts = ROWS
        .iter()
        .zip(&classes)
        .flat_map(|((name, _), class)| [name.as_bytes(), &class[..]]);
    ContentHash::of_parts("hirsel.mod-rows.v1", parts)
}

/// The runtime ABI revision: the layouts and calling rules loaded code shares with the
/// runtime. A change to either adds one.
/// Revision 4 introduced PACK-14/15 typed export adapters (distinct from the
/// program-root ABI). Revision 5 introduced PACK-16 `Held` lowering, literal ownership
/// by an image's `CauseData` and stop custody. Revision 6 admits ordered Arc release
/// under the same metered class as the other Arc operations.
pub const ABI: u32 = 6;

/// The runtime ABI digest of this build: [`ABI`] and the row table.
pub fn abi() -> ContentHash {
    ContentHash::of_parts(
        "hirsel.mod-abi.v1",
        [&ABI.to_le_bytes()[..], &rows_digest().0],
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Backend {
    Cranelift = 0,
    Llvm = 1,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Header {
    pub target: Target,
    pub backend: Backend,
    pub policy: u64,
    pub abi: ContentHash,
    pub compiler: ContentHash,
    pub coreir: ContentHash,
    pub text_align: u32,
    pub data_align: u32,
}

impl Header {
    /// The module key: two modules with one key are the same code.
    pub fn key(&self) -> ContentHash {
        ContentHash::of_parts(
            "hirsel.mod-key.v1",
            [
                &(self.target.index() as u32).to_le_bytes()[..],
                &[self.backend as u8],
                &self.policy.to_le_bytes(),
                &self.abi.0,
                &self.compiler.0,
                &self.coreir.0,
            ],
        )
    }
}

/// The module digest of a file's bytes.
pub fn digest(bytes: &[u8]) -> ContentHash {
    ContentHash::of_parts("hirsel.mod.v1", [bytes])
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Import {
    Row(u32),
    Instance(ContentHash),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Kind {
    Abs64Text = 1,
    Abs64Data = 2,
    Rel32Data = 3,
    PageData = 4,
    Low12AddData = 5,
    Low12LoadData = 6,
}

impl Kind {
    pub(crate) fn of(byte: u8) -> Option<Self> {
        Some(match byte {
            1 => Self::Abs64Text,
            2 => Self::Abs64Data,
            3 => Self::Rel32Data,
            4 => Self::PageData,
            5 => Self::Low12AddData,
            6 => Self::Low12LoadData,
            _ => return None,
        })
    }
    /// The patched field's width in bytes.
    pub const fn width(self) -> u32 {
        match self {
            Self::Abs64Text | Self::Abs64Data => 8,
            Self::Rel32Data | Self::PageData | Self::Low12AddData | Self::Low12LoadData => 4,
        }
    }
    /// Whether the kind is one `arch`'s text may carry.
    pub(crate) const fn text(self, arch: Arch) -> bool {
        let arm = matches!(
            self,
            Self::PageData | Self::Low12AddData | Self::Low12LoadData
        );
        match arch {
            Arch::X86_64 => matches!(self, Self::Rel32Data),
            Arch::Aarch64 => arm,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Fixup {
    pub site: u32,
    pub kind: Kind,
    /// x86-64 `Rel32Data` only: bytes from the field to the end of its instruction.
    pub bias: u8,
    pub addend: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Entry {
    pub identity: ContentHash,
    pub signature: ContentHash,
    pub text: u32,
    pub descriptor: u32,
    pub waits: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Span {
    pub start: u32,
    pub length: u32,
}

/// A module to write. `data` excludes the import slots, which [`Image::encode`] puts
/// first; every data offset in a fixup or entry counts them.
#[derive(Clone, Debug)]
pub struct Image {
    pub header: Header,
    pub text: Vec<u8>,
    pub data: Vec<u8>,
    pub imports: Vec<Import>,
    pub text_fixups: Vec<Fixup>,
    pub data_fixups: Vec<Fixup>,
    pub entries: Vec<Entry>,
    pub spans: Vec<Span>,
    pub cfi: Vec<u8>,
    pub sites: Vec<u8>,
    pub names: Vec<String>,
}
