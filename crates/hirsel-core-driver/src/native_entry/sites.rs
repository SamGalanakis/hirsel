//! SITES revision2: the linker owns site ordinals, never their load address.
use super::*;
use object::{Object as _, ObjectSymbol as _};

use hirsel_core_ir::source::{site_item_symbol, site_symbol};
use object::ObjectSection as _;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs::File,
    path::PathBuf,
};

fn allocatable<'data>(section: &impl object::ObjectSection<'data>) -> bool {
    match section.flags() {
        object::SectionFlags::Elf { sh_flags, .. } => sh_flags.0 & object::elf::SHF_ALLOC.0 != 0,
        // Mach-O loads every segment but the debug one.
        object::SectionFlags::MachO { .. } => {
            section.segment_name().ok().flatten() != Some("__DWARF")
        }
        _ => false,
    }
}

/// A symbol's name as the compiler wrote it: Mach-O prefixes C names with `_`.
fn unmangled(format: object::BinaryFormat, name: &[u8]) -> &[u8] {
    match format {
        object::BinaryFormat::MachO => name.strip_prefix(b"_").unwrap_or(name),
        _ => name,
    }
}

/// Keep the runtime binders used by the actual link inputs and report whether
/// those inputs call the unit spawn ABI. The separate DWARF object is already
/// bound against the complete source map.
/// With the key of `sources` (`recipe_build::bind_annexes`) a resident
/// compiler remembers the result by that key and what the inputs reference.
pub(crate) fn runtime_sources(
    sources: &SourceMap,
    map: Option<hirsel_core_ids::ContentHash>,
    inputs: &[PathBuf],
    keys: &[(hirsel_core_ids::ContentHash, u64)],
) -> std::io::Result<(std::sync::Arc<SourceMap>, bool)> {
    let scans = inputs
        .iter()
        .zip(keys)
        .map(|(input, key)| crate::resident::REFERENCED.get_or(Some(*key), || referenced(input)))
        .collect::<std::io::Result<Vec<_>>>()?;
    let digests: Vec<_> = scans.iter().map(|scan| scan.digest).collect();
    let key = map.map(|map| (map, digests.clone()));
    crate::resident::RUNTIME_SOURCES.get_or(key, || {
        // A resident compiler that rebased its map carries over what it kept
        // of the previous map for the same inputs.
        let rebased = map
            .and_then(crate::source_delta::transition)
            .and_then(|transition| {
                let (prior, spawns_units) =
                    crate::resident::RUNTIME_SOURCES.get(&(transition.from, digests))?;
                Some((rebase(sources, &scans, &prior, &transition)?, spawns_units))
            });
        Ok(rebased.unwrap_or_else(|| filter(sources, &scans)))
    })
}

/// [`filter`]'s result for `sources` from its result `prior` for the map
/// `transition` was rebased from: none when the files it keeps would change.
fn rebase(
    sources: &SourceMap,
    scans: &[Referenced],
    prior: &SourceMap,
    transition: &crate::source_delta::Transition,
) -> Option<std::sync::Arc<SourceMap>> {
    if sources.items.is_empty() {
        return None;
    }
    let names = names(scans);
    // The kept files, in map order, are a subsequence of the map's.
    let mut kept = prior.files.iter().enumerate().peekable();
    let numbers: Vec<Option<u32>> = sources
        .files
        .iter()
        .map(|file| {
            let (index, _) = kept.next_if(|(_, kept)| kept.path == file.path)?;
            Some(index as u32)
        })
        .collect();
    let mut items = prior.items.clone();
    for item in &transition.removed {
        items.remove(item);
    }
    let mut owners = HashMap::new();
    for item in &transition.bound {
        let named = names.contains(site_symbol(item).as_bytes())
            || hirsel_core_ir::source::numbers_unit_sites(item)
                && *owners
                    .entry(hirsel_core_ir::source::release_site_owner(item))
                    .or_insert_with_key(|owner| names.contains(site_symbol(owner).as_bytes()));
        if named {
            let mut at = sources.items[item];
            at.file = numbers[at.file as usize]?;
            items.insert(item.clone(), at);
        }
    }
    let mut used = vec![false; prior.files.len()];
    for item in items.values() {
        used[item.file as usize] = true;
    }
    used.iter().all(|&used| used).then(|| {
        std::sync::Arc::new(SourceMap {
            files: prior.files.clone(),
            items,
            points: sources.points.clone(),
        })
    })
}

fn names(scans: &[Referenced]) -> HashSet<&[u8]> {
    scans
        .iter()
        .flat_map(|scan| scan.items.iter().map(Vec::as_slice))
        .collect()
}

/// The part of `sources` the inputs whose `scans` are given reference, and
/// whether they spawn units.
fn filter(sources: &SourceMap, scans: &[Referenced]) -> (std::sync::Arc<SourceMap>, bool) {
    let spawns_units = scans.iter().any(|scan| scan.spawns_units);
    if sources.items.is_empty() {
        return (std::sync::Arc::new(sources.clone()), spawns_units);
    }
    let names = names(scans);
    let named = named_items(sources, &names);
    let mut last_owner = None;
    let retained = sources
        .items
        .iter()
        .zip(named)
        .filter(|((item, _), named)| {
            let item = *item;
            if *named {
                return true;
            }
            if !hirsel_core_ir::source::numbers_unit_sites(item) {
                return false;
            }
            // Numbered items of the same unit share one owner symbol and are
            // adjacent in the source map's item order.
            let owner = (&item.0.package, item.0.modules.as_slice());
            if let Some((prior, used)) = last_owner
                && owner == prior
            {
                return used;
            }
            let used = names.contains(
                site_symbol(&hirsel_core_ir::source::release_site_owner(item)).as_bytes(),
            );
            last_owner = Some((owner, used));
            used
        });
    let retained = retained.map(|(item, _)| item);
    let mut items: BTreeMap<_, _> = retained.map(|(item, at)| (item.clone(), *at)).collect();
    let mut file_numbers = vec![None; sources.files.len()];
    for item in items.values() {
        file_numbers[item.file as usize] = Some(0);
    }
    let mut files = Vec::new();
    for (file, mapped) in sources.files.iter().zip(&mut file_numbers) {
        if mapped.is_some() {
            *mapped = Some(files.len() as u32);
            files.push(file.clone());
        }
    }
    for item in items.values_mut() {
        item.file = file_numbers[item.file as usize].expect("retained source file");
    }
    (
        std::sync::Arc::new(SourceMap {
            files,
            items,
            points: sources.points.clone(),
        }),
        spawns_units,
    )
}

/// Whether each of `sources`' items, in map order, is one of `names` by its
/// own site symbol. A symbol is a hash of its item, so the items are split
/// across this process's processors.
fn named_items(sources: &SourceMap, names: &HashSet<&[u8]>) -> Vec<bool> {
    let items: Vec<&hirsel_core_ids::DefPath> = sources.items.keys().collect();
    let threads = std::thread::available_parallelism().map_or(1, usize::from);
    let chunk = items.len().div_ceil(threads).max(1024);
    let named = |items: &[&hirsel_core_ids::DefPath]| -> Vec<bool> {
        items
            .iter()
            .map(|item| names.contains(site_symbol(item).as_bytes()))
            .collect()
    };
    if items.len() <= chunk {
        return named(&items);
    }
    std::thread::scope(|scope| {
        let parts: Vec<_> = items
            .chunks(chunk)
            .map(|part| scope.spawn(move || named(part)))
            .collect();
        parts
            .into_iter()
            .flat_map(|part| part.join().expect("a site symbol scan panicked"))
            .collect()
    })
}

/// The site items an input object's allocated sections reference, and
/// whether they call the unit spawn ABI: a function of the object's bytes;
/// and the digest of both.
#[derive(Clone)]
pub(crate) struct Referenced {
    items: std::sync::Arc<[Vec<u8>]>,
    spawns_units: bool,
    digest: hirsel_core_ids::ContentHash,
}

fn referenced(input: &PathBuf) -> std::io::Result<Referenced> {
    let mut items = HashSet::new();
    let mut spawns_units = false;
    let cache = object::read::ReadCache::new(File::open(input)?);
    let object = object::File::parse(&cache).map_err(std::io::Error::other)?;
    for section in object.sections() {
        if !allocatable(&section) {
            continue;
        }
        for (_, relocation) in section.relocations() {
            let object::RelocationTarget::Symbol(index) = relocation.target() else {
                continue;
            };
            let symbol = object
                .symbol_by_index(index)
                .map_err(std::io::Error::other)?;
            let name = unmangled(
                object.format(),
                symbol.name_bytes().map_err(std::io::Error::other)?,
            );
            spawns_units |= name == b"hirsel_core_spawn_unit";
            if let Some(item) = site_item_symbol(name)
                && !items.contains(item)
            {
                items.insert(item.to_vec());
            }
        }
    }
    let mut items: Vec<_> = items.into_iter().collect();
    items.sort_unstable();
    let digest = hirsel_core_ids::ContentHash::of_parts(
        "hirsel.site-references.v1",
        std::iter::once(&[u8::from(spawns_units)][..]).chain(items.iter().map(Vec::as_slice)),
    );
    Ok(Referenced {
        items: items.into(),
        spawns_units,
        digest,
    })
}

/// Site relocations in a GC link with `--emit-relocs`. Absolute symbols
/// themselves survive GC even when the last reference to one was discarded.
pub(crate) fn live_site_words(bytes: &[u8]) -> Result<BTreeSet<u64>> {
    let file = object::File::parse(bytes).map_err(error)?;
    let mut live = BTreeSet::new();
    for section in file.sections().filter(allocatable) {
        for (_, relocation) in section.relocations() {
            let object::RelocationTarget::Symbol(index) = relocation.target() else {
                continue;
            };
            let symbol = file.symbol_by_index(index).map_err(error)?;
            let name = unmangled(file.format(), symbol.name_bytes().map_err(error)?);
            if site_item_symbol(name).is_none() {
                continue;
            }
            let invalid = || {
                format!(
                    "linked site relocation against {} is not an absolute site word",
                    String::from_utf8_lossy(name)
                )
            };
            if symbol.section() != object::SymbolSection::Absolute
                || relocation.kind() != object::RelocationKind::Absolute
                || relocation.has_implicit_addend()
            {
                return Err(invalid());
            }
            let site = symbol
                .address()
                .checked_add_signed(relocation.addend())
                .filter(|site| (1..hirsel_core_layout::source_positions::SITE_END).contains(site))
                .ok_or_else(invalid)?;
            live.insert(site);
        }
    }
    Ok(live)
}

/// Each item's symbol is its first site word: its file's first site plus the
/// item's offset in that file.
pub(super) fn define(object: &mut object::write::Object<'_>, sources: &SourceMap) -> Result<()> {
    let starts = sources.site_starts()?;
    for (path, binding) in &sources.items {
        let site = starts[binding.file as usize] + u64::from(binding.file_offset);
        absolute(object, site_symbol(path), site);
    }
    Ok(())
}

pub(super) fn define_release(
    object: &mut object::write::Object<'_>,
    sources: &SourceMap,
    bindings: &[hirsel_core_ir::source::ReleaseSite],
) -> Result<()> {
    if bindings.is_empty() {
        return Ok(());
    }
    let starts = sources.site_starts()?;
    for binding in bindings {
        if let Some(item) = sources.items.get(&binding.item) {
            absolute(
                object,
                hirsel_core_ir::source::release_site_symbol(&binding.item, binding.ordinal),
                starts[item.file as usize] + u64::from(item.file_offset) + u64::from(binding.start),
            );
        }
    }
    Ok(())
}

fn absolute(object: &mut object::write::Object<'_>, name: String, value: u64) {
    object.add_symbol(object::write::Symbol {
        name: name.into_bytes(),
        value,
        size: 0,
        // Mach-O mangles (prefixes `_`) only text and data symbols, as the
        // units' imports of these names are.
        kind: if object.format() == object::BinaryFormat::MachO {
            object::SymbolKind::Data
        } else {
            object::SymbolKind::Unknown
        },
        scope: object::SymbolScope::Linkage,
        weak: false,
        section: object::write::SymbolSection::Absolute,
        // Linkage scope is already hidden on Mach-O (private extern).
        flags: if object.format() == object::BinaryFormat::Elf {
            object::SymbolFlags::Elf {
                st_info: object::elf::SymbolInfo(object::elf::STB_GLOBAL.0 << 4),
                st_other: object::elf::SymbolOther(object::elf::STV_HIDDEN.0),
            }
        } else {
            object::SymbolFlags::None
        },
    });
}

pub(crate) fn check_linked_sites(bytes: &[u8]) -> Result<()> {
    let file = object::File::parse(bytes).map_err(error)?;
    let sites: std::collections::HashMap<_, _> = file
        .dynamic_symbols()
        .filter_map(|s| {
            s.name_bytes()
                .ok()
                .map(|n| unmangled(file.format(), n))
                .and_then(|n| std::str::from_utf8(n).ok())
                .filter(|n| n.starts_with("hirsel_core_site_"))
                .map(|name| (s.index(), name))
        })
        .collect();
    if let Some(relocations) = file.dynamic_relocations() {
        for (_, relocation) in relocations {
            if let object::RelocationTarget::Symbol(index) = relocation.target()
                && let Some(name) = sites.get(&index)
            {
                return Err(format!("dynamic relocation against site symbol {name}"));
            }
        }
    }
    // The external linker must preserve the absolute definitions. This also
    // catches a section-relative definition reduced to a nameless RELATIVE.
    for symbol in file.symbols() {
        if let Ok(name) = symbol.name_bytes()
            && let Ok(name) = std::str::from_utf8(unmangled(file.format(), name))
            && name.starts_with("hirsel_core_site_")
            && symbol.section() != object::SymbolSection::Absolute
        {
            return Err(format!("linked site symbol {name} is not absolute"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn absolute_site_symbols_and_section_relative_refusal() {
        for architecture in [object::Architecture::X86_64, object::Architecture::Aarch64] {
            let mut object = object::write::Object::new(
                object::BinaryFormat::Elf,
                architecture,
                object::Endianness::Little,
            );
            let path = hirsel_core_ids::DefPath::core("site_binding");
            let sources = SourceMap {
                points: Default::default(),
                files: vec![hirsel_core_ir::source::SourceFile {
                    path: "site.hirsel".into(),
                    text: String::new(),
                }],
                items: std::collections::BTreeMap::from([(
                    path.clone(),
                    hirsel_core_ir::source::ItemSource {
                        file: 0,
                        file_offset: 0,
                    },
                )]),
            };
            define(&mut object, &sources).unwrap();
            let bytes = object.write().unwrap();
            let file = object::File::parse(bytes.as_slice()).unwrap();
            let symbol = file
                .symbols()
                .find(|s| s.name().unwrap() == hirsel_core_ir::source::site_symbol(&path))
                .unwrap();
            assert_eq!(symbol.section(), object::SymbolSection::Absolute);
            assert_eq!(symbol.address(), 1);
            assert_eq!(symbol.size(), 0);
            assert_eq!(symbol.scope(), object::SymbolScope::Linkage);
            assert!(
                matches!(symbol.flags(), object::SymbolFlags::Elf { st_info, st_other } if st_info.0 == object::elf::STB_GLOBAL.0 << 4 && st_other.0 == object::elf::STV_HIDDEN.0)
            );
            check_linked_sites(&bytes).unwrap();
            let mut bad = object::write::Object::new(
                object::BinaryFormat::Elf,
                architecture,
                object::Endianness::Little,
            );
            let section = bad.add_section(vec![], b".data".to_vec(), object::SectionKind::Data);
            bad.section_mut(section).set_data(vec![0; 8], 8);
            bad.add_symbol(object::write::Symbol {
                name: hirsel_core_ir::source::site_symbol(&path).into_bytes(),
                value: 0,
                size: 8,
                kind: object::SymbolKind::Data,
                scope: object::SymbolScope::Linkage,
                weak: false,
                section: object::write::SymbolSection::Section(section),
                flags: object::SymbolFlags::None,
            });
            let bytes = bad.write().unwrap();
            assert!(
                check_linked_sites(&bytes)
                    .unwrap_err()
                    .contains("is not absolute")
            );
        }
    }
}
