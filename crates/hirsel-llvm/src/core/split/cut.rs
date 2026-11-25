//! The Mach-O arm64 form of the per-part cut. The rules are the ELF cut's
//! (`super`); what differs is where Mach-O keeps a definition:
//! - all functions share one `__TEXT,__text` and every constant shares its
//!   kind's section; with MH_SUBSECTIONS_VIA_SYMBOLS each symbol starts an
//!   atom (as ld64 and lld split them), so a part takes atoms, not sections;
//! - relocations carry their addend in place or in a preceding
//!   ARM64_RELOC_ADDEND, and a SUBTRACTOR precedes its UNSIGNED: both stay
//!   with their partner, in source order;
//! - a function's unwind is a `__LD,__compact_unwind` entry (its address a
//!   section-relative non-extern UNSIGNED, rewritten here) and, for DWARF
//!   mode, an FDE in `__TEXT,__eh_frame` whose pc-begin is
//!   `SUBTRACTOR(section start) + UNSIGNED(function)` with the field's
//!   negated offset in place;
//! - `ltmpN` section-start symbols are not atoms and not copied; linker
//!   optimisation hints inside the part are rebased onto it.
//!
//! Byte layouts are <mach-o/loader.h>, <mach-o/nlist.h>, <mach-o/reloc.h>,
//! <mach-o/arm64/reloc.h> and <mach-o/compact_unwind_encoding.h>.
use super::{error, restrict, uleb};
use hirsel_core_ids::ContentHash;
use hirsel_core_ir::source::NativeObject;
use std::collections::{HashMap, HashSet, VecDeque};

const HEADER: usize = 32;
const LC_SEGMENT_64: u32 = 0x19;
const LC_SYMTAB: u32 = 0x2;
const LC_DYSYMTAB: u32 = 0xb;
const LC_BUILD_VERSION: u32 = 0x32;
const LC_DATA_IN_CODE: u32 = 0x29;
const LC_LINKER_OPTIMIZATION_HINT: u32 = 0x2e;
const SECTION_TYPE: u32 = 0xff;
const S_REGULAR: u32 = 0x0;
const S_ZEROFILL: u32 = 0x1;
/// The alignment (log2) of every definition in a split module.
const ATOM_ALIGN: u32 = 4;
/// AArch64 `nop`, little-endian: LLVM's padding between functions.
const NOP: [u8; 4] = [0x1f, 0x20, 0x03, 0xd5];
const S_CSTRING_LITERALS: u32 = 0x2;
const S_4BYTE_LITERALS: u32 = 0x3;
const S_8BYTE_LITERALS: u32 = 0x4;
const S_COALESCED: u32 = 0xb;
const S_16BYTE_LITERALS: u32 = 0xe;
const S_ATTR_PURE_INSTRUCTIONS: u32 = 0x8000_0000;
const S_ATTR_SOME_INSTRUCTIONS: u32 = 0x0000_0400;
const S_ATTR_DEBUG: u32 = 0x0200_0000;
const N_STAB: u8 = 0xe0;
const N_PEXT: u8 = 0x10;
const N_TYPE: u8 = 0x0e;
const N_EXT: u8 = 0x01;
const N_UNDF: u8 = 0x0;
const N_SECT: u8 = 0xe;
const N_WEAK_DEF: u16 = 0x0080;
const N_ALT_ENTRY: u16 = 0x0200;
const ARM64_RELOC_UNSIGNED: u8 = 0;
const ARM64_RELOC_SUBTRACTOR: u8 = 1;
const ARM64_RELOC_TLVP_LOAD_PAGE21: u8 = 8;
const ARM64_RELOC_TLVP_LOAD_PAGEOFF12: u8 = 9;
const ARM64_RELOC_ADDEND: u8 = 10;
/// One `compact_unwind_entry`: function (8), length (4), encoding (4),
/// personality (8), LSDA (8).
const COMPACT_ENTRY: usize = 32;

/// S_ZEROFILL, S_GB_ZEROFILL and S_THREAD_LOCAL_ZEROFILL have no file bytes.
fn zerofill(flags: u32) -> bool {
    matches!(flags & SECTION_TYPE, 0x1 | 0xc | 0x12)
}

fn word(bytes: &[u8], at: usize) -> Result<u32, String> {
    bytes
        .get(at..at + 4)
        .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
        .ok_or_else(|| "truncated Mach-O object".to_owned())
}
fn quad(bytes: &[u8], at: usize) -> Result<u64, String> {
    bytes
        .get(at..at + 8)
        .map(|b| u64::from_le_bytes(b.try_into().unwrap()))
        .ok_or_else(|| "truncated Mach-O object".to_owned())
}
fn name16(bytes: &[u8]) -> &[u8] {
    &bytes[..bytes.iter().position(|&b| b == 0).unwrap_or(16)]
}

#[derive(Clone, Copy)]
struct Reloc {
    /// Offset in its section.
    address: u32,
    /// A symbol index (extern), a section ordinal (not extern), or an
    /// ADDEND's value.
    symbol: u32,
    pcrel: bool,
    length: u8,
    external: bool,
    kind: u8,
}

struct Section<'a> {
    header: &'a [u8],
    addr: u64,
    size: u64,
    /// log2.
    align: u32,
    flags: u32,
    data: &'a [u8],
    /// By address; a pair's two entries keep their order.
    relocations: Vec<Reloc>,
    /// Atom starts, as offsets: every symbol but an alt entry or `ltmpN`.
    starts: Vec<u64>,
    /// Its symbols by offset: (offset, symbol index).
    symbols: Vec<(u64, usize)>,
}
impl Section<'_> {
    /// The relocations whose places lie in `start..end`.
    fn relocations_in(&self, start: u64, end: u64) -> &[Reloc] {
        let at = |x: u64| {
            self.relocations
                .partition_point(|r| u64::from(r.address) < x)
        };
        &self.relocations[at(start)..at(end)]
    }
    fn named(&self, segment: &[u8], section: &[u8]) -> bool {
        name16(&self.header[16..32]) == segment && name16(&self.header[..16]) == section
    }
    fn name(&self) -> String {
        format!(
            "{},{}",
            String::from_utf8_lossy(name16(&self.header[16..32])),
            String::from_utf8_lossy(name16(&self.header[..16]))
        )
    }
    fn kind(&self) -> u32 {
        self.flags & SECTION_TYPE
    }
    fn text(&self) -> bool {
        self.flags & (S_ATTR_PURE_INSTRUCTIONS | S_ATTR_SOME_INSTRUCTIONS) != 0
    }
    /// The fixed entry size of a literal section; a C string's is its own.
    fn literal(&self) -> Option<u64> {
        match self.kind() {
            S_CSTRING_LITERALS => Some(0),
            S_4BYTE_LITERALS => Some(4),
            S_8BYTE_LITERALS => Some(8),
            S_16BYTE_LITERALS => Some(16),
            _ => None,
        }
    }
    /// The atom holding `offset`: its start and end.
    fn atom(&self, offset: u64) -> (u64, u64) {
        let at = self.starts.partition_point(|&s| s <= offset);
        let start = if at == 0 { 0 } else { self.starts[at - 1] };
        (start, self.starts.get(at).copied().unwrap_or(self.size))
    }
}

struct Symbol<'a> {
    name: &'a [u8],
    kind: u8,
    /// 0-based section index; None when not in a section.
    section: Option<usize>,
    desc: u16,
    /// Offset in its section, or the raw value outside one.
    value: u64,
}
impl Symbol<'_> {
    fn external(&self) -> bool {
        self.kind & N_EXT != 0
    }
    /// LLVM's section-start label, which no relocation but an FDE's
    /// subtrahend names.
    fn section_start(&self) -> bool {
        self.name.starts_with(b"ltmp") && self.kind & N_EXT == 0
    }
}

/// A DWARF unwind record of one function.
struct Fde {
    /// The function's symbol and its offset in the text.
    symbol: usize,
    function: u64,
    /// The record's bytes in `__eh_frame`, and its CIE's start.
    record: (usize, usize),
    cie: usize,
    /// The pc-begin field's in-place value, less its own place: what the
    /// field adds past the subtrahend once placed anywhere.
    past: u64,
}

/// A linker optimisation hint: its kind and the addresses it names, with
/// their least and greatest.
struct Hint {
    low: u64,
    high: u64,
    kind: u64,
    addresses: Vec<u64>,
}

struct Input<'a> {
    header: &'a [u8],
    build: Option<&'a [u8]>,
    sections: Vec<Section<'a>>,
    symbols: Vec<Symbol<'a>>,
    /// By least address.
    hints: Vec<Hint>,
    text: usize,
    compact: Option<usize>,
    unwind: Option<usize>,
    /// Text definitions by name.
    names: HashMap<&'a [u8], usize>,
    /// Compact unwind entries by function offset: (offset, entry offset).
    entries: Vec<(u64, usize)>,
    /// DWARF unwind records by function offset.
    fdes: Vec<Fde>,
}

impl<'a> Input<'a> {
    fn parse(data: &'a [u8]) -> Result<Self, String> {
        // MH_OBJECT.
        if word(data, 12)? != 1 {
            return Err("release object is not a Mach-O relocatable object".into());
        }
        let mut input = Input {
            header: &data[..HEADER],
            build: None,
            sections: Vec::new(),
            symbols: Vec::new(),
            hints: Vec::new(),
            text: usize::MAX,
            compact: None,
            unwind: None,
            names: HashMap::new(),
            entries: Vec::new(),
            fdes: Vec::new(),
        };
        let mut at = HEADER;
        let mut symtab = None;
        for _ in 0..word(data, 16)? {
            let (cmd, size) = (word(data, at)?, word(data, at + 4)? as usize);
            match cmd {
                LC_SEGMENT_64 => {
                    for i in 0..word(data, at + 64)? as usize {
                        let h = at + 72 + i * 80;
                        let header = data.get(h..h + 80).ok_or("truncated Mach-O section")?;
                        let (size, offset) = (quad(header, 40)?, word(header, 48)? as usize);
                        let flags = word(header, 64)?;
                        let mut relocations = Vec::new();
                        let reloff = word(header, 56)? as usize;
                        for r in 0..word(header, 60)? as usize {
                            let (address, info) =
                                (word(data, reloff + r * 8)?, word(data, reloff + r * 8 + 4)?);
                            if address & 0x8000_0000 != 0 {
                                return Err("release object has a scattered relocation".into());
                            }
                            relocations.push(Reloc {
                                address,
                                symbol: info & 0x00ff_ffff,
                                pcrel: info >> 24 & 1 != 0,
                                length: (info >> 25 & 3) as u8,
                                external: info >> 27 & 1 != 0,
                                kind: (info >> 28) as u8,
                            });
                        }
                        input.sections.push(Section {
                            header,
                            addr: quad(header, 32)?,
                            size,
                            align: word(header, 52)?,
                            flags,
                            data: if zerofill(flags) {
                                &[]
                            } else {
                                data.get(offset..offset + size as usize)
                                    .ok_or("truncated Mach-O section data")?
                            },
                            relocations,
                            starts: Vec::new(),
                            symbols: Vec::new(),
                        });
                    }
                }
                LC_BUILD_VERSION => input.build = Some(&data[at..at + size]),
                LC_SYMTAB => symtab = Some(at),
                LC_DYSYMTAB => {}
                LC_DATA_IN_CODE | LC_LINKER_OPTIMIZATION_HINT => {
                    let (offset, length) =
                        (word(data, at + 8)? as usize, word(data, at + 12)? as usize);
                    let bytes = data
                        .get(offset..offset + length)
                        .ok_or("truncated Mach-O hints")?;
                    if cmd == LC_DATA_IN_CODE {
                        if length > 0 {
                            return Err("release object has data in code".into());
                        }
                        continue;
                    }
                    let mut at = 0;
                    while at < bytes.len() {
                        let (kind, n) = uleb(bytes, at)?;
                        at += n;
                        // Zero bytes pad the hints to a pointer's size.
                        if kind == 0 {
                            break;
                        }
                        let (count, n) = uleb(bytes, at)?;
                        at += n;
                        let mut addresses = Vec::new();
                        for _ in 0..count {
                            let (address, n) = uleb(bytes, at)?;
                            at += n;
                            addresses.push(address);
                        }
                        let (Some(&low), Some(&high)) =
                            (addresses.iter().min(), addresses.iter().max())
                        else {
                            continue;
                        };
                        input.hints.push(Hint {
                            low,
                            high,
                            kind,
                            addresses,
                        });
                    }
                }
                _ => return Err(format!("release object has load command {cmd:#x}")),
            }
            at += size;
        }
        for (i, s) in input.sections.iter().enumerate() {
            if s.named(b"__TEXT", b"__text") {
                input.text = i;
            } else if s.named(b"__LD", b"__compact_unwind") {
                input.compact = Some(i);
            } else if s.named(b"__TEXT", b"__eh_frame") && s.kind() == S_COALESCED {
                input.unwind = Some(i);
            } else if s.flags & S_ATTR_DEBUG != 0 {
                // DWARF: a part's rows come from `restrict`.
            } else if s.text()
                || !(s.kind() == S_REGULAR || s.kind() == S_ZEROFILL || s.literal().is_some())
                || s.literal().is_some() && !s.relocations.is_empty()
            {
                return Err(format!(
                    "release object section {} cannot be cut per part",
                    s.name()
                ));
            }
        }
        if input.text == usize::MAX {
            return Err("release object has no __TEXT,__text".into());
        }
        let Some(symtab) = symtab else {
            return Err("release object has no symbol table".into());
        };
        let (symoff, nsyms) = (
            word(data, symtab + 8)? as usize,
            word(data, symtab + 12)? as usize,
        );
        let (stroff, strsize) = (
            word(data, symtab + 16)? as usize,
            word(data, symtab + 20)? as usize,
        );
        let strings = data
            .get(stroff..stroff + strsize)
            .ok_or("truncated Mach-O strings")?;
        for i in 0..nsyms {
            let raw = data
                .get(symoff + i * 16..symoff + i * 16 + 16)
                .ok_or("truncated Mach-O symbols")?;
            let rest = strings
                .get(word(raw, 0)? as usize..)
                .ok_or("Mach-O symbol name out of range")?;
            let name = &rest[..rest
                .iter()
                .position(|&b| b == 0)
                .ok_or("unterminated Mach-O name")?];
            let (kind, sect) = (raw[4], raw[5]);
            let desc = u16::from_le_bytes([raw[6], raw[7]]);
            let value = quad(raw, 8)?;
            if kind & N_STAB != 0 {
                return Err("release object has debugger symbols".into());
            }
            let section = match kind & N_TYPE {
                N_SECT => Some(usize::from(sect) - 1),
                N_UNDF if value == 0 => None,
                _ => return Err("release object has an absolute, common or indirect symbol".into()),
            };
            let value = match section {
                Some(s) => {
                    value
                        - input
                            .sections
                            .get(s)
                            .ok_or("Mach-O symbol section out of range")?
                            .addr
                }
                None => value,
            };
            let symbol = Symbol {
                name,
                kind,
                section,
                desc,
                value,
            };
            if let Some(s) = section {
                input.sections[s].symbols.push((value, i));
                if desc & N_ALT_ENTRY == 0 && !symbol.section_start() {
                    input.sections[s].starts.push(value);
                }
                if s == input.text && input.names.insert(name, i).is_some() {
                    return Err("release object defines a text symbol twice".into());
                }
            }
            input.symbols.push(symbol);
        }
        for s in &mut input.sections {
            s.starts.sort_unstable();
            s.starts.dedup();
            s.symbols.sort_unstable();
            s.relocations.sort_by_key(|r| r.address);
        }
        input.hints.sort_by_key(|h| h.low);
        input.compact_entries()?;
        input.unwind_records()?;
        Ok(input)
    }

    /// Each compact unwind entry's function: a section-relative address in
    /// the text, or a text symbol plus an addend.
    fn compact_entries(&mut self) -> Result<(), String> {
        let Some(source) = self.compact else {
            return Ok(());
        };
        let s = &self.sections[source];
        let text = &self.sections[self.text];
        if s.data.len() % COMPACT_ENTRY != 0 {
            return Err("compact unwind section is not whole entries".into());
        }
        for entry in (0..s.data.len()).step_by(COMPACT_ENTRY) {
            let address = match s.relocations_in(entry as u64, (entry + COMPACT_ENTRY) as u64) {
                [r] if r.address as usize == entry
                    && r.kind == ARM64_RELOC_UNSIGNED
                    && r.length == 3
                    && !r.external
                    && r.symbol as usize == self.text + 1 =>
                {
                    quad(s.data, entry)? - text.addr
                }
                [r] if r.address as usize == entry
                    && r.kind == ARM64_RELOC_UNSIGNED
                    && r.external =>
                {
                    let y = &self.symbols[r.symbol as usize];
                    match y.section {
                        Some(section) if section == self.text => y.value + quad(s.data, entry)?,
                        _ => return Err("compact unwind entry names no function".into()),
                    }
                }
                _ => {
                    return Err("compact unwind entry with a personality or LSDA is not cut".into());
                }
            };
            self.entries.push((address, entry));
        }
        self.entries.sort_unstable();
        Ok(())
    }

    /// Each FDE's function, its CIE and its pc-begin, which must be
    /// `SUBTRACTOR(a symbol of this section) + UNSIGNED(function)`.
    fn unwind_records(&mut self) -> Result<(), String> {
        let Some(source) = self.unwind else {
            return Ok(());
        };
        let s = &self.sections[source];
        let mut offset = 0;
        while offset < s.data.len() {
            let length = word(s.data, offset)? as usize;
            if length == 0 {
                break;
            }
            if length == 0xffff_ffff {
                return Err("64-bit unwind records are not cut".into());
            }
            let record = offset;
            offset += 4 + length;
            let id = word(s.data, record + 4)? as usize;
            let inside = s.relocations_in(record as u64, offset as u64);
            if id == 0 {
                if !inside.is_empty() {
                    return Err("unwind CIE with relocations is not cut".into());
                }
                cie_header(s.data.get(record..offset).ok_or("truncated unwind CIE")?)?;
                continue;
            }
            let [sub, function] = inside else {
                return Err("unwind record with an LSDA or no code address is not cut".into());
            };
            let field = record + 8;
            if sub.kind != ARM64_RELOC_SUBTRACTOR
                || function.kind != ARM64_RELOC_UNSIGNED
                || sub.address as usize != field
                || function.address as usize != field
                || sub.length != 3
                || !sub.external
                || !function.external
            {
                return Err("unwind record code address is not a section difference".into());
            }
            let subtrahend = &self.symbols[sub.symbol as usize];
            let target = &self.symbols[function.symbol as usize];
            if subtrahend.section != Some(source) {
                return Err("unwind record code address is not relative to its section".into());
            }
            if target.section != Some(self.text) {
                return Err("unwind record covers no function".into());
            }
            let cie = (record + 4)
                .checked_sub(id)
                .ok_or("unwind record with a bad CIE pointer")?;
            self.fdes.push(Fde {
                symbol: function.symbol as usize,
                function: target.value,
                record: (record, offset),
                cie,
                // In place: minus the field's place past the subtrahend.
                past: quad(s.data, field)?.wrapping_add(field as u64 - subtrahend.value),
            });
        }
        self.fdes.sort_by_key(|f| f.function);
        Ok(())
    }

    /// The text atom `name` starts.
    fn definition(&self, name: &str) -> Result<(u64, u64), String> {
        let symbol = self
            .names
            .get(format!("_{name}").as_bytes())
            .map(|&i| &self.symbols[i])
            .ok_or_else(|| format!("release part {name} has no text section"))?;
        let (start, end) = self.sections[self.text].atom(symbol.value);
        if start != symbol.value || symbol.desc & N_ALT_ENTRY != 0 {
            return Err(format!("release part {name} does not own its section"));
        }
        Ok((start, end))
    }
}

/// Cut a Mach-O release object, as [`super::split_native_object`] cuts an ELF one.
pub(super) fn split(
    object: &[u8],
    parts: &[(&str, ContentHash)],
    debug_lines: bool,
) -> Result<Vec<NativeObject>, String> {
    let input = Input::parse(object)?;
    let debug = match debug_lines {
        true => Some(super::super::debug::object_debug(object)?.0),
        false => None,
    };
    // A Mach-O row names its function, which the part defines.
    let ids: HashMap<&str, usize> = debug
        .iter()
        .flat_map(|d| d.sections.iter().enumerate())
        .map(|(i, s)| (s.symbol.as_str(), i))
        .collect();
    let mut claimed = HashSet::new();
    let mut out = Vec::with_capacity(parts.len());
    for &(name, _) in parts {
        let atom = input.definition(name)?;
        if !claimed.insert(atom.0) {
            return Err(format!("release part {name} is cut twice"));
        }
        let mut part = Part::new(&input, atom);
        part.cut()?;
        let debug = match &debug {
            None => None,
            Some(debug) => {
                let id = ids
                    .get(name)
                    .ok_or("release part has no debug code section")?;
                Some(restrict(debug, *id as u32, name.to_owned())?)
            }
        };
        out.push(NativeObject {
            bytes: part.write()?,
            debug,
            coverage: None,
        });
    }
    let text = &input.sections[input.text];
    if let Some(&lost) = text
        .starts
        .iter()
        .find(|&s| text.atom(*s).1 > *s && !claimed.contains(s))
    {
        return Err(format!(
            "release object text at {lost:#x} belongs to no part"
        ));
    }
    Ok(out)
}

struct OutSection {
    header: Vec<u8>,
    align: u32,
    data: Vec<u8>,
    relocations: Vec<Reloc>,
    /// A non-extern relocation's in-place address: its offset here, the out
    /// section it names and the offset there.
    addresses: Vec<(usize, usize, u64)>,
}

struct OutSymbol {
    name: Vec<u8>,
    kind: u8,
    section: Option<usize>,
    desc: u16,
    value: u64,
}

struct Part<'i, 'a> {
    input: &'i Input<'a>,
    atom: (u64, u64),
    sections: Vec<OutSection>,
    /// Source section -> out section.
    placed: HashMap<usize, usize>,
    /// Source atom (section, start) or literal piece -> its offset out.
    atoms: HashMap<(usize, u64), u64>,
    symbols: Vec<OutSymbol>,
    mapped: HashMap<usize, usize>,
    undefined: HashMap<&'a [u8], usize>,
    queue: VecDeque<(usize, u64, u64)>,
    pool: usize,
    hints: Vec<(u64, Vec<u64>)>,
}

impl<'i, 'a> Part<'i, 'a> {
    fn new(input: &'i Input<'a>, atom: (u64, u64)) -> Self {
        Part {
            input,
            atom,
            sections: Vec::new(),
            placed: HashMap::new(),
            atoms: HashMap::new(),
            symbols: Vec::new(),
            mapped: HashMap::new(),
            undefined: HashMap::new(),
            queue: VecDeque::new(),
            pool: 0,
            hints: Vec::new(),
        }
    }
}

/// The type and description of a part's copy of a symbol: an external
/// constant becomes a weak private-extern definition, so the link keeps one
/// copy and one address; text, locals and weak constants keep theirs.
fn copied(y: &Symbol, text: bool) -> (u8, u16) {
    let kind = y.kind & !N_TYPE;
    if text || !y.external() || y.desc & N_WEAK_DEF != 0 {
        (kind, y.desc)
    } else {
        (kind | N_PEXT, y.desc | N_WEAK_DEF)
    }
}

/// The end of a Mach-O CIE's fixed fields: version 1, augmentation "zR"
/// (no personality, no LSDA), code and data alignment, return register,
/// augmentation length and the FDE address encoding, which must be
/// DW_EH_PE_pcrel with 8-byte absolute pointers (0x10).
fn cie_header(cie: &[u8]) -> Result<usize, String> {
    if cie.get(8) != Some(&1) || cie.get(9..12) != Some(&b"zR\0"[..]) {
        return Err("unwind CIE is not version 1 with augmentation zR".into());
    }
    let (_, n) = uleb(cie, 12)?;
    let mut at = 12 + n;
    let (_, n) = uleb(cie, at)?;
    at += n + 1;
    let (length, n) = uleb(cie, at)?;
    at += n;
    if length != 1 || cie.get(at) != Some(&0x10) {
        return Err("unwind CIE address encoding is not pcrel absptr".into());
    }
    Ok(at + 1)
}
mod part;
mod write;
