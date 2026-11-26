//! Source-neutral native metadata, stored beside the executable code.
use hirsel_core_ids::{ContentHash, DefPath};
use serde::{Deserialize, Serialize};
use std::fmt::Write;

/// One cached native product. `bytes` is a complete, unchanged linker input;
/// optional debug rows are never sections of that input.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct NativeObject {
    pub bytes: Vec<u8>,
    pub debug: Option<NativeDebug>,
    /// A coverage object's map fragment, stored beside it under its key.
    #[serde(skip)]
    pub coverage: Option<crate::coverage::Fragment>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct NativeDebug {
    pub items: Vec<DefPath>,
    pub sections: Vec<CodeSection>,
    pub sequences: Vec<LineSequence>,
    /// Preorder; a parent index always precedes its child.
    pub scopes: Vec<DebugScope>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub enum RemarkKind {
    BoundsCheck,
    OverflowCheck,
    StepSync,
}

/// A pass decision in a native unit. Source owners use the same neutral byte
/// offsets or release ordinals as the unit's DWARF rows.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct CodegenRemark {
    pub definition: DefPath,
    pub linkage: String,
    pub owner: DefPath,
    pub start: u32,
    pub kind: RemarkKind,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CodeSection {
    /// Global hidden alias at the start of the producer's code section.
    pub symbol: String,
    pub size: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CodeRange {
    pub section: u32,
    pub begin: u64,
    pub end: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct NativeLocation {
    /// Index in `NativeDebug.items`.
    pub item: u32,
    /// Byte offset within the item, independent of its position in a file.
    pub start: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LineSequence {
    pub range: CodeRange,
    pub rows: Vec<LineRow>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LineRow {
    /// Offset from the start of `LineSequence.range.section`.
    pub offset: u64,
    /// None marks compiler-generated instructions (DWARF line zero).
    pub source: Option<NativeLocation>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ScopeKind {
    Function { linkage: String },
    Inline,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DebugScope {
    pub parent: Option<u32>,
    /// Abstract function scope, when this is a physical or inline instance.
    pub origin: Option<u32>,
    pub kind: ScopeKind,
    pub name: String,
    pub declaration: Option<NativeLocation>,
    pub call: Option<NativeLocation>,
    /// Empty for an abstract function declaration.
    pub ranges: Vec<CodeRange>,
}

/// The driver supplies an engine-free unit key (never the cache key, which
/// names the libLLVM file); aliases do not hash the emitted object or depend
/// on any absolute source coordinate.
pub fn debug_symbol(unit: ContentHash, section: u32) -> String {
    let mut name = String::from("__hirsel_debug_");
    for byte in unit.0 {
        write!(name, "{byte:02x}").unwrap();
    }
    write!(name, "_{section}").unwrap();
    name
}
