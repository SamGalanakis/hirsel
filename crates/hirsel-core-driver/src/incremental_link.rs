//! The resident development link: a resident compiler remembers its last
//! link's objects and the executable lld made of them, and links an edit
//! whose objects keep every section's size, every relocation and every
//! symbol (a symbol's name may change at the same length) by writing the
//! changed bytes into that executable in place, as Zig's incremental ELF
//! linker patches a changed atom where it already lies. The executable is
//! lld's for the new objects byte for byte: the layout, the symbol values
//! and every relocation's result are unchanged, and the bytes lld copies from
//! a section outside its relocations' fields are the section's own. Any other
//! edit, a link outside a resident compiler, and a release link (which
//! collects sections) are linked by lld.
//!
//! The patch never pads or moves a section: the executable stays the one a
//! full link of the same objects makes, so a build in an empty store and an
//! incremental build deliver the same bytes.
use hirsel_core_ids::ContentHash;
use std::io;
use std::sync::{Arc, Mutex};

/// One object a link reads, in link order: bytes made for this link, or
/// checked bytes with the key they are a function of.
pub(crate) enum Slot<'a> {
    Bytes(&'a [u8]),
    Keyed((ContentHash, u64), &'a Arc<[u8]>),
}

struct Remembered {
    key: Option<(ContentHash, u64)>,
    bytes: Arc<[u8]>,
}

struct Previous {
    /// Every argument of the link that is not one of its objects.
    frame: ContentHash,
    objects: Vec<Remembered>,
    /// Publication may still hold the previous bytes when a new edit arrives.
    image: Arc<Vec<u8>>,
    /// The image's symbols, made by the first patch over it.
    symbols: Option<Symbols>,
}

/// A resident compiler's last development link: its objects and the
/// executable lld made of them, patched since by each edit it linked.
#[derive(Default)]
pub(crate) struct Incremental(Option<Previous>);

/// The resident compiler's incremental link.
pub(crate) static RESIDENT: crate::resident::WorkingMemo<(), Arc<Mutex<Incremental>>> =
    crate::resident::WorkingMemo::new();

pub(crate) fn patch(frame: ContentHash, objects: &[Slot<'_>]) -> io::Result<Option<Arc<Vec<u8>>>> {
    RESIDENT
        .previous_or_default()
        .lock()
        .unwrap()
        .patch(frame, objects)
}

pub(crate) fn remember(
    frame: ContentHash,
    objects: &[Slot<'_>],
    image: &Arc<Vec<u8>>,
) -> io::Result<()> {
    RESIDENT
        .previous_or_default()
        .lock()
        .unwrap()
        .remember(frame, objects, image)
}

impl Incremental {
    /// The executable lld would make of `objects` under `frame`, written
    /// over the remembered one, when only patchable edits separate them.
    pub(crate) fn patch(
        &mut self,
        frame: ContentHash,
        objects: &[Slot<'_>],
    ) -> io::Result<Option<Arc<Vec<u8>>>> {
        let Some(previous) = self.0.as_mut() else {
            return Ok(None);
        };
        if previous.frame != frame || previous.objects.len() != objects.len() {
            return Ok(None);
        }
        let mut changed = Vec::new();
        for (n, (slot, old)) in objects.iter().zip(&previous.objects).enumerate() {
            let new = match slot {
                Slot::Bytes(bytes) if *bytes == &*old.bytes => continue,
                Slot::Bytes(bytes) => Arc::from(*bytes),
                Slot::Keyed(key, _) if old.key == Some(*key) => continue,
                Slot::Keyed(_, bytes) => Arc::clone(bytes),
            };
            changed.push((n, new));
        }
        let symbols = previous
            .symbols
            .get_or_insert_with(|| Symbols::of(&previous.image).unwrap_or_default());
        let mut edits = Vec::new();
        for (n, new) in &changed {
            match edit(&previous.image, symbols, &previous.objects[*n].bytes, new) {
                Some(more) => edits.extend(more),
                None => return Ok(None),
            }
        }
        // Two renames to one name would make one symbol of two.
        let mut renamed = rustc_hash::FxHashSet::default();
        for edit in &edits {
            if let Edit::Rename(old, new) = edit
                && !(renamed.insert(old.clone()) && renamed.insert(new.clone()))
            {
                return Ok(None);
            }
        }
        if !edits.is_empty() {
            let image = Arc::make_mut(&mut previous.image);
            for edit in edits {
                match edit {
                    Edit::Bytes(at, bytes) => image[at..at + bytes.len()].copy_from_slice(&bytes),
                    Edit::Rename(old, new) => symbols.rename(image, &old, new),
                }
            }
        }
        for (n, new) in changed {
            let key = match objects[n] {
                Slot::Bytes(_) => None,
                Slot::Keyed(key, _) => Some(key),
            };
            previous.objects[n] = Remembered { key, bytes: new };
        }
        Ok(Some(Arc::clone(&previous.image)))
    }

    /// Remember the executable lld made of `objects` under `frame`.
    pub(crate) fn remember(
        &mut self,
        frame: ContentHash,
        objects: &[Slot<'_>],
        image: &Arc<Vec<u8>>,
    ) -> io::Result<()> {
        self.0 = None;
        let Some(elf) = Elf::parse(image) else {
            return Ok(());
        };
        // A build ID is a digest of the whole image.
        if elf
            .sections()
            .any(|s| elf.name(&s) == b".note.gnu.build-id")
        {
            return Ok(());
        }
        let objects = objects
            .iter()
            .map(|slot| {
                Ok(match slot {
                    Slot::Bytes(bytes) => Remembered {
                        key: None,
                        bytes: Arc::from(*bytes),
                    },
                    Slot::Keyed(key, bytes) => Remembered {
                        key: Some(*key),
                        bytes: Arc::clone(bytes),
                    },
                })
            })
            .collect::<io::Result<_>>()?;
        self.0 = Some(Previous {
            frame,
            objects,
            image: Arc::clone(image),
            symbols: None,
        });
        Ok(())
    }
}

enum Edit {
    Bytes(usize, Vec<u8>),
    Rename(Vec<u8>, Vec<u8>),
}

const SHT_PROGBITS: u32 = 1;
const SHT_SYMTAB: u32 = 2;
const SHT_RELA: u32 = 4;
const SHT_NOTE: u32 = 7;
const SHT_NOBITS: u32 = 8;
const SHF_ALLOC: u64 = 0x2;
/// Flags under which lld reads a section's bytes rather than copying them,
/// or may drop or reorder it: MERGE, STRINGS, LINK_ORDER, GROUP, TLS,
/// COMPRESSED, EXCLUDE.
const SHF_READ: u64 = 0x10 | 0x20 | 0x80 | 0x200 | 0x400 | 0x800 | 0x8000_0000;
const STT_SECTION: u8 = 3;
const STT_FILE: u8 = 4;
const STT_TLS: u8 = 6;
const STB_LOCAL: u8 = 0;
const STB_GLOBAL: u8 = 1;
const STV_DEFAULT: u8 = 0;

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}
fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}
fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Section {
    name: u32,
    kind: u32,
    flags: u64,
    addr: u64,
    offset: u64,
    size: u64,
    link: u32,
    info: u32,
    align: u64,
    entsize: u64,
}
impl Section {
    /// Every field but where the section's bytes lie in its file.
    fn layout(&self) -> Section {
        Section { offset: 0, ..*self }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Sym {
    name: u32,
    info: u8,
    other: u8,
    shndx: u16,
    value: u64,
    size: u64,
}

/// A little-endian x86-64 ELF64 file.
struct Elf<'a> {
    bytes: &'a [u8],
    sections: Vec<Section>,
    names: Section,
}
impl<'a> Elf<'a> {
    fn parse(bytes: &'a [u8]) -> Option<Self> {
        // ELFCLASS64, ELFDATA2LSB, EM_X86_64.
        if bytes.get(..6)? != b"\x7fELF\x02\x01" || u16_at(bytes, 0x12)? != 62 {
            return None;
        }
        let shoff = u64_at(bytes, 0x28)? as usize;
        let size = u16_at(bytes, 0x3a)? as usize;
        let count = u16_at(bytes, 0x3c)? as usize;
        let names = u16_at(bytes, 0x3e)? as usize;
        if size != 64 {
            return None;
        }
        let sections = (0..count)
            .map(|n| {
                let at = shoff + n * size;
                Some(Section {
                    name: u32_at(bytes, at)?,
                    kind: u32_at(bytes, at + 4)?,
                    flags: u64_at(bytes, at + 8)?,
                    addr: u64_at(bytes, at + 16)?,
                    offset: u64_at(bytes, at + 24)?,
                    size: u64_at(bytes, at + 32)?,
                    link: u32_at(bytes, at + 40)?,
                    info: u32_at(bytes, at + 44)?,
                    align: u64_at(bytes, at + 48)?,
                    entsize: u64_at(bytes, at + 56)?,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        let names = *sections.get(names)?;
        Some(Self {
            bytes,
            sections,
            names,
        })
    }
    fn sections(&self) -> impl Iterator<Item = Section> + '_ {
        self.sections.iter().copied()
    }
    fn data(&self, section: &Section) -> Option<&'a [u8]> {
        if section.kind == SHT_NOBITS {
            return Some(&[]);
        }
        let start = section.offset as usize;
        self.bytes
            .get(start..start.checked_add(section.size as usize)?)
    }
    fn string(&self, table: &Section, at: u32) -> Option<&'a [u8]> {
        let data = self.data(table)?.get(at as usize..)?;
        Some(&data[..data.iter().position(|&b| b == 0)?])
    }
    fn name(&self, section: &Section) -> &'a [u8] {
        self.string(&self.names, section.name).unwrap_or_default()
    }
    /// The symbol table and its strings.
    fn symtab(&self) -> Option<(Section, Vec<Sym>)> {
        let table = self.sections().find(|s| s.kind == SHT_SYMTAB)?;
        let data = self.data(&table)?;
        let symbols = data
            .as_chunks::<24>()
            .0
            .iter()
            .map(|s| Sym {
                name: u32::from_le_bytes(s[0..4].try_into().unwrap()),
                info: s[4],
                other: s[5],
                shndx: u16::from_le_bytes(s[6..8].try_into().unwrap()),
                value: u64::from_le_bytes(s[8..16].try_into().unwrap()),
                size: u64::from_le_bytes(s[16..24].try_into().unwrap()),
            })
            .collect();
        Some((*self.sections.get(table.link as usize)?, symbols))
    }
}

/// The image's symbols by name: how many share it, and the last one's
/// index; and where the string table lies.
#[derive(Default)]
struct Symbols {
    by_name: rustc_hash::FxHashMap<Vec<u8>, (u32, usize)>,
    symbols: Vec<Sym>,
    strings: Section,
}
impl Symbols {
    fn of(image: &[u8]) -> Option<Self> {
        let elf = Elf::parse(image)?;
        let (strings, symbols) = elf.symtab()?;
        let mut by_name = rustc_hash::FxHashMap::default();
        for (n, symbol) in symbols.iter().enumerate() {
            let name = elf.string(&strings, symbol.name)?;
            if !name.is_empty() {
                let entry = by_name.entry(name.to_vec()).or_insert((0, n));
                *entry = (entry.0 + 1, n);
            }
        }
        Some(Self {
            by_name,
            symbols,
            strings,
        })
    }
    /// The one image symbol named `name`.
    fn unique(&self, name: &[u8]) -> Option<&Sym> {
        match self.by_name.get(name) {
            Some(&(1, n)) => Some(&self.symbols[n]),
            _ => None,
        }
    }
    fn rename(&mut self, image: &mut [u8], old: &[u8], new: Vec<u8>) {
        let (count, n) = self.by_name.remove(old).unwrap();
        let at = self.strings.offset as usize + self.symbols[n].name as usize;
        image[at..at + new.len()].copy_from_slice(&new);
        self.by_name.insert(new, (count, n));
    }
}

/// The bytes lld may write at and around a relocation's offset, by type;
/// None for a type this link does not patch around.
fn window(kind: u32) -> Option<(i64, i64)> {
    Some(match kind {
        0 => (0, 0),
        14 | 15 => (0, 1),
        12 | 13 => (0, 2),
        2 | 3 | 4 | 10 | 11 | 21 | 26 | 32 => (0, 4),
        1 | 16 | 17 | 18 | 24 | 25 | 27..=31 | 33 | 37 => (0, 8),
        // GOTPCREL, GOTPCRELX, REX_GOTPCRELX: relaxation rewrites the
        // opcode and REX prefix before the field.
        9 | 41 | 42 => (-3, 4),
        // TLS models: relaxation rewrites whole instruction sequences.
        19 | 20 | 22 | 23 | 34 | 35 | 43..=45 => (-16, 16),
        36 => (0, 16),
        _ => return None,
    })
}

/// The edits that turn the image linked with `old` into the one lld links
/// with `new` in its place, if `new` differs from `old` only as this module
/// patches.
fn edit(image: &[u8], symbols: &Symbols, old: &[u8], new: &[u8]) -> Option<Vec<Edit>> {
    let image = Elf::parse(image)?;
    let (o, n) = (Elf::parse(old)?, Elf::parse(new)?);
    if o.sections.len() != n.sections.len()
        || o.sections()
            .zip(n.sections())
            .any(|(a, b)| a.layout() != b.layout())
    {
        return None;
    }
    let (old_strings, old_symbols) = o.symtab()?;
    let (new_strings, new_symbols) = n.symtab()?;
    if old_symbols.len() != new_symbols.len() {
        return None;
    }
    let symtab_strings = o
        .sections()
        .position(|s| s.kind == SHT_SYMTAB)
        .map(|n| o.sections[n].link as usize)?;
    let mut edits = Vec::new();
    // Where each input section lies in the image, found once.
    let mut placed: rustc_hash::FxHashMap<usize, Option<usize>> = Default::default();
    let mut place = |index: usize| -> Option<usize> {
        *placed
            .entry(index)
            .or_insert_with(|| locate(&image, symbols, &o, &old_symbols, &old_strings, index))
    };
    for (a, b) in old_symbols.iter().zip(&new_symbols) {
        let (an, bn) = (
            o.string(&old_strings, a.name)?,
            n.string(&new_strings, b.name)?,
        );
        if (Sym { name: 0, ..*a }) != (Sym { name: 0, ..*b }) {
            return None;
        }
        if an == bn {
            continue;
        }
        // A renamed symbol keeps its length and its definition, and is
        // never exported, so only its string in the image changes.
        let bind = a.info >> 4;
        if an.len() != bn.len()
            || a.shndx == 0
            || a.shndx >= 0xff00
            || !matches!(bind, STB_LOCAL | STB_GLOBAL)
            || (bind == STB_GLOBAL && a.other & 3 == STV_DEFAULT)
            || symbols.by_name.contains_key(bn)
        {
            return None;
        }
        let at = place(a.shndx as usize)?;
        let held = symbols.unique(an)?;
        let section = image.sections.get(held.shndx as usize)?;
        if held.size != a.size
            || section.offset as usize + (held.value - section.addr) as usize
                != at + a.value as usize
            || image.string(&symbols.strings, held.name)? != an
        {
            return None;
        }
        // No other symbol's name lies in this one's string.
        let start = held.name as usize;
        if symbols
            .symbols
            .iter()
            .filter(|s| (start..=start + an.len()).contains(&(s.name as usize)))
            .count()
            != 1
        {
            return None;
        }
        edits.push(Edit::Rename(an.to_vec(), bn.to_vec()));
    }
    for (index, (section, other)) in o.sections().zip(n.sections()).enumerate() {
        let (a, b) = (o.data(&section)?, n.data(&other)?);
        if a == b || index == symtab_strings || section.kind == SHT_SYMTAB {
            continue;
        }
        let name = o.name(&section);
        if !matches!(section.kind, SHT_PROGBITS | SHT_NOTE)
            || section.flags & SHF_READ != 0
            || name == b".eh_frame"
            || name.starts_with(b".note.gnu.property")
            || name.starts_with(b".note.GNU")
            || name.starts_with(b".debug")
        {
            return None;
        }
        let at = place(index)?;
        let mut fixed = vec![false; a.len()];
        for rela in o
            .sections()
            .filter(|r| r.kind == SHT_RELA && r.info as usize == index)
        {
            for entry in o.data(&rela)?.as_chunks::<24>().0 {
                let offset = u64::from_le_bytes(entry[0..8].try_into().unwrap()) as i64;
                let kind = u32::from_le_bytes(entry[8..12].try_into().unwrap());
                let (low, high) = window(kind)?;
                let (low, high) = (
                    (offset + low).max(0) as usize,
                    (offset + high).max(0) as usize,
                );
                for f in fixed.iter_mut().take(high.min(a.len())).skip(low) {
                    *f = true;
                }
            }
        }
        let held = image.bytes.get(at..at + a.len())?;
        let mut d = 0;
        while d < a.len() {
            if fixed[d] {
                if a[d] != b[d] {
                    return None;
                }
                d += 1;
                continue;
            }
            // lld copied this byte from the old object.
            if held[d] != a[d] {
                return None;
            }
            if a[d] == b[d] {
                d += 1;
                continue;
            }
            let start = d;
            while d < a.len() && !fixed[d] && a[d] != b[d] {
                if held[d] != a[d] {
                    return None;
                }
                d += 1;
            }
            edits.push(Edit::Bytes(at + start, b[start..d].to_vec()));
        }
    }
    Some(edits)
}

/// Where in the image lld put the old object's section `index`: by a
/// symbol in it that the image holds once, or, for a section outside
/// memory, by the image's one section of its name and size.
fn locate(
    image: &Elf<'_>,
    symbols: &Symbols,
    object: &Elf<'_>,
    object_symbols: &[Sym],
    object_strings: &Section,
    index: usize,
) -> Option<usize> {
    let section = object.sections.get(index)?;
    if section.flags & SHF_ALLOC == 0 {
        let name = object.name(section);
        let mut same = image.sections().filter(|s| image.name(s) == name);
        let held = same.next()?;
        if same.next().is_some() || held.size != section.size || held.kind == SHT_NOBITS {
            return None;
        }
        return Some(held.offset as usize);
    }
    for symbol in object_symbols {
        let (bind, kind) = (symbol.info >> 4, symbol.info & 15);
        if symbol.shndx as usize != index
            || matches!(kind, STT_SECTION | STT_FILE | STT_TLS)
            || !matches!(bind, STB_LOCAL | STB_GLOBAL)
        {
            continue;
        }
        let name = object.string(object_strings, symbol.name)?;
        if name.is_empty() || name.starts_with(b".L") {
            continue;
        }
        let Some(held) = symbols.unique(name) else {
            continue;
        };
        let out = image.sections.get(held.shndx as usize)?;
        let start = held.value.checked_sub(symbol.value)?;
        if out.kind == SHT_NOBITS
            || out.flags & SHF_ALLOC == 0
            || start < out.addr
            || start + section.size > out.addr + out.size
        {
            return None;
        }
        return Some((out.offset + start - out.addr) as usize);
    }
    None
}

#[cfg(test)]
mod tests;
