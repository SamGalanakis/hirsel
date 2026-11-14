//! SITES revision2: the linker owns site ordinals, never their load address.
use super::*;
use object::{Object as _, ObjectSymbol as _};

pub(super) fn define(object: &mut object::write::Object<'_>, sources: &SourceMap) {
    for (i, path) in sources.items.keys().enumerate() {
        object.add_symbol(object::write::Symbol {
            name: hirsel_core_ir::source::site_symbol(path).into_bytes(),
            value: (i as u64 + 1) << 32,
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
                files: vec![],
                items: std::collections::BTreeMap::from([(
                    path.clone(),
                    hirsel_core_ir::source::ItemSource {
                        file: 0,
                        file_offset: 0,
                    },
                )]),
            };
            define(&mut object, &sources);
            let bytes = object.write().unwrap();
            let file = object::File::parse(bytes.as_slice()).unwrap();
            let symbol = file
                .symbols()
                .find(|s| s.name().unwrap() == hirsel_core_ir::source::site_symbol(&path))
                .unwrap();
            assert_eq!(symbol.section(), object::SymbolSection::Absolute);
            assert_eq!(symbol.address(), 1u64 << 32);
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
