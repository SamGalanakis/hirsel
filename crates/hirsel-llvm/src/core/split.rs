//! Cut a release program's object per definition. The program is generated
//! once, with a section per function and per constant; a part is one
//! function's text section and what only it needs from the rest:
//! - its relocations, with every other function an undefined symbol;
//! - the sections it references by section symbol (its jump tables);
//! - its constant-pool entries and the merged constants it names, copied out
//!   of their shared mergeable sections;
//! - every constant it reaches, transitively, as a weak hidden definition,
//!   so the link keeps one copy and one address per program;
//! - its unwind records (the CIE and each FDE of its text);
//! - with debug lines, its own rows of the program's debug data.
//!
//! A part is therefore a function of its definition, as its key (the part's
//! bitcode) is: equal keys give equal parts, whichever other definitions were
//! generated beside it. Anything outside that shape stops the cut; the engine
//! keys a module it cannot cut as one whole part instead.
use hirsel_core_ids::ContentHash;
use hirsel_core_ir::source::{CodeSection, NativeDebug, NativeObject, ScopeKind, debug_symbol};
use object::LittleEndian as LE;
use object::elf;
use object::read::elf::{FileHeader as _, Rela as _, SectionHeader as _, Sym as _};
use std::collections::{HashMap, VecDeque};

type Header = elf::FileHeader64<LE>;

/// The ELF constants as the raw numbers the file holds.
mod raw {
    use object::elf as e;
    pub const ET_REL: u16 = e::ET_REL.0;
    pub const SHF_ALLOC: u64 = e::SHF_ALLOC.0;
    pub const SHF_EXECINSTR: u64 = e::SHF_EXECINSTR.0;
    pub const SHF_GROUP: u64 = e::SHF_GROUP.0;
    pub const SHF_INFO_LINK: u64 = e::SHF_INFO_LINK.0;
    pub const SHF_MERGE: u64 = e::SHF_MERGE.0;
    pub const SHF_TLS: u64 = e::SHF_TLS.0;
    pub const SHN_ABS: u16 = e::SHN_ABS.0;
    pub const SHT_NOBITS: u32 = e::SHT_NOBITS.0;
    pub const SHT_NULL: u32 = e::SHT_NULL.0;
    pub const SHT_PROGBITS: u32 = e::SHT_PROGBITS.0;
    pub const SHT_RELA: u32 = e::SHT_RELA.0;
    pub const SHT_STRTAB: u32 = e::SHT_STRTAB.0;
    pub const SHT_SYMTAB: u32 = e::SHT_SYMTAB.0;
    pub const SHT_X86_64_UNWIND: u32 = e::SHT_X86_64_UNWIND.0;
    pub const STB_GLOBAL: u8 = e::STB_GLOBAL.0;
    pub const STB_LOCAL: u8 = e::STB_LOCAL.0;
    pub const STB_WEAK: u8 = e::STB_WEAK.0;
    pub const STT_FILE: u8 = e::STT_FILE.0;
    pub const STT_FUNC: u8 = e::STT_FUNC.0;
    pub const STT_NOTYPE: u8 = e::STT_NOTYPE.0;
    pub const STT_OBJECT: u8 = e::STT_OBJECT.0;
    pub const STT_SECTION: u8 = e::STT_SECTION.0;
    pub const STV_HIDDEN: u8 = e::STV_HIDDEN.0;
}

fn error(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// Cut `object`, the one object generated for a program's unreused parts, into
/// one finished object per part, in `parts` order. Each part is its
/// definition's symbol and its key; the empty symbol is a whole module, its
/// object not cut.
pub fn split_native_object(
    object: &[u8],
    parts: &[(&str, ContentHash)],
    debug_lines: bool,
) -> Result<Vec<NativeObject>, String> {
    if let [("", key)] = parts {
        return Ok(vec![super::finish_native_object(
            object.to_vec(),
            *key,
            debug_lines,
        )?]);
    }
    let input = Input::parse(object)?;
    let debug = if debug_lines {
        Some(super::debug::object_debug(object)?)
    } else {
        None
    };
    let mut claimed = vec![false; input.sections.len()];
    let mut out = Vec::with_capacity(parts.len());
    for &(name, key) in parts {
        let text = input.definition(name)?;
        if std::mem::replace(&mut claimed[text], true) {
            return Err(format!("release part {name} is cut twice"));
        }
        let mut part = Part::new(&input, text);
        part.cut()?;
        let debug = match &debug {
            None => None,
            Some((debug, indices)) => {
                part.symbols.push(Symbol {
                    name: debug_symbol(key, 0).into_bytes(),
                    bind: raw::STB_GLOBAL,
                    kind: raw::STT_NOTYPE,
                    other: raw::STV_HIDDEN,
                    section: Some(0),
                    value: 0,
                    size: 0,
                });
                Some(restrict(debug, indices, text, key)?)
            }
        };
        out.push(NativeObject {
            bytes: part.write(),
            debug,
            coverage: None,
        });
    }
    if let Some(lost) = (0..input.sections.len())
        .find(|&i| input.sections[i].text && input.sections[i].size > 0 && !claimed[i])
    {
        return Err(format!(
            "release object section {} belongs to no part",
            String::from_utf8_lossy(input.sections[lost].name)
        ));
    }
    Ok(out)
}

struct Section<'a> {
    name: &'a [u8],
    sh_type: u32,
    flags: u64,
    align: u64,
    entsize: u64,
    size: u64,
    data: &'a [u8],
    text: bool,
    /// Its relocations: offset, symbol, type, addend.
    relocations: Vec<(u64, usize, u32, i64)>,
    /// Its symbols other than its section symbol.
    symbols: Vec<usize>,
}
impl Section<'_> {
    fn merge(&self) -> bool {
        self.flags & u64::from(raw::SHF_MERGE) != 0
    }
}

struct InputSymbol<'a> {
    name: &'a [u8],
    bind: u8,
    kind: u8,
    other: u8,
    /// None for an undefined symbol.
    section: Option<usize>,
    value: u64,
    size: u64,
}

/// The binding and visibility of a part's copy of a symbol: a global constant becomes a weak
/// hidden definition; text, locals and weak (shared, carried by every part) constants keep theirs.
fn copied(y: &InputSymbol, text: bool) -> (u8, u8) {
    if text || y.bind != raw::STB_GLOBAL {
        (y.bind, y.other)
    } else {
        (raw::STB_WEAK, (y.other & !3) | raw::STV_HIDDEN)
    }
}

struct Input<'a> {
    machine: u16,
    flags: u32,
    sections: Vec<Section<'a>>,
    symbols: Vec<InputSymbol<'a>>,
    functions: HashMap<&'a [u8], usize>,
    unwind: Option<usize>,
    note: Option<usize>,
}

impl<'a> Input<'a> {
    fn parse(data: &'a [u8]) -> Result<Self, String> {
        let header = Header::parse(data).map_err(error)?;
        let table = header.sections(LE, data).map_err(error)?;
        let mut sections = Vec::with_capacity(table.len());
        let (mut unwind, mut note) = (None, None);
        for (i, sh) in table.iter().enumerate() {
            let name = table.section_name(LE, sh).map_err(error)?;
            let sh_type = sh.sh_type(LE).0;
            let flags = sh.sh_flags(LE).0;
            let alloc = flags & u64::from(raw::SHF_ALLOC) != 0;
            let text = alloc && flags & u64::from(raw::SHF_EXECINSTR) != 0;
            let supported = match sh_type {
                raw::SHT_NULL | raw::SHT_SYMTAB | raw::SHT_STRTAB | raw::SHT_RELA => true,
                raw::SHT_X86_64_UNWIND => name == b".eh_frame",
                raw::SHT_PROGBITS if name == b".note.GNU-stack" => true,
                raw::SHT_PROGBITS | raw::SHT_NOBITS if !alloc => name.starts_with(b".debug"),
                raw::SHT_PROGBITS | raw::SHT_NOBITS => {
                    flags & u64::from(raw::SHF_TLS | raw::SHF_GROUP) == 0
                }
                _ => false,
            };
            if !supported {
                return Err(format!(
                    "release object section {} cannot be cut per part",
                    String::from_utf8_lossy(name)
                ));
            }
            if name == b".eh_frame" {
                unwind = Some(i);
            } else if name == b".note.GNU-stack" {
                note = Some(i);
            }
            sections.push(Section {
                name,
                sh_type,
                flags,
                align: sh.sh_addralign(LE).max(1),
                entsize: sh.sh_entsize(LE),
                size: sh.sh_size(LE),
                data: if sh_type == raw::SHT_NOBITS {
                    &[]
                } else {
                    sh.data(LE, data).map_err(error)?
                },
                text,
                relocations: Vec::new(),
                symbols: Vec::new(),
            });
        }
        let table_symbols = table.symbols(LE, data, elf::SHT_SYMTAB).map_err(error)?;
        let mut symbols = Vec::with_capacity(table_symbols.len());
        let mut functions = HashMap::new();
        for (i, sym) in table_symbols.iter().enumerate() {
            let shndx = sym.st_shndx(LE).0;
            if shndx == raw::SHN_ABS && sym.st_type().0 == raw::STT_FILE || i == 0 {
                symbols.push(InputSymbol {
                    name: b"",
                    bind: 0,
                    kind: raw::STT_FILE,
                    other: 0,
                    section: None,
                    value: 0,
                    size: 0,
                });
                continue;
            }
            let section = table_symbols
                .symbol_section(LE, sym, object::SymbolIndex(i))
                .map_err(error)?
                .map(|s| s.0);
            if section.is_none() && !sym.is_undefined(LE) {
                return Err("release object has an absolute or common symbol".into());
            }
            let symbol = InputSymbol {
                name: table_symbols.symbol_name(LE, sym).map_err(error)?,
                bind: sym.st_bind().0,
                kind: sym.st_type().0,
                other: sym.st_other().0,
                section,
                value: sym.st_value(LE),
                size: sym.st_size(LE),
            };
            if let Some(s) = section
                && symbol.kind != raw::STT_SECTION
            {
                sections[s].symbols.push(i);
                if sections[s].text && symbol.kind == raw::STT_FUNC {
                    functions.insert(symbol.name, i);
                }
            }
            symbols.push(symbol);
        }
        for sh in table.iter() {
            if let Some((relas, _)) = sh.rela(LE, data).map_err(error)? {
                let target = sh.sh_info(LE) as usize;
                let list = &mut sections
                    .get_mut(target)
                    .ok_or("relocation section without a target")?
                    .relocations;
                list.extend(relas.iter().map(|r| {
                    (
                        r.r_offset(LE),
                        r.r_sym(LE, false) as usize,
                        r.r_type(LE, false).0,
                        r.r_addend(LE),
                    )
                }));
                list.sort_by_key(|r| r.0);
            }
        }
        Ok(Input {
            machine: header.e_machine(LE).0,
            flags: header.e_flags(LE).0,
            sections,
            symbols,
            functions,
            unwind,
            note,
        })
    }

    /// The text section that defines `name`, which starts there.
    fn definition(&self, name: &str) -> Result<usize, String> {
        let &symbol = self
            .functions
            .get(name.as_bytes())
            .ok_or_else(|| format!("release part {name} has no text section"))?;
        let symbol = &self.symbols[symbol];
        match symbol.section {
            Some(section) if symbol.value == 0 => Ok(section),
            _ => Err(format!("release part {name} does not own its section")),
        }
    }
}

struct OutSection {
    name: Vec<u8>,
    sh_type: u32,
    flags: u64,
    align: u64,
    entsize: u64,
    size: u64,
    data: Vec<u8>,
    /// Offset, out symbol, type, addend.
    relocations: Vec<(u64, usize, u32, i64)>,
    symbol: Option<usize>,
}

struct Symbol {
    name: Vec<u8>,
    bind: u8,
    kind: u8,
    other: u8,
    /// Out section index; None for an undefined symbol.
    section: Option<usize>,
    value: u64,
    size: u64,
}

struct Part<'i, 'a> {
    input: &'i Input<'a>,
    text: usize,
    sections: Vec<OutSection>,
    /// Source section copied whole -> out section.
    copied: HashMap<usize, usize>,
    /// Source mergeable section -> out section and its pieces by source offset.
    merged: HashMap<usize, (usize, HashMap<u64, u64>)>,
    symbols: Vec<Symbol>,
    mapped: HashMap<usize, usize>,
    undefined: HashMap<&'a [u8], usize>,
    queue: VecDeque<usize>,
    pool: usize,
}

impl<'i, 'a> Part<'i, 'a> {
    fn new(input: &'i Input<'a>, text: usize) -> Self {
        Part {
            input,
            text,
            sections: Vec::new(),
            copied: HashMap::new(),
            merged: HashMap::new(),
            symbols: Vec::new(),
            mapped: HashMap::new(),
            undefined: HashMap::new(),
            queue: VecDeque::new(),
            pool: 0,
        }
    }

    fn cut(&mut self) -> Result<(), String> {
        self.copy(self.text)?;
        while let Some(source) = self.queue.pop_front() {
            let out = self.copied[&source];
            for &(offset, symbol, kind, addend) in &self.input.sections[source].relocations {
                let target = self.target(symbol)?;
                self.sections[out]
                    .relocations
                    .push((offset, target, kind, addend));
            }
        }
        self.unwind()?;
        if let Some(note) = self.input.note {
            let s = &self.input.sections[note];
            self.sections.push(OutSection {
                name: s.name.to_vec(),
                sh_type: s.sh_type,
                flags: s.flags,
                align: 1,
                entsize: 0,
                size: 0,
                data: Vec::new(),
                relocations: Vec::new(),
                symbol: None,
            });
        }
        Ok(())
    }

    /// Copy a section whole with its symbols; its relocations follow from the
    /// queue. Its symbols bind as `copied` says.
    fn copy(&mut self, source: usize) -> Result<usize, String> {
        if let Some(&out) = self.copied.get(&source) {
            return Ok(out);
        }
        let s = &self.input.sections[source];
        if s.text && source != self.text {
            return Err(format!(
                "release part reaches into another definition's section {}",
                String::from_utf8_lossy(s.name)
            ));
        }
        if s.merge() || s.flags & u64::from(raw::SHF_ALLOC) == 0 {
            return Err(format!(
                "release part references section {} whole",
                String::from_utf8_lossy(s.name)
            ));
        }
        let out = self.sections.len();
        self.sections.push(OutSection {
            name: s.name.to_vec(),
            sh_type: s.sh_type,
            flags: s.flags,
            align: s.align,
            entsize: s.entsize,
            size: s.size,
            data: s.data.to_vec(),
            relocations: Vec::new(),
            symbol: None,
        });
        self.copied.insert(source, out);
        self.queue.push_back(source);
        for &i in &s.symbols {
            let y = &self.input.symbols[i];
            let (bind, other) = copied(y, s.text);
            let index = self.symbols.len();
            self.symbols.push(Symbol {
                name: y.name.to_vec(),
                bind,
                kind: y.kind,
                other,
                section: Some(out),
                value: y.value,
                size: y.size,
            });
            self.mapped.insert(i, index);
        }
        Ok(out)
    }

    fn section_symbol(&mut self, out: usize) -> usize {
        if let Some(index) = self.sections[out].symbol {
            return index;
        }
        let index = self.symbols.len();
        self.symbols.push(Symbol {
            name: Vec::new(),
            bind: raw::STB_LOCAL,
            kind: raw::STT_SECTION,
            other: 0,
            section: Some(out),
            value: 0,
            size: 0,
        });
        self.sections[out].symbol = Some(index);
        index
    }

    fn undefined(&mut self, symbol: usize) -> usize {
        let y = &self.input.symbols[symbol];
        if let Some(&index) = self.undefined.get(y.name) {
            return index;
        }
        let index = self.symbols.len();
        self.symbols.push(Symbol {
            name: y.name.to_vec(),
            bind: if y.bind == raw::STB_WEAK {
                raw::STB_WEAK
            } else {
                raw::STB_GLOBAL
            },
            kind: raw::STT_NOTYPE,
            other: y.other,
            section: None,
            value: 0,
            size: 0,
        });
        self.undefined.insert(y.name, index);
        index
    }

    /// The out symbol a relocation against source `symbol` uses.
    fn target(&mut self, symbol: usize) -> Result<usize, String> {
        if let Some(&index) = self.mapped.get(&symbol) {
            return Ok(index);
        }
        let y = &self.input.symbols[symbol];
        let index = match y.section {
            None => self.undefined(symbol),
            Some(section) if y.kind == raw::STT_SECTION => {
                if self.input.sections[section].merge() {
                    return Err("release part addresses a mergeable section by offset".into());
                }
                let out = self.copy(section)?;
                return Ok(self.section_symbol(out));
            }
            Some(section) if self.input.sections[section].merge() => self.piece(section, symbol)?,
            Some(section) if self.input.sections[section].text => {
                if y.bind == raw::STB_LOCAL {
                    return Err(format!(
                        "release part references local {} of another definition",
                        String::from_utf8_lossy(y.name)
                    ));
                }
                self.undefined(symbol)
            }
            Some(section) => {
                self.copy(section)?;
                return self
                    .mapped
                    .get(&symbol)
                    .copied()
                    .ok_or_else(|| "release constant symbol was not copied".into());
            }
        };
        self.mapped.insert(symbol, index);
        Ok(index)
    }

    /// One entry of a shared mergeable section: a constant-pool entry (local,
    /// one entry long) or a merged constant (its own size).
    fn piece(&mut self, section: usize, symbol: usize) -> Result<usize, String> {
        let s = &self.input.sections[section];
        let y = &self.input.symbols[symbol];
        let len = if y.size > 0 { y.size } else { s.entsize };
        if s.entsize == 0
            || len % s.entsize != 0
            || y.value % s.entsize != 0
            || y.value + len > s.data.len() as u64
            || !s.relocations.is_empty()
            || !(y.bind == raw::STB_LOCAL && y.kind == raw::STT_NOTYPE || y.kind == raw::STT_OBJECT)
        {
            return Err(format!(
                "release part cannot take {} out of {}",
                String::from_utf8_lossy(y.name),
                String::from_utf8_lossy(s.name)
            ));
        }
        let out = match self.merged.get(&section) {
            Some(&(out, _)) => out,
            None => {
                let out = self.sections.len();
                self.sections.push(OutSection {
                    name: s.name.to_vec(),
                    sh_type: s.sh_type,
                    flags: s.flags,
                    align: s.align,
                    entsize: s.entsize,
                    size: 0,
                    data: Vec::new(),
                    relocations: Vec::new(),
                    symbol: None,
                });
                self.merged.insert(section, (out, HashMap::new()));
                out
            }
        };
        let pieces = &mut self.merged.get_mut(&section).unwrap().1;
        let data = &mut self.sections[out].data;
        let value = *pieces.entry(y.value).or_insert_with(|| {
            let at = data.len() as u64;
            data.extend_from_slice(&s.data[y.value as usize..(y.value + len) as usize]);
            at
        });
        self.sections[out].size = self.sections[out].data.len() as u64;
        let (bind, other) = copied(y, false);
        let name = if y.bind == raw::STB_LOCAL {
            // Pool labels are numbered per program; a part's are its own.
            self.pool += 1;
            format!(".LCPI0_{}", self.pool - 1).into_bytes()
        } else {
            y.name.to_vec()
        };
        let index = self.symbols.len();
        self.symbols.push(Symbol {
            name,
            bind,
            kind: y.kind,
            other,
            section: Some(out),
            value,
            size: y.size,
        });
        Ok(index)
    }

    /// The part's unwind records: each FDE of its text with its CIE.
    fn unwind(&mut self) -> Result<(), String> {
        let Some(source) = self.input.unwind else {
            return Ok(());
        };
        let s = &self.input.sections[source];
        let at = |offset: usize| -> Result<u32, String> {
            s.data
                .get(offset..offset + 4)
                .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
                .ok_or_else(|| "truncated unwind record".into())
        };
        let mut data = Vec::new();
        let mut relocations = Vec::new();
        let mut cies: HashMap<usize, usize> = HashMap::new();
        let mut offset = 0;
        while offset < s.data.len() {
            let length = at(offset)? as usize;
            if length == 0 {
                break;
            }
            if length == 0xffff_ffff {
                return Err("64-bit unwind records are not cut".into());
            }
            let end = offset + 4 + length;
            let id = at(offset + 4)?;
            let record = offset;
            offset = end;
            if id == 0 {
                continue;
            }
            let Some(&(_, symbol, _, _)) = s.relocations.iter().find(|r| r.0 == record as u64 + 8)
            else {
                return Err("unwind record without a code address".into());
            };
            if self.input.symbols[symbol].section != Some(self.text) {
                continue;
            }
            let cie = (record + 4)
                .checked_sub(id as usize)
                .ok_or("unwind record with a bad CIE pointer")?;
            let cie_out = match cies.get(&cie) {
                Some(&c) => c,
                None => {
                    let len = at(cie)? as usize + 4;
                    if s.relocations
                        .iter()
                        .any(|r| (cie as u64..(cie + len) as u64).contains(&r.0))
                    {
                        return Err("unwind CIE with relocations is not cut".into());
                    }
                    let bytes = s.data.get(cie..cie + len).ok_or("truncated unwind CIE")?;
                    let c = repad(&mut data, bytes, cie_header(bytes)?);
                    cies.insert(cie, c);
                    c
                }
            };
            let bytes = &s.data[record..end];
            // Header, address and range (pcrel sdata4, as `cie_header`
            // requires), then the augmentation data its length counts.
            let (augmentation, n) = uleb(bytes, 16)?;
            let fde = repad(&mut data, bytes, 16 + n + augmentation as usize);
            let pointer = (fde + 4 - cie_out) as u32;
            data[fde + 4..fde + 8].copy_from_slice(&pointer.to_le_bytes());
            for &(at, symbol, kind, addend) in &s.relocations {
                if (record as u64..end as u64).contains(&at) {
                    let target = self.target(symbol)?;
                    relocations.push((at - record as u64 + fde as u64, target, kind, addend));
                }
            }
        }
        if !data.is_empty() {
            self.sections.push(OutSection {
                name: s.name.to_vec(),
                sh_type: s.sh_type,
                flags: s.flags,
                align: s.align,
                entsize: s.entsize,
                size: data.len() as u64,
                data,
                relocations,
                symbol: None,
            });
        }
        Ok(())
    }

    /// The part as a relocatable ELF file: its sections, their relocation
    /// sections, then the symbol, string and section-name tables.
    fn write(self) -> Vec<u8> {
        // Locals first, in creation order, then globals.
        let mut order: Vec<usize> = (0..self.symbols.len())
            .filter(|&i| self.symbols[i].bind == raw::STB_LOCAL)
            .collect();
        let locals = order.len() + 1;
        order.extend((0..self.symbols.len()).filter(|&i| self.symbols[i].bind != raw::STB_LOCAL));
        let mut position = vec![0u32; self.symbols.len()];
        for (at, &i) in order.iter().enumerate() {
            position[i] = at as u32 + 1;
        }
        let mut strtab = vec![0u8];
        let mut shstrtab = vec![0u8];
        let name = |table: &mut Vec<u8>, name: &[u8]| -> u32 {
            if name.is_empty() {
                return 0;
            }
            let at = table.len() as u32;
            table.extend_from_slice(name);
            table.push(0);
            at
        };
        let mut file = vec![0u8; 64];
        let align = |file: &mut Vec<u8>, to: u64| {
            let len = file.len() as u64;
            file.resize(len.next_multiple_of(to.max(1)) as usize, 0);
        };
        let mut headers: Vec<[u64; 10]> = vec![[0; 10]];
        for s in &self.sections {
            align(&mut file, s.align);
            let at = file.len() as u64;
            if s.sh_type != raw::SHT_NOBITS {
                file.extend_from_slice(&s.data);
            }
            let n = u64::from(name(&mut shstrtab, &s.name));
            headers.push([
                n,
                u64::from(s.sh_type),
                s.flags,
                0,
                at,
                s.size,
                0,
                0,
                s.align,
                s.entsize,
            ]);
        }
        let symtab = (headers.len()
            + self
                .sections
                .iter()
                .filter(|s| !s.relocations.is_empty())
                .count()) as u64;
        for (i, s) in self.sections.iter().enumerate() {
            if s.relocations.is_empty() {
                continue;
            }
            align(&mut file, 8);
            let at = file.len() as u64;
            for &(offset, symbol, kind, addend) in &s.relocations {
                file.extend_from_slice(&offset.to_le_bytes());
                let info = (u64::from(position[symbol]) << 32) | u64::from(kind);
                file.extend_from_slice(&info.to_le_bytes());
                file.extend_from_slice(&addend.to_le_bytes());
            }
            let mut rela_name = b".rela".to_vec();
            rela_name.extend_from_slice(&s.name);
            let n = u64::from(name(&mut shstrtab, &rela_name));
            headers.push([
                n,
                u64::from(raw::SHT_RELA),
                u64::from(raw::SHF_INFO_LINK),
                0,
                at,
                file.len() as u64 - at,
                symtab,
                i as u64 + 1,
                8,
                24,
            ]);
        }
        align(&mut file, 8);
        let at = file.len() as u64;
        file.extend_from_slice(&[0; 24]);
        for &i in &order {
            let y = &self.symbols[i];
            file.extend_from_slice(&name(&mut strtab, &y.name).to_le_bytes());
            file.push((y.bind << 4) | y.kind);
            file.push(y.other);
            file.extend_from_slice(&y.section.map_or(0u16, |s| s as u16 + 1).to_le_bytes());
            file.extend_from_slice(&y.value.to_le_bytes());
            file.extend_from_slice(&y.size.to_le_bytes());
        }
        let n = u64::from(name(&mut shstrtab, b".symtab"));
        headers.push([
            n,
            u64::from(raw::SHT_SYMTAB),
            0,
            0,
            at,
            file.len() as u64 - at,
            symtab + 1,
            locals as u64,
            8,
            24,
        ]);
        let at = file.len() as u64;
        file.extend_from_slice(&strtab);
        let n = u64::from(name(&mut shstrtab, b".strtab"));
        headers.push([
            n,
            u64::from(raw::SHT_STRTAB),
            0,
            0,
            at,
            strtab.len() as u64,
            0,
            0,
            1,
            0,
        ]);
        let n = u64::from(name(&mut shstrtab, b".shstrtab"));
        let at = file.len() as u64;
        file.extend_from_slice(&shstrtab);
        headers.push([
            n,
            u64::from(raw::SHT_STRTAB),
            0,
            0,
            at,
            shstrtab.len() as u64,
            0,
            0,
            1,
            0,
        ]);
        align(&mut file, 8);
        let shoff = file.len() as u64;
        for h in &headers {
            file.extend_from_slice(&(h[0] as u32).to_le_bytes());
            file.extend_from_slice(&(h[1] as u32).to_le_bytes());
            for v in &h[2..6] {
                file.extend_from_slice(&v.to_le_bytes());
            }
            file.extend_from_slice(&(h[6] as u32).to_le_bytes());
            file.extend_from_slice(&(h[7] as u32).to_le_bytes());
            for v in &h[8..10] {
                file.extend_from_slice(&v.to_le_bytes());
            }
        }
        let mut header = Vec::with_capacity(64);
        header.extend_from_slice(&[0x7f, b'E', b'L', b'F', 2, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        header.extend_from_slice(&raw::ET_REL.to_le_bytes());
        header.extend_from_slice(&self.input.machine.to_le_bytes());
        header.extend_from_slice(&1u32.to_le_bytes());
        header.extend_from_slice(&0u64.to_le_bytes());
        header.extend_from_slice(&0u64.to_le_bytes());
        header.extend_from_slice(&shoff.to_le_bytes());
        header.extend_from_slice(&self.input.flags.to_le_bytes());
        header.extend_from_slice(&64u16.to_le_bytes());
        header.extend_from_slice(&0u16.to_le_bytes());
        header.extend_from_slice(&0u16.to_le_bytes());
        header.extend_from_slice(&64u16.to_le_bytes());
        header.extend_from_slice(&(headers.len() as u16).to_le_bytes());
        header.extend_from_slice(&(headers.len() as u16 - 1).to_le_bytes());
        file[..64].copy_from_slice(&header);
        file
    }
}

/// Append an unwind record without the padding its place in the program's
/// section gave it, padded again to 8 bytes in the part's own: trailing
/// zero bytes past `floor` (the end of the record's fixed fields) are
/// `DW_CFA_nop`, which is also what padding is, so dropping and re-adding
/// them keeps the record's meaning and makes its bytes its own. Returns
/// where the record starts.
fn repad(data: &mut Vec<u8>, record: &[u8], floor: usize) -> usize {
    let mut end = record.len();
    while end > floor && record[end - 1] == 0 {
        end -= 1;
    }
    let start = data.len();
    data.extend_from_slice(&record[..end]);
    data.resize(data.len().next_multiple_of(8), 0);
    let length = (data.len() - start - 4) as u32;
    data[start..start + 4].copy_from_slice(&length.to_le_bytes());
    start
}
/// The end of a CIE's fixed fields: version, augmentation "zR" (the only
/// form a cut part's functions have: no personality, no LSDA), code and data
/// alignment, return register, augmentation length and the FDE address
/// encoding, which must be pcrel sdata4.
fn cie_header(cie: &[u8]) -> Result<usize, String> {
    if cie.get(8) != Some(&1) || cie.get(9..12) != Some(&b"zR\0"[..]) {
        return Err("unwind CIE is not version 1 with augmentation zR".into());
    }
    let (_, n) = uleb(cie, 12)?;
    let mut at = 12 + n;
    let (_, n) = uleb(cie, at)?; // data alignment (sleb; same length)
    at += n + 1; // return address register
    let (length, n) = uleb(cie, at)?;
    at += n;
    if length != 1 || cie.get(at) != Some(&0x1b) {
        return Err("unwind CIE address encoding is not pcrel sdata4".into());
    }
    Ok(at + 1)
}
fn uleb(bytes: &[u8], at: usize) -> Result<(u64, usize), String> {
    let mut value = 0u64;
    for (i, &b) in bytes.get(at..).unwrap_or(&[]).iter().enumerate().take(10) {
        value |= u64::from(b & 0x7f) << (7 * i);
        if b & 0x80 == 0 {
            return Ok((value, i + 1));
        }
    }
    Err("truncated unwind LEB128".into())
}

/// The debug rows of one code section, as if its part had been generated
/// alone: the section is the part's only one, scopes are its own trees then
/// the abstract scopes they name, and items are numbered by first use. A
/// physical function scope carries its abstract origin's name and
/// declaration, so it does not depend on whether another definition inlined
/// the same function.
fn restrict(
    debug: &NativeDebug,
    indices: &[usize],
    text: usize,
    key: ContentHash,
) -> Result<NativeDebug, String> {
    let id = indices
        .iter()
        .position(|&i| i == text)
        .ok_or("release part has no debug code section")? as u32;
    let mut out = NativeDebug {
        sections: vec![CodeSection {
            symbol: debug_symbol(key, 0),
            size: debug.sections[id as usize].size,
        }],
        ..NativeDebug::default()
    };
    let mut items: HashMap<u32, u32> = HashMap::new();
    let mut item = |location: Option<hirsel_core_ir::source::NativeLocation>,
                    out: &mut NativeDebug| {
        location.map(|mut l| {
            l.item = *items.entry(l.item).or_insert_with(|| {
                out.items.push(debug.items[l.item as usize].clone());
                out.items.len() as u32 - 1
            });
            l
        })
    };
    for sequence in debug.sequences.iter().filter(|s| s.range.section == id) {
        let mut sequence = sequence.clone();
        sequence.range.section = 0;
        for row in &mut sequence.rows {
            row.source = item(row.source, &mut out);
        }
        out.sequences.push(sequence);
    }
    // Own scopes in preorder, each after its parent; then named origins.
    let mut order: Vec<u32> = Vec::new();
    let mut kept: HashMap<u32, u32> = HashMap::new();
    fn visit(
        scopes: &[hirsel_core_ir::source::DebugScope],
        s: u32,
        order: &mut Vec<u32>,
        kept: &mut HashMap<u32, u32>,
    ) {
        if kept.contains_key(&s) {
            return;
        }
        if let Some(parent) = scopes[s as usize].parent {
            visit(scopes, parent, order, kept);
        }
        kept.insert(s, order.len() as u32);
        order.push(s);
    }
    for (s, scope) in debug.scopes.iter().enumerate() {
        if scope.ranges.iter().any(|r| r.section == id) {
            visit(&debug.scopes, s as u32, &mut order, &mut kept);
        }
    }
    let own = order.len();
    let folded = |s: u32| {
        let scope = &debug.scopes[s as usize];
        matches!(scope.kind, ScopeKind::Function { .. })
            && !scope.ranges.is_empty()
            && scope.origin.is_some()
    };
    let mut at = 0;
    while at < order.len() {
        let s = order[at];
        if let Some(origin) = debug.scopes[s as usize].origin
            && !(at < own && folded(s))
        {
            visit(&debug.scopes, origin, &mut order, &mut kept);
        }
        at += 1;
    }
    for (at, &s) in order.iter().enumerate() {
        let mut scope = debug.scopes[s as usize].clone();
        if at < own && folded(s) {
            let origin = &debug.scopes[scope.origin.take().unwrap() as usize];
            if scope.name.is_empty() {
                scope.name = origin.name.clone();
            }
            if scope.declaration.is_none() {
                scope.declaration = origin.declaration;
            }
            if let (ScopeKind::Function { linkage }, ScopeKind::Function { linkage: from }) =
                (&mut scope.kind, &origin.kind)
                && linkage.is_empty()
            {
                *linkage = from.clone();
            }
        }
        scope.parent = scope.parent.map(|p| kept[&p]);
        scope.origin = scope.origin.map(|o| kept[&o]);
        scope.ranges.retain(|r| r.section == id);
        for range in &mut scope.ranges {
            range.section = 0;
        }
        scope.declaration = item(scope.declaration, &mut out);
        scope.call = item(scope.call, &mut out);
        out.scopes.push(scope);
    }
    Ok(out)
}

/// The symbols `object` defines as strong globals, whatever their
/// visibility: the names another object of the same link must not define.
/// Weak definitions (shared constants, revision carriers a link overrides)
/// are left out.
pub fn strong_definitions(object: &[u8]) -> Result<Vec<Vec<u8>>, String> {
    let header = Header::parse(object).map_err(error)?;
    let table = header.sections(LE, object).map_err(error)?;
    let symbols = table.symbols(LE, object, elf::SHT_SYMTAB).map_err(error)?;
    let mut names = Vec::new();
    for symbol in symbols.iter() {
        if symbol.st_bind().0 == raw::STB_GLOBAL
            && !symbol.is_undefined(LE)
            && symbol.st_shndx(LE).0 != raw::SHN_ABS
        {
            names.push(symbols.symbol_name(LE, symbol).map_err(error)?.to_vec());
        }
    }
    Ok(names)
}

/// The symbols `object` names and does not define: what another object of
/// its link defines for it.
pub fn undefined_symbols(object: &[u8]) -> Result<Vec<Vec<u8>>, String> {
    let header = Header::parse(object).map_err(error)?;
    let table = header.sections(LE, object).map_err(error)?;
    let symbols = table.symbols(LE, object, elf::SHT_SYMTAB).map_err(error)?;
    let mut names = Vec::new();
    for symbol in symbols.iter() {
        if symbol.st_bind().0 != raw::STB_LOCAL
            && symbol.is_undefined(LE)
            && symbol.st_name(LE) != 0
        {
            names.push(symbols.symbol_name(LE, symbol).map_err(error)?.to_vec());
        }
    }
    Ok(names)
}

/// Rename a release std object's symbols and omit locals the link never
/// reads. ARM mapping symbols still distinguish instructions from data.
pub fn rewrite_std_symbols(
    object: &[u8],
    rename: &dyn Fn(&[u8]) -> Option<Vec<u8>>,
) -> Result<Vec<u8>, String> {
    use object::build::Id as _;
    use object::build::elf::{Builder, SectionData};
    let mut elf = Builder::read(object).map_err(error)?;
    let used: std::collections::HashSet<_> = elf
        .sections
        .iter()
        .filter_map(|section| match &section.data {
            SectionData::Relocation(relocations) => Some(relocations),
            _ => None,
        })
        .flatten()
        .filter_map(|relocation| relocation.symbol.map(|symbol| symbol.index()))
        .collect();
    let arm = elf.header.e_machine == elf::EM_AARCH64;
    for symbol in &mut elf.symbols {
        let name = symbol.name.as_slice();
        if symbol.st_bind() == elf::STB_LOCAL
            && !used.contains(&symbol.id().index())
            && !(arm && name.starts_with(b"$"))
        {
            symbol.delete = true;
        } else if let Some(name) = rename(name) {
            symbol.name = name.into();
        }
    }
    let mut out = Vec::new();
    elf.write(&mut out).map_err(error)?;
    Ok(out)
}
