//! SITES revision2: the linker owns site ordinals, never their load address.
use super::*;
use object::{Object as _, ObjectSymbol as _};

use hirsel_core_ir::source::{site_item_symbol, site_symbol};
use object::ObjectSection as _;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

/// Whether `section` is loaded with the image: every Mach-O section but the
/// debug ones is.
fn allocatable<'data>(section: &impl object::ObjectSection<'data>) -> bool {
    match section.flags() {
        object::SectionFlags::Elf { sh_flags, .. } => sh_flags.0 & object::elf::SHF_ALLOC.0 != 0,
        object::SectionFlags::MachO { flags, .. } => flags.0 & object::macho::S_ATTR_DEBUG.0 == 0,
        _ => false,
    }
}

/// `name` as Hirsel defines it: a Mach-O object's names carry a leading `_`.
fn link_name(format: hirsel_core_layout::ObjectFormat, name: &[u8]) -> &[u8] {
    match format {
        hirsel_core_layout::ObjectFormat::Elf => name,
        hirsel_core_layout::ObjectFormat::MachO => name.strip_prefix(b"_").unwrap_or(name),
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
    members: &[std::sync::Arc<[u8]>],
    keys: &[(hirsel_core_ids::ContentHash, u64)],
) -> std::io::Result<(std::sync::Arc<SourceMap>, bool)> {
    let scans = keys
        .iter()
        .enumerate()
        .map(|(n, key)| crate::resident::REFERENCED.get_or(Some(*key), || referenced(&members[n])))
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

/// The link names of every code descriptor the inputs define, sorted: the
/// index a recording entry gives the runtime, which finds a spawned
/// helper's entry identity by its code word (docs/recording.md).
/// With them the link's spawn table: each spawnable function's descriptor
/// name with the name of its `(X, R)`'s spawn entry, joined from the
/// markers the function's object defines to the entries the spawn sites'
/// objects define. None unless `indexed`.
pub(super) fn descriptors(
    indexed: bool,
    members: &[std::sync::Arc<[u8]>],
    keys: &[(hirsel_core_ids::ContentHash, u64)],
) -> std::io::Result<Descriptors> {
    let mut names = BTreeSet::new();
    let mut spawnables = BTreeSet::new();
    let mut entries = BTreeSet::new();
    for (n, key) in keys.iter().enumerate().filter(|_| indexed) {
        let scan = crate::resident::REFERENCED.get_or(Some(*key), || referenced(&members[n]))?;
        names.extend(scan.descriptors.iter().cloned());
        spawnables.extend(scan.spawnables.iter().cloned());
        entries.extend(scan.entries.iter().cloned());
    }
    let spawns = spawnables
        .into_iter()
        .filter(|(descriptor, entry)| names.contains(descriptor) && entries.contains(entry))
        .collect();
    Ok(Descriptors {
        names: names.into_iter().collect(),
        spawns,
    })
}

/// The link's descriptor index and spawn table, by link names.
#[derive(Default)]
pub(crate) struct Descriptors {
    pub(crate) names: Vec<Vec<u8>>,
    /// `(descriptor, spawn entry)`, one row per spawnable function.
    pub(crate) spawns: Vec<(Vec<u8>, Vec<u8>)>,
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
/// own site symbol. The site scan shares the driver's worker budget.
fn named_items(sources: &SourceMap, names: &HashSet<&[u8]>) -> Vec<bool> {
    let items: Vec<&hirsel_core_ids::DefPath> = sources.items.keys().collect();
    let threads = hirsel_guard::jobs();
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
    /// The code descriptors (PROG-9) the object defines, by link name.
    descriptors: std::sync::Arc<[Vec<u8>]>,
    /// The spawnable markers it defines, as `(descriptor, spawn entry)`.
    spawnables: std::sync::Arc<[(Vec<u8>, Vec<u8>)]>,
    /// The spawn entries it defines.
    entries: std::sync::Arc<[Vec<u8>]>,
    digest: hirsel_core_ids::ContentHash,
}

fn referenced(input: &[u8]) -> std::io::Result<Referenced> {
    let mut items = HashSet::new();
    let mut spawns_units = false;
    let object = object::File::parse(input).map_err(std::io::Error::other)?;
    let format = crate::native_debug::object_format(object.format())?;
    let mut descriptors = BTreeSet::new();
    let mut spawnables = BTreeSet::new();
    let mut entries = BTreeSet::new();
    for symbol in object.symbols() {
        if !symbol.is_definition() || !symbol.is_global() {
            continue;
        }
        let Ok(name) = symbol.name_bytes() else {
            continue;
        };
        let name = link_name(format, name);
        if name.ends_with(b"$static") {
            descriptors.insert(name.to_vec());
        } else if name.ends_with(b"$spawn_entry") {
            entries.insert(name.to_vec());
        } else if let Ok(name) = std::str::from_utf8(name)
            && let Some((descriptor, entry)) = hirsel_core_ir::spawnable_parts(name)
        {
            spawnables.insert((descriptor.as_bytes().to_vec(), entry.into_bytes()));
        }
    }
    let descriptors: Vec<_> = descriptors.into_iter().collect();
    let spawnables: Vec<_> = spawnables.into_iter().collect();
    let entries: Vec<_> = entries.into_iter().collect();
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
            let name = link_name(format, symbol.name_bytes().map_err(std::io::Error::other)?);
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
        "hirsel.site-references.v3",
        std::iter::once(&[u8::from(spawns_units)][..])
            .chain(items.iter().map(Vec::as_slice))
            .chain(std::iter::once(&b"descriptors"[..]))
            .chain(descriptors.iter().map(Vec::as_slice))
            .chain(std::iter::once(&b"spawnables"[..]))
            .chain(
                spawnables
                    .iter()
                    .flat_map(|(d, e)| [d.as_slice(), e.as_slice()]),
            )
            .chain(std::iter::once(&b"entries"[..]))
            .chain(entries.iter().map(Vec::as_slice)),
    );
    Ok(Referenced {
        items: items.into(),
        spawns_units,
        descriptors: descriptors.into(),
        spawnables: spawnables.into(),
        entries: entries.into(),
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
            let name = symbol.name_bytes().map_err(error)?;
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
/// item's offset in that file. A Mach-O entry also defines the origin its
/// units measure their site words from.
pub(super) fn define(object: &mut object::write::Object<'_>, sources: &SourceMap) -> Result<()> {
    if object.format() == object::BinaryFormat::MachO {
        absolute(
            object,
            hirsel_core_ir::source::SITES_ORIGIN_SYMBOL.into(),
            0,
        );
    }
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
    // `object` prefixes only text, data and TLS names, so a Mach-O absolute
    // symbol takes its leading `_` here, as the code referring to it does.
    let mut name = name.into_bytes();
    if let Some(prefix) = object.mangling().global_prefix() {
        name.insert(0, prefix);
    }
    object.add_symbol(object::write::Symbol {
        name,
        value,
        size: 0,
        kind: object::SymbolKind::Unknown,
        scope: object::SymbolScope::Linkage,
        weak: false,
        section: object::write::SymbolSection::Absolute,
        // A Mach-O linkage-scope symbol is private extern already.
        flags: match object.format() {
            object::BinaryFormat::MachO => object::SymbolFlags::None,
            _ => object::SymbolFlags::Elf {
                st_info: object::elf::SymbolInfo(object::elf::STB_GLOBAL.0 << 4),
                st_other: object::elf::SymbolOther(object::elf::STV_HIDDEN.0),
            },
        },
    });
}

pub(crate) fn check_linked_sites(bytes: &[u8]) -> Result<()> {
    let file = object::File::parse(bytes).map_err(error)?;
    let format = crate::native_debug::object_format(file.format()).map_err(error)?;
    let is_site = |name: &str| link_name(format, name.as_bytes()).starts_with(b"hirsel_core_site_");
    let sites: std::collections::HashMap<_, _> = file
        .dynamic_symbols()
        .filter_map(|s| {
            s.name()
                .ok()
                .filter(|n| is_site(n))
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
            && is_site(name)
            && symbol.section() != object::SymbolSection::Absolute
        {
            return Err(format!("linked site symbol {name} is not absolute"));
        }
    }
    if let object::File::MachO64(file) = file {
        check_chained_rebases(&file, bytes)?;
    }
    Ok(())
}

fn check_chained_rebases(file: &object::read::macho::MachOFile64<'_>, bytes: &[u8]) -> Result<()> {
    use object::ObjectSegment as _;
    use object::read::macho::Fixup;

    let endian = file.endian();
    let mut commands = file.macho_load_commands().map_err(error)?;
    while let Some(command) = commands.next().map_err(error)? {
        let Some(command) = command.dyld_chained_fixups().map_err(error)? else {
            continue;
        };
        let segments: Vec<_> = file.segments().collect();
        let base = segments
            .iter()
            .find(|segment| segment.name_bytes() == Ok(Some(b"__TEXT")))
            .ok_or("Mach-O chained fixups have no __TEXT image base")?
            .address();
        let fixups = command.chained_fixups(endian, bytes).map_err(error)?;
        for chain in fixups.segments(endian).map_err(error)? {
            let chain = chain.map_err(error)?;
            let segment = segments
                .get(chain.index() as usize)
                .ok_or("Mach-O chained fixups name a missing segment")?;
            for entry in chain.fixups(endian, base, segment.data().map_err(error)?) {
                let (offset, fixup) = entry.map_err(error)?;
                // Absolute targets below the base wrap when object converts
                // them to image offsets. Dyld would slide the site's value.
                if let Fixup::Rebase(rebase) = fixup
                    && base.checked_add(rebase.target_offset).is_none()
                {
                    let address = segment.address().saturating_add(offset);
                    let target = base.wrapping_add(rebase.target_offset);
                    return Err(format!(
                        "Mach-O chained rebase at {address:#x} targets {target:#x} below image base {base:#x}"
                    ));
                }
            }
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
            let name = site_symbol(&path);
            assert_eq!(
                named_items(&sources, &HashSet::from([name.as_bytes()])),
                [true]
            );
            assert_eq!(named_items(&sources, &HashSet::new()), [false]);
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

    /// A Mach-O C name `x` is the symbol `_x`, which is what the code naming a
    /// site refers to; an unprefixed definition leaves every site undefined at
    /// the link.
    #[test]
    fn macho_site_symbols_take_the_global_prefix() {
        let mut object = object::write::Object::new(
            object::BinaryFormat::MachO,
            object::Architecture::Aarch64,
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
        let name = format!("_{}", hirsel_core_ir::source::site_symbol(&path));
        let symbol = file.symbols().find(|s| s.name() == Ok(name.as_str()));
        assert_eq!(
            symbol.map(|s| s.section()),
            Some(object::SymbolSection::Absolute),
            "{name}"
        );
        check_linked_sites(&bytes).unwrap();

        let section = object.add_section(
            b"__DATA".to_vec(),
            b"__data".to_vec(),
            object::SectionKind::Data,
        );
        object.section_mut(section).set_data(vec![0; 8], 8);
        let relative = "_hirsel_core_site_relative";
        object.add_symbol(object::write::Symbol {
            name: relative.as_bytes().to_vec(),
            value: 0,
            size: 8,
            kind: object::SymbolKind::Unknown,
            scope: object::SymbolScope::Linkage,
            weak: false,
            section: object::write::SymbolSection::Section(section),
            flags: object::SymbolFlags::None,
        });
        assert_eq!(
            check_linked_sites(&object.write().unwrap()).unwrap_err(),
            format!("linked site symbol {relative} is not absolute")
        );
    }
}
