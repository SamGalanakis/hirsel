//! SITES revision2: the linker owns site ordinals, never their load address.
use super::*;
use object::{Object as _, ObjectSymbol as _};

use hirsel_core_ir::source::{site_item_symbol, site_symbol};
use object::ObjectSection as _;
use std::{collections::HashSet, fs::File, path::PathBuf};

/// Keep the runtime binders used by the actual link inputs. The separate
/// DWARF object is already bound against the complete source map.
pub(crate) fn retain_referenced(
    sources: &mut SourceMap,
    inputs: &[PathBuf],
) -> std::io::Result<()> {
    if sources.items.is_empty() {
        return Ok(());
    }
    let mut names = HashSet::new();
    for input in inputs {
        let cache = object::read::ReadCache::new(File::open(input)?);
        let object = object::File::parse(&cache).map_err(std::io::Error::other)?;
        for section in object.sections() {
            if !matches!(section.flags(), object::SectionFlags::Elf { sh_flags, .. }
                if sh_flags.0 & object::elf::SHF_ALLOC.0 != 0)
            {
                continue;
            }
            for (_, relocation) in section.relocations() {
                let object::RelocationTarget::Symbol(index) = relocation.target() else {
                    continue;
                };
                let symbol = object
                    .symbol_by_index(index)
                    .map_err(std::io::Error::other)?;
                let name = symbol.name_bytes().map_err(std::io::Error::other)?;
                if let Some(item) = site_item_symbol(name)
                    && !names.contains(item)
                {
                    names.insert(item.to_vec());
                }
            }
        }
    }
    // A release site's symbol is named by its item's owner.
    sources.items.retain(|item, _| {
        names.contains(site_symbol(item).as_bytes())
            || names
                .contains(site_symbol(&hirsel_core_ir::source::release_site_owner(item)).as_bytes())
    });
    let mut file_numbers = vec![None; sources.files.len()];
    for item in sources.items.values() {
        file_numbers[item.file as usize] = Some(0);
    }
    for (file, mapped) in std::mem::take(&mut sources.files)
        .into_iter()
        .zip(&mut file_numbers)
    {
        if mapped.is_some() {
            *mapped = Some(sources.files.len() as u32);
            sources.files.push(file);
        }
    }
    for item in sources.items.values_mut() {
        item.file = file_numbers[item.file as usize].expect("retained source file");
    }
    Ok(())
}

/// Each item's symbol is its first site word: its file's first site plus the
/// item's offset in that file.
pub(super) fn define(object: &mut object::write::Object<'_>, sources: &SourceMap) -> Result<()> {
    let items: Vec<_> = sources.items.keys().cloned().collect();
    for (path, site) in items.iter().zip(sources.item_sites(&items)?) {
        absolute(object, hirsel_core_ir::source::site_symbol(path), site);
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
        kind: object::SymbolKind::Unknown,
        scope: object::SymbolScope::Linkage,
        weak: false,
        section: object::write::SymbolSection::Absolute,
        flags: object::SymbolFlags::Elf {
            st_info: object::elf::SymbolInfo(object::elf::STB_GLOBAL.0 << 4),
            st_other: object::elf::SymbolOther(object::elf::STV_HIDDEN.0),
        },
    });
}

pub(crate) fn check_linked_sites(bytes: &[u8]) -> Result<()> {
    let file = object::File::parse(bytes).map_err(error)?;
    let sites: std::collections::HashMap<_, _> = file
        .dynamic_symbols()
        .filter_map(|s| {
            s.name()
                .ok()
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
        if let Ok(name) = symbol.name()
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
