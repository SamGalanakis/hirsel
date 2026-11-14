//! Current source bindings for native diagnostics, separate from semantic bodies.
use hirsel_core_ids::DefPath;
use std::collections::BTreeMap;
use std::fmt::Write;

#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SourceMap {
    pub files: Vec<SourceFile>,
    pub items: BTreeMap<DefPath, ItemSource>,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SourceFile {
    pub path: String,
    pub text: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ItemSource {
    pub file: u32,
    pub file_offset: u32,
}

impl SourceMap {
    /// Resolve an item-relative byte offset in the current checked source.
    /// Lines and Unicode scalar columns start at one; a tab counts as one.
    pub fn position(&self, item: &DefPath, start: u32) -> Option<(&SourceFile, u32, u32)> {
        let binding = self.items.get(item)?;
        let file = &self.files[binding.file as usize];
        let prefix = &file.text[..(binding.file_offset + start) as usize];
        let line = prefix.bytes().filter(|b| *b == b'\n').count() as u32 + 1;
        let column = prefix.rsplit('\n').next().unwrap().chars().count() as u32 + 1;
        Some((file, line, column))
    }
}

/// A cached native unit loads this item's linked ordinal high word. The fresh
/// entry unit defines the symbol as `(ordinal + 1) << 32` beside its source table.
pub fn site_symbol(item: &DefPath) -> String {
    let hash = item.fingerprint();
    let mut symbol = String::from("hirsel_core_site_");
    for byte in hash.0 {
        write!(symbol, "{byte:02x}").unwrap();
    }
    symbol
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_bindings_move_lines_without_changing_item_symbols() {
        let item = DefPath::core("source_test");
        let symbol = site_symbol(&item);
        let mut map = SourceMap {
            files: vec![SourceFile {
                path: "test.hirsel".into(),
                text: "\n\tλ + 1".into(),
            }],
            items: BTreeMap::from([(
                item.clone(),
                ItemSource {
                    file: 0,
                    file_offset: 1,
                },
            )]),
        };
        let (_, line, column) = map.position(&item, 4).unwrap();
        assert_eq!((line, column), (2, 4));
        map.files[0].text.insert_str(0, "\n\n");
        map.items.get_mut(&item).unwrap().file_offset += 2;
        let (_, line, column) = map.position(&item, 4).unwrap();
        assert_eq!((line, column), (4, 4));
        assert_eq!(site_symbol(&item), symbol);
        assert_ne!(site_symbol(&DefPath::core("another_source_test")), symbol);
    }
}
