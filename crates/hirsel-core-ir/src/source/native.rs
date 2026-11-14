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
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct NativeDebug {
    pub items: Vec<DefPath>,
    pub sections: Vec<CodeSection>,
    pub sequences: Vec<LineSequence>,
    /// Preorder; a parent index always precedes its child.
    pub scopes: Vec<DebugScope>,
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

/// The driver supplies its existing unit key; aliases do not hash the emitted
/// object or depend on any absolute source coordinate.
pub fn debug_symbol(unit: ContentHash, section: u32) -> String {
    let mut name = String::from("__hirsel_debug_");
    for byte in unit.0 {
        write!(name, "{byte:02x}").unwrap();
    }
    write!(name, "_{section}").unwrap();
    name
}
