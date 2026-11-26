//! Current source bindings for native diagnostics, separate from semantic bodies.
use hirsel_core_ids::DefPath;
use std::collections::BTreeMap;
use std::sync::Arc;

mod native;
pub use native::*;

#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SourceMap {
    pub files: Vec<SourceFile>,
    pub items: BTreeMap<DefPath, ItemSource>,
    /// Exact coordinates of annex positions, by file and then dense ordinal.
    pub points: BTreeMap<String, Vec<(u32, u32)>>,
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
    /// The site word of each file's first byte, in file order: 1, then each
    /// file starts one past the previous file's end, so a site at a file's end
    /// stays in that file. Refuses sources whose last site would reach
    /// `SITE_END`; a site word is never truncated.
    pub fn site_starts(&self) -> Result<Vec<u64>, String> {
        let mut next = 1u64;
        let starts = self
            .files
            .iter()
            .map(|file| {
                let start = next;
                next += self
                    .points
                    .get(&file.path)
                    .map_or_else(|| file.text.len() as u64 + 1, |points| points.len() as u64);
                start
            })
            .collect();
        if next > hirsel_core_layout::source_positions::SITE_END {
            return Err(format!(
                "linked sources hold {} bytes; failure sites address fewer than {}",
                next - 1 - self.files.len() as u64,
                hirsel_core_layout::source_positions::SITE_END - 1,
            ));
        }
        Ok(starts)
    }
    /// Each item's first site word (`site_starts` plus its file offset), 0 for
    /// an item these sources do not bind. A cause anchored `start` bytes into
    /// item `i` has the site word `sites[i] + start`, as a linked native unit's.
    pub fn item_sites(&self, items: &[DefPath]) -> Result<Vec<u64>, String> {
        let starts = self.site_starts()?;
        Ok(items
            .iter()
            .map(|item| {
                self.items.get(item).map_or(0, |binding| {
                    starts[binding.file as usize] + u64::from(binding.file_offset)
                })
            })
            .collect())
    }
    /// Index this source snapshot once for repeated position lookups.
    pub fn positions(&self) -> SourcePositions<'_> {
        let files = self
            .files
            .iter()
            .map(|file| {
                let positions = match self.points.get(&file.path) {
                    Some(points) => FilePositions::Ordinals(points),
                    None => {
                        let mut starts = vec![0];
                        starts.extend(
                            file.text
                                .bytes()
                                .enumerate()
                                .filter_map(|(byte, b)| (b == b'\n').then_some(byte + 1)),
                        );
                        FilePositions::LineStarts(starts)
                    }
                };
                (Arc::from(file.path.as_str()), positions)
            })
            .collect();
        SourcePositions { map: self, files }
    }
}

/// Position lookup for a borrowed source snapshot.
pub struct SourcePositions<'a> {
    map: &'a SourceMap,
    files: Vec<(Arc<str>, FilePositions<'a>)>,
}

enum FilePositions<'a> {
    Ordinals(&'a [(u32, u32)]),
    LineStarts(Vec<usize>),
}

impl SourcePositions<'_> {
    /// Resolve an item-relative source offset. Lines and Unicode scalar
    /// columns start at one, and a tab counts as one.
    pub fn position(&self, item: &DefPath, start: u32) -> Option<(&SourceFile, u32, u32)> {
        let (index, line, column) = self.item_position(item, start)?;
        Some((&self.map.files[index], line, column))
    }

    pub(crate) fn annex_position(
        &self,
        item: &DefPath,
        start: u32,
    ) -> Option<(Arc<str>, u32, u32)> {
        let (index, line, column) = self.item_position(item, start)?;
        Some((Arc::clone(&self.files[index].0), line, column))
    }

    fn item_position(&self, item: &DefPath, start: u32) -> Option<(usize, u32, u32)> {
        let binding = self.map.items.get(item)?;
        let index = binding.file as usize;
        let offset = (binding.file_offset + start) as usize;
        let (line, column) = self.file_position(index, offset)?;
        Some((index, line, column))
    }

    /// Resolve a file-relative byte offset or annex ordinal.
    pub fn file_position(&self, index: usize, offset: usize) -> Option<(u32, u32)> {
        let (line, column) = match &self.files[index].1 {
            FilePositions::Ordinals(points) => *points.get(offset)?,
            FilePositions::LineStarts(starts) => {
                let file = &self.map.files[index];
                let line = starts.partition_point(|&start| start <= offset);
                let column = file.text.get(starts[line - 1]..offset)?.chars().count() as u32 + 1;
                (line as u32, column)
            }
        };
        Some((line, column))
    }
}

/// The items a failure anchor in `module` names, outside core@1 (whose anchors
/// a native unit never emits): the items whose first sites a run needs.
pub fn anchored_items(module: &crate::Module) -> Vec<DefPath> {
    let anchors = module
        .functions
        .iter()
        .filter_map(|f| f.body.as_ref())
        .flat_map(|body| {
            let blocks = body.blocks.iter().map(|b| &b.anchor);
            body.inst_anchors.iter().chain(blocks).flatten()
        });
    named_items(&module.items, anchors)
}

pub(crate) fn named_items<'a>(
    items: &[DefPath],
    anchors: impl Iterator<Item = &'a crate::Anchor>,
) -> Vec<DefPath> {
    let mut named = vec![false; items.len()];
    for anchor in anchors {
        named[anchor.item.index()] = true;
    }
    items
        .iter()
        .zip(named)
        .filter(|(item, named)| *named && !item.package_is("core", "1"))
        .map(|(item, _)| item.clone())
        .collect()
}

/// Sources for a module that has none: one blank file per item, each one byte
/// longer than the item's furthest anchor, so distinct anchors keep distinct
/// site words (a program can compare `Site`s) under the one dense encoding.
pub fn module_sources(module: &crate::Module) -> SourceMap {
    let mut ends = vec![0u32; module.items.len()];
    for body in module.functions.iter().filter_map(|f| f.body.as_ref()) {
        let blocks = body.blocks.iter().map(|b| &b.anchor);
        for anchor in body.inst_anchors.iter().chain(blocks).flatten() {
            let end = &mut ends[anchor.item.index()];
            *end = (*end).max(anchor.end.max(anchor.start) + 1);
        }
    }
    SourceMap {
        points: BTreeMap::new(),
        files: module
            .items
            .iter()
            .zip(&ends)
            .map(|(item, &end)| SourceFile {
                path: item.to_string(),
                text: " ".repeat(end as usize),
            })
            .collect(),
        items: module
            .items
            .iter()
            .enumerate()
            .map(|(file, item)| {
                let file = u32::try_from(file).expect("item count fits a file index");
                (
                    item.clone(),
                    ItemSource {
                        file,
                        file_offset: 0,
                    },
                )
            })
            .collect(),
    }
}

/// Each item's first site word under `module_sources`.
pub fn module_sites(module: &crate::Module) -> Vec<u64> {
    module_sources(module)
        .item_sites(&module.items)
        .expect("a module's anchors fit the site bound")
}

/// A cached native unit addresses this item's linked site, plus the anchor's
/// item-relative start. The fresh entry unit defines the symbol as the item's
/// first site word (`SourceMap::site_starts` plus its file offset).
pub fn site_symbol(item: &DefPath) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let hash = item.fingerprint();
    let mut symbol = Vec::with_capacity(SITE_SYMBOL_PREFIX.len() + 2 * hash.0.len());
    symbol.extend_from_slice(SITE_SYMBOL_PREFIX.as_bytes());
    for byte in hash.0 {
        symbol.extend([HEX[usize::from(byte >> 4)], HEX[usize::from(byte & 15)]]);
    }
    String::from_utf8(symbol).expect("hex digits are ASCII")
}

const SITE_SYMBOL_PREFIX: &str = "hirsel_core_site_";

/// The absolute symbol of value zero that a Mach-O site word is measured
/// from. A Mach-O word that names one symbol is an address to the linker: lld
/// records a rebase for it even when the symbol is absolute, and dyld then
/// adds the image's slide to the site. So a Mach-O unit writes each site word
/// as the difference `site symbol - origin + start`, which the linker resolves
/// and nothing slides. The fresh entry unit defines the origin. An ELF site
/// word names the site symbol alone.
pub const SITES_ORIGIN_SYMBOL: &str = "hirsel_core_sites_origin";

pub fn site_item_symbol(name: &[u8]) -> Option<&[u8]> {
    let suffix = name.strip_prefix(SITE_SYMBOL_PREFIX.as_bytes())?;
    Some(match suffix.iter().position(|&byte| byte == b'_') {
        Some(index) => &name[..SITE_SYMBOL_PREFIX.len() + index],
        None => name,
    })
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ReleaseSite {
    pub item: DefPath,
    pub start: u32,
    pub ordinal: u32,
    pub partition: Option<u32>,
}

/// The item whose release sites `item`'s are numbered among. A recipe unit
/// binds each of its distinct sites to its own item, `unit/<symbol>/<n>`, with
/// `n` counting every anchored site; its release sites are numbered across the
/// unit, so a site added or removed before a failing one renames none of the
/// unit's code. Any other item numbers its own.
pub fn release_site_owner(item: &DefPath) -> DefPath {
    let mut owner = item.clone();
    if numbers_unit_sites(item) {
        owner.0.name = owner.0.modules[1].clone();
        owner.0.modules.truncate(1);
    }
    owner
}

/// Whether `item` is a recipe unit's site item, whose owner
/// ([`release_site_owner`]) is another item.
pub fn numbers_unit_sites(item: &DefPath) -> bool {
    matches!(item.0.modules.as_slice(), [unit, _] if unit == "unit")
}

/// Synthetic DWARF file identity for release-code locations numbered by
/// the instructions that use them. The link binds those ordinals to sources.
pub fn release_debug_owner(item: &DefPath, partition: Option<u32>) -> DefPath {
    let mut owner = release_site_owner(item);
    owner.0.modules.insert(0, "$debug".into());
    if let Some(partition) = partition {
        owner.0.modules.insert(1, format!("p{partition}"));
    }
    owner
}

/// The symbol of the site numbered `ordinal` among those of `item`'s owner
/// ([`release_site_owner`]).
pub fn release_site_symbol(item: &DefPath, ordinal: u32, partition: Option<u32>) -> String {
    let owner = site_symbol(&release_site_owner(item));
    match partition {
        Some(partition) => format!("{owner}_release_p{partition}_{ordinal}"),
        None => format!("{owner}_release_{ordinal}"),
    }
}

/// SITES.md 6(b): whether a function reads its caller's site. Only a core@1
/// definition or a generated one (a `$` in its name, which no source identifier
/// has) does; every other body anchors each failure in its own item. Engines and the
/// evaluator read it from the callee's identity, never from its body, so a caller,
/// a library and an application agree without a stored fact.
pub fn inherits_site(instance: &crate::InstanceKey) -> bool {
    instance.definition.package_is("core", "1") || instance.definition.0.name.contains('$')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_bindings_move_lines_without_changing_item_symbols() {
        let item = DefPath::core("source_test");
        let symbol = site_symbol(&item);
        let mut map = SourceMap {
            points: Default::default(),
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
        let (_, line, column) = map.positions().position(&item, 4).unwrap();
        assert_eq!((line, column), (2, 4));
        map.files[0].text.insert_str(0, "\n\n");
        map.items.get_mut(&item).unwrap().file_offset += 2;
        let (_, line, column) = map.positions().position(&item, 4).unwrap();
        assert_eq!((line, column), (4, 4));
        assert_eq!(site_symbol(&item), symbol);
        assert_ne!(site_symbol(&DefPath::core("another_source_test")), symbol);
    }
}
