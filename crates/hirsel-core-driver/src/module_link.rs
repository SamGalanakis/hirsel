//! The link of a `.hirsel-mod` (PACK-2): a release's relocatable objects become one
//! pointer-free image. It runs in the `Compiler` worker; a pack host only applies the
//! image's closed fixups.
//!
//! Sections reached from the roots are laid out in two bands. Every call and branch
//! inside the image is resolved here. A reference to an import goes through the import's
//! slot at the start of `data`; a reference the code makes through a global-offset
//! entry goes through a pointer slot after them, or, where the instruction can take the
//! address directly, straight to its target.
use hirsel_core_ids::ContentHash;
use hirsel_core_layout::Arch;
use hirsel_mod::{Class, Entry, Fixup, Header, Image, Import, Kind, NO_DESCRIPTOR, ROWS, Span};
use object::elf;
use object::{
    Object, ObjectSection, ObjectSymbol, RelocationFlags, RelocationTarget, SectionFlags,
    SectionIndex, SymbolIndex, SymbolSection,
};
use std::collections::{BTreeMap, BTreeSet};

#[cfg(test)]
#[path = "module_link/tests.rs"]
mod tests;

/// An entry the image exports: optional explicit entry identity (else derived from
/// `symbol`), its exported function symbol, optional PROG-9 descriptor symbol, the
/// PACK-14 `hirsel.pack-export.v1` signature digest, and whether it waits.
pub(crate) struct Root<'a> {
    pub identity: Option<ContentHash>,
    pub symbol: &'a str,
    pub descriptor: Option<&'a str>,
    pub signature: ContentHash,
    pub waits: bool,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Band {
    Text,
    Data,
}

/// A section of one object: `(object, section)`.
type Part = (usize, usize);

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Target {
    /// An import row.
    Row(u32),
    /// An offset in a section the image holds.
    Place(Part, u64),
    Absolute(u64),
}

struct Link<'a> {
    arch: Arch,
    objects: Vec<object::File<'a>>,
    /// The definition every global name resolves to.
    globals: BTreeMap<&'a [u8], (usize, SymbolIndex)>,
    bands: BTreeMap<Part, Band>,
    /// Where each kept section starts in its band.
    starts: BTreeMap<Part, u64>,
    rows: BTreeMap<u32, u32>,
    pointers: BTreeMap<Target, u32>,
}

fn refuse<T>(reason: impl std::fmt::Display) -> Result<T, String> {
    Err(reason.to_string())
}

fn elf_type(flags: RelocationFlags) -> Result<u32, String> {
    match flags {
        RelocationFlags::Elf { r_type } => Ok(r_type.0),
        other => refuse(format_args!("relocation {other:?}")),
    }
}

/// How a relocation type uses its target.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Use {
    /// A call or branch to code in the image.
    Branch,
    /// A load of the target's address from a pointer slot.
    Pointer,
    /// The second instruction of an aarch64 pointer load.
    PointerLow,
    /// A 64-bit address word.
    Word,
}

const R_X86_64_PLT32: u32 = elf::R_X86_64_PLT32.0;
const R_X86_64_PC32: u32 = elf::R_X86_64_PC32.0;
const R_X86_64_GOTPCREL: u32 = elf::R_X86_64_GOTPCREL.0;
const R_X86_64_GOTPCRELX: u32 = elf::R_X86_64_GOTPCRELX.0;
const R_X86_64_REX_GOTPCRELX: u32 = elf::R_X86_64_REX_GOTPCRELX.0;
const R_X86_64_64: u32 = elf::R_X86_64_64.0;
const R_AARCH64_CALL26: u32 = elf::R_AARCH64_CALL26.0;
const R_AARCH64_JUMP26: u32 = elf::R_AARCH64_JUMP26.0;
const R_AARCH64_ADR_GOT_PAGE: u32 = elf::R_AARCH64_ADR_GOT_PAGE.0;
const R_AARCH64_LD64_GOT_LO12_NC: u32 = elf::R_AARCH64_LD64_GOT_LO12_NC.0;
const R_AARCH64_ABS64: u32 = elf::R_AARCH64_ABS64.0;

fn use_of(arch: Arch, r_type: u32) -> Result<Use, String> {
    Ok(match arch {
        Arch::X86_64 => match r_type {
            R_X86_64_PLT32 | R_X86_64_PC32 => Use::Branch,
            R_X86_64_GOTPCREL | R_X86_64_GOTPCRELX | R_X86_64_REX_GOTPCRELX => Use::Pointer,
            R_X86_64_64 => Use::Word,
            other => return refuse(format_args!("x86-64 relocation type {other}")),
        },
        Arch::Aarch64 => match r_type {
            R_AARCH64_CALL26 | R_AARCH64_JUMP26 => Use::Branch,
            R_AARCH64_ADR_GOT_PAGE => Use::Pointer,
            R_AARCH64_LD64_GOT_LO12_NC => Use::PointerLow,
            R_AARCH64_ABS64 => Use::Word,
            other => return refuse(format_args!("aarch64 relocation type {other}")),
        },
    })
}

impl<'a> Link<'a> {
    fn band(
        &self,
        object: usize,
        section: &object::Section<'a, '_>,
    ) -> Result<Option<Band>, String> {
        let SectionFlags::Elf { sh_flags, .. } = section.flags() else {
            return refuse("an object that is not ELF");
        };
        let name = section.name().map_err(|e| e.to_string())?;
        if !sh_flags.contains(elf::SHF_ALLOC) || name.starts_with(".eh_frame") {
            return Ok(None);
        }
        if sh_flags.contains(elf::SHF_TLS) {
            return refuse(format_args!("thread-local section {name}"));
        }
        if sh_flags.contains(elf::SHF_EXECINSTR) {
            return Ok(Some(Band::Text));
        }
        // Relocated constants are writable only until a link applies them, and a
        // zero-initialised object here is an empty carrier nothing writes.
        let constant = !sh_flags.contains(elf::SHF_WRITE)
            || name.starts_with(".data.rel.ro")
            || name.starts_with(".bss");
        if !constant {
            return refuse(format_args!(
                "object {object} has writable data section {name}"
            ));
        }
        Ok(Some(Band::Data))
    }

    /// What `symbol` of `object` names, with `addend` applied to a place.
    fn resolve(&self, object: usize, symbol: SymbolIndex, addend: i64) -> Result<Target, String> {
        let file = &self.objects[object];
        let mut found = file.symbol_by_index(symbol).map_err(|e| e.to_string())?;
        let mut home = object;
        if found.is_global() || found.is_undefined() {
            let name = found.name_bytes().map_err(|e| e.to_string())?;
            match self.globals.get(name) {
                Some(&(other, index)) => {
                    home = other;
                    found = self.objects[other]
                        .symbol_by_index(index)
                        .map_err(|e| e.to_string())?;
                }
                None => {
                    let name = String::from_utf8_lossy(name);
                    let Some(row) = hirsel_mod::row(&name) else {
                        return refuse(format_args!("import {name} is not in the row table"));
                    };
                    let class = ROWS[row as usize].1;
                    if matches!(class, Class::Foreign | Class::Exec) {
                        return refuse(format_args!("the release reaches {name}"));
                    }
                    if addend != 0 {
                        return refuse(format_args!("an offset into import {name}"));
                    }
                    return Ok(Target::Row(row));
                }
            }
        }
        match found.section() {
            SymbolSection::Absolute => Ok(Target::Absolute(
                found.address().wrapping_add_signed(addend),
            )),
            SymbolSection::Section(section) => {
                let offset = found.address().checked_add_signed(addend);
                match offset {
                    Some(offset) => Ok(Target::Place((home, section.0), offset)),
                    None => refuse("a relocation before its section"),
                }
            }
            other => refuse(format_args!("a symbol in {other:?}")),
        }
    }

    /// Whether the x86-64 pointer load at `site` of `data` is a `mov` that can take
    /// its target's address instead (`mov r64, [rip + d]` to `lea r64, [rip + d]`).
    fn takes_address(&self, data: &[u8], site: u64, target: Target) -> bool {
        let x86 = match self.arch {
            Arch::X86_64 => true,
            Arch::Aarch64 => false,
        };
        x86 && matches!(target, Target::Place(..))
            && site >= 2
            && data.get(site as usize - 2) == Some(&0x8b)
    }

    fn place(&self, target: Target) -> Result<(Band, u64), String> {
        let Target::Place(part, offset) = target else {
            unreachable!("only a place has an offset")
        };
        match (self.bands.get(&part), self.starts.get(&part)) {
            (Some(&band), Some(&start)) => Ok((band, start + offset)),
            _ => refuse("a reference into a section the image does not hold"),
        }
    }

    /// The data offset of the slot a pointer load of `target` reads.
    fn slot(&self, target: Target) -> u64 {
        match target {
            Target::Row(row) => 8 * u64::from(self.rows[&row]),
            _ => 8 * (self.rows.len() as u64 + u64::from(self.pointers[&target])),
        }
    }
}

fn trap(arch: Arch, text: &mut Vec<u8>, to: usize) {
    match arch {
        Arch::X86_64 => text.resize(to, 0xcc),
        Arch::Aarch64 => {
            // `brk #0`. Code sections are instruction-aligned, so a gap is whole words.
            while text.len() < to {
                text.extend_from_slice(&0xd420_0000u32.to_le_bytes());
            }
        }
    }
}

fn offset32(value: u64, what: &str) -> Result<u32, String> {
    u32::try_from(value).map_err(|_| format!("{what} exceeds a module's 4 GiB"))
}

/// Link `objects` into an image whose entries are `roots`.
pub(crate) fn link(
    header: Header,
    objects: &[&[u8]],
    roots: &[Root<'_>],
    sites: Vec<u8>,
) -> Result<Image, String> {
    let arch = header.target.arch();
    let mut link = Link {
        arch,
        objects: Vec::with_capacity(objects.len()),
        globals: BTreeMap::new(),
        bands: BTreeMap::new(),
        starts: BTreeMap::new(),
        rows: BTreeMap::new(),
        pointers: BTreeMap::new(),
    };
    for bytes in objects {
        let file = object::File::parse(*bytes).map_err(|e| e.to_string())?;
        let matches = match arch {
            Arch::X86_64 => file.architecture() == object::Architecture::X86_64,
            Arch::Aarch64 => file.architecture() == object::Architecture::Aarch64,
        };
        if file.format() != object::BinaryFormat::Elf || !matches {
            return refuse(format_args!(
                "a {:?} {:?} object for {}",
                file.format(),
                file.architecture(),
                header.target.triple()
            ));
        }
        link.objects.push(file);
    }
    // A strong definition replaces a weak one; among weak ones the first holds.
    let mut strong = BTreeSet::new();
    for (i, file) in link.objects.iter().enumerate() {
        for symbol in file.symbols() {
            if !symbol.is_global() || symbol.is_undefined() {
                continue;
            }
            let name = symbol.name_bytes().map_err(|e| e.to_string())?;
            if !symbol.is_weak() && !strong.insert(name) {
                return refuse(format_args!(
                    "{} is defined twice",
                    String::from_utf8_lossy(name)
                ));
            }
            if !symbol.is_weak() || !link.globals.contains_key(name) {
                link.globals.insert(name, (i, symbol.index()));
            }
        }
    }

    // Keep the sections the roots reach.
    let mut pending = Vec::new();
    let mut entries = Vec::new();
    for root in roots {
        let Some(&(object, symbol)) = link.globals.get(root.symbol.as_bytes()) else {
            return refuse(format_args!("entry {} is not defined", root.symbol));
        };
        let Target::Place(part, _) = link.resolve(object, symbol, 0)? else {
            return refuse(format_args!("entry {} is not code", root.symbol));
        };
        pending.push(part);
        let descriptor = root
            .descriptor
            .map(str::to_owned)
            .unwrap_or_else(|| format!("{}$static", root.symbol));
        if let Some(&(object, symbol)) = link.globals.get(descriptor.as_bytes())
            && let Target::Place(part, _) = link.resolve(object, symbol, 0)?
        {
            pending.push(part);
        }
        entries.push((root, (object, symbol)));
    }
    let mut uses = Vec::new();
    let mut row_stubs: BTreeMap<u32, u64> = BTreeMap::new();
    while let Some(part) = pending.pop() {
        if link.bands.contains_key(&part) {
            continue;
        }
        let section = link.objects[part.0]
            .section_by_index(SectionIndex(part.1))
            .map_err(|e| e.to_string())?;
        let Some(band) = link.band(part.0, &section)? else {
            return refuse(format_args!(
                "a reference into section {}",
                section.name().unwrap_or("?")
            ));
        };
        link.bands.insert(part, band);
        let data = section.data().map_err(|e| e.to_string())?;
        for (site, relocation) in section.relocations() {
            let RelocationTarget::Symbol(symbol) = relocation.target() else {
                return refuse("a relocation without a symbol");
            };
            let kind = use_of(arch, elf_type(relocation.flags())?)?;
            let addend = relocation.addend();
            // Only an address word's addend is part of its target; a branch's and a
            // pointer load's also measure from the instruction.
            let word = matches!(kind, Use::Word);
            let target = link.resolve(part.0, symbol, if word { addend } else { 0 })?;
            if let Target::Place(to, _) = target {
                pending.push(to);
            }
            match (kind, band, target) {
                (Use::Word, Band::Data, Target::Place(..) | Target::Absolute(_)) => {}
                (Use::Branch, Band::Text, Target::Place(..)) => {}
                (Use::Branch, Band::Text, Target::Row(row)) => {
                    link.rows.insert(row, 0);
                    row_stubs.insert(row, 0);
                }
                (Use::Pointer | Use::PointerLow, Band::Text, Target::Row(row)) => {
                    link.rows.insert(row, 0);
                }
                (Use::Pointer | Use::PointerLow, Band::Text, _) => {
                    if !link.takes_address(data, site, target) {
                        link.pointers.insert(target, 0);
                    }
                }
                _ => {
                    return refuse(format_args!(
                        "relocation type {} ({kind:?} in {band:?} -> {target:?}) at {site:#x} of {}",
                        elf_type(relocation.flags())?,
                        section.name().unwrap_or("?")
                    ));
                }
            }
            uses.push((part, site, kind, target, addend));
        }
    }
    for (i, slot) in link.rows.values_mut().enumerate() {
        *slot = i as u32;
    }
    for (i, slot) in link.pointers.values_mut().enumerate() {
        *slot = i as u32;
    }

    // Lay the kept sections out, in object and section order.
    let mut text = Vec::new();
    let mut data = vec![0u8; 8 * link.pointers.len()];
    let slots = 8 * link.rows.len() as u64;
    let (mut text_align, mut data_align) = (64u64, 64u64);
    let mut spans = BTreeMap::new();
    for (&part, &band) in &link.bands {
        let file = &link.objects[part.0];
        let section = file
            .section_by_index(SectionIndex(part.1))
            .map_err(|e| e.to_string())?;
        let align = section.align().max(1);
        let bytes = section.data().map_err(|e| e.to_string())?;
        match band {
            Band::Text => {
                text_align = text_align.max(align);
                let start = text.len().next_multiple_of(align as usize);
                trap(arch, &mut text, start);
                text.extend_from_slice(bytes);
                link.starts.insert(part, start as u64);
                for symbol in file.symbols() {
                    if symbol.section_index() == Some(SectionIndex(part.1))
                        && symbol.kind() == object::SymbolKind::Text
                        && symbol.size() != 0
                    {
                        let name = symbol.name().map_err(|e| e.to_string())?;
                        spans
                            .entry(start as u64 + symbol.address())
                            .or_insert((symbol.size(), name));
                    }
                }
            }
            Band::Data => {
                data_align = data_align.max(align);
                let start = (slots as usize + data.len()).next_multiple_of(align as usize);
                data.resize(start - slots as usize, 0);
                if bytes.is_empty() {
                    data.resize(data.len() + section.size() as usize, 0);
                } else {
                    data.extend_from_slice(bytes);
                }
                link.starts.insert(part, start as u64);
            }
        }
    }

    let mut text_fixups = Vec::new();
    let mut data_fixups = Vec::new();
    // Emit one indirect-jump stub in `.text` for each runtime row called through a
    // direct PLT/PC-relative branch (e.g. LLVM PIC calls / synthesized libcalls).
    for (&row, stub_offset) in &mut row_stubs {
        let start = text.len().next_multiple_of(16);
        trap(arch, &mut text, start);
        *stub_offset = start as u64;
        let slot_off = 8 * i64::from(link.rows[&row]);
        match arch {
            Arch::X86_64 => {
                // `jmp qword ptr [rip + slot]`
                text.extend_from_slice(&[0xff, 0x25, 0, 0, 0, 0]);
                text_fixups.push(Fixup {
                    site: offset32(start as u64 + 2, "text")?,
                    kind: Kind::Rel32Data,
                    bias: 4,
                    addend: slot_off,
                });
            }
            Arch::Aarch64 => {
                // `adrp x16, slot; ldr x16, [x16, lo12(slot)]; br x16`
                for word in [0x9000_0010u32, 0xf940_0210, 0xd61f_0200] {
                    text.extend_from_slice(&word.to_le_bytes());
                }
                text_fixups.push(Fixup {
                    site: offset32(start as u64, "text")?,
                    kind: Kind::PageData,
                    bias: 0,
                    addend: slot_off,
                });
                text_fixups.push(Fixup {
                    site: offset32(start as u64 + 4, "text")?,
                    kind: Kind::Low12LoadData,
                    bias: 0,
                    addend: slot_off,
                });
            }
        }
    }
    for (target, slot) in &link.pointers {
        let site = offset32(slots + 8 * u64::from(*slot), "data")?;
        match *target {
            Target::Absolute(value) => {
                let at = (u64::from(site) - slots) as usize;
                data[at..at + 8].copy_from_slice(&value.to_le_bytes());
            }
            target => {
                let (band, offset) = link.place(target)?;
                data_fixups.push(Fixup {
                    site,
                    kind: match band {
                        Band::Text => Kind::Abs64Text,
                        Band::Data => Kind::Abs64Data,
                    },
                    bias: 0,
                    addend: offset as i64,
                });
            }
        }
    }
    for (part, site, kind, target, addend) in uses {
        let band = link.bands[&part];
        let at = link.starts[&part] + site;
        let field = match band {
            Band::Text => at as usize,
            Band::Data => (at - slots) as usize,
        };
        let abs64 = |to: Band, offset: u64| -> Result<Fixup, String> {
            Ok(Fixup {
                site: offset32(at, "data")?,
                kind: match to {
                    Band::Text => Kind::Abs64Text,
                    Band::Data => Kind::Abs64Data,
                },
                bias: 0,
                addend: offset as i64,
            })
        };
        match kind {
            Use::Word => match target {
                Target::Absolute(value) => {
                    data[field..field + 8].copy_from_slice(&value.to_le_bytes())
                }
                target => {
                    let (to, offset) = link.place(target)?;
                    data_fixups.push(abs64(to, offset)?);
                }
            },
            Use::Branch => {
                let (to, offset) = match target {
                    Target::Row(row) => (Band::Text, row_stubs[&row]),
                    other => link.place(other)?,
                };
                match (to, arch) {
                    (Band::Data, Arch::X86_64) => {
                        // x86-64 PIC `R_X86_64_PC32` addressing local data (`[rip + disp32]`).
                        // `S + A - P = (D + offset + addend + bias) - (T + at + bias)`.
                        let bias = if offset as i64 + addend + 4 >= 0 {
                            4u8
                        } else {
                            (-(offset as i64 + addend)).clamp(4, 8) as u8
                        };
                        text_fixups.push(Fixup {
                            site: offset32(at, "text")?,
                            kind: Kind::Rel32Data,
                            bias,
                            addend: offset as i64 + addend + i64::from(bias),
                        });
                        continue;
                    }
                    (Band::Data, Arch::Aarch64) => return refuse("a branch into data"),
                    (Band::Text, Arch::X86_64) => {
                        let distance = offset as i64 + addend - at as i64;
                        let Ok(value) = i32::try_from(distance) else {
                            return refuse("a branch farther than 2 GiB");
                        };
                        text[field..field + 4].copy_from_slice(&value.to_le_bytes());
                    }
                    (Band::Text, Arch::Aarch64) => {
                        let distance = offset as i64 + addend - at as i64;
                        if distance % 4 != 0 || !(-(1 << 27)..1 << 27).contains(&distance) {
                            return refuse("a branch farther than 128 MiB");
                        }
                        let word = u32::from_le_bytes(text[field..field + 4].try_into().unwrap());
                        let word = word & 0xfc00_0000 | (distance >> 2) as u32 & 0x03ff_ffff;
                        text[field..field + 4].copy_from_slice(&word.to_le_bytes());
                    }
                }
            }
            Use::Pointer | Use::PointerLow => {
                let fixed = offset32(at, "text")?;
                let x86 = match arch {
                    Arch::X86_64 => true,
                    Arch::Aarch64 => false,
                };
                let bias = match u8::try_from(-addend) {
                    Ok(bias) if x86 && (4..=8).contains(&bias) => bias,
                    Ok(0) if !x86 => 0,
                    _ => return refuse("a pointer load's addend"),
                };
                let original = link.objects[part.0]
                    .section_by_index(SectionIndex(part.1))
                    .and_then(|s| s.data())
                    .map_err(|e| e.to_string())?;
                if link.takes_address(original, site, target) {
                    let (to, offset) = link.place(target)?;
                    text[field - 2] = 0x8d;
                    match to {
                        Band::Text => {
                            let distance = offset as i64 - (at as i64 + i64::from(bias));
                            let Ok(value) = i32::try_from(distance) else {
                                return refuse("code farther than 2 GiB");
                            };
                            text[field..field + 4].copy_from_slice(&value.to_le_bytes());
                        }
                        Band::Data => text_fixups.push(Fixup {
                            site: fixed,
                            kind: Kind::Rel32Data,
                            bias,
                            addend: offset as i64,
                        }),
                    }
                } else {
                    text_fixups.push(Fixup {
                        site: fixed,
                        kind: match (x86, kind) {
                            (true, _) => Kind::Rel32Data,
                            (false, Use::Pointer) => Kind::PageData,
                            (false, _) => Kind::Low12LoadData,
                        },
                        bias,
                        addend: link.slot(target) as i64,
                    });
                }
            }
        }
    }

    let mut image = Image {
        header: Header {
            text_align: offset32(text_align, "text alignment")?,
            data_align: offset32(data_align, "data alignment")?,
            ..header
        },
        imports: link.rows.keys().map(|row| Import::Row(*row)).collect(),
        text_fixups,
        data_fixups,
        entries: Vec::new(),
        spans: Vec::new(),
        cfi: Vec::new(),
        sites,
        names: Vec::new(),
        text,
        data,
    };
    offset32(image.text.len() as u64, "text")?;
    offset32(slots + image.data.len() as u64, "data")?;
    let mut free = 0;
    for (start, (size, name)) in spans {
        // Two names of one function keep its first.
        if start >= free {
            image.spans.push(Span {
                start: start as u32,
                length: size as u32,
            });
            image.names.push(name.to_owned());
            free = start + size;
        }
    }
    for (root, (object, symbol)) in entries {
        let Some(identity) = root.identity.or_else(|| identity(root.symbol)) else {
            return refuse(format_args!("entry {} has no code identity", root.symbol));
        };
        let (band, offset) = link.place(link.resolve(object, symbol, 0)?)?;
        if band != Band::Text {
            return refuse(format_args!("entry {} is not code", root.symbol));
        }
        let descriptor = root
            .descriptor
            .map(str::to_owned)
            .unwrap_or_else(|| format!("{}$static", root.symbol));
        let descriptor = match link.globals.get(descriptor.as_bytes()) {
            Some(&(object, symbol)) => link.place(link.resolve(object, symbol, 0)?)?.1 as u32,
            None => NO_DESCRIPTOR,
        };
        image.entries.push(Entry {
            identity,
            signature: root.signature,
            text: offset as u32,
            descriptor,
            waits: root.waits,
        });
    }
    Ok(image)
}

/// The code identity a function symbol `h<64 hex digits>` spells.
fn identity(symbol: &str) -> Option<ContentHash> {
    let hex = symbol.strip_prefix('h')?.as_bytes();
    if hex.len() != 64 {
        return None;
    }
    let mut out = [0; 32];
    for (byte, pair) in out.iter_mut().zip(hex.chunks_exact(2)) {
        *byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(ContentHash(out))
}
