//! Cutting one part: its text atom, the constants and literals it reaches,
//! its unwind records and linker hints.
use super::*;

impl<'a> Part<'_, 'a> {
    pub(super) fn cut(&mut self) -> Result<(), String> {
        let text = self.input.text;
        self.copy(text, self.atom.0)?;
        let input = self.input;
        for &symbol in input.owned.get(&self.atom.0).into_iter().flatten() {
            let y = &input.symbols[symbol];
            self.copy(y.section.expect("an owned symbol is defined"), y.value)?;
        }
        while let Some((source, start, end)) = self.queue.pop_front() {
            let out = self.placed[&source];
            let base = self.atoms[&(source, start)];
            let input = self.input;
            for &r in input.sections[source].relocations_in(start, end) {
                let mut r = r;
                r.address = (u64::from(r.address) - start + base) as u32;
                if matches!(
                    r.kind,
                    ARM64_RELOC_TLVP_LOAD_PAGE21 | ARM64_RELOC_TLVP_LOAD_PAGEOFF12
                ) {
                    return Err("release part reaches thread-local data".into());
                }
                if r.kind != ARM64_RELOC_ADDEND {
                    if !r.external {
                        return Err(format!(
                            "release part addresses section {} by address",
                            r.symbol
                        ));
                    }
                    r.symbol = self.target(r.symbol as usize)? as u32;
                }
                self.sections[out].relocations.push(r);
            }
        }
        let (start, end) = self.atom;
        let text = &self.input.sections[text];
        let (low, high) = (text.addr + start, text.addr + end);
        // A hint belongs to the part holding its least address; one that
        // reaches past it spans parts.
        let hints = &self.input.hints;
        let first = hints.partition_point(|h| h.low < low);
        for h in hints[first..].iter().take_while(|h| h.low < high) {
            if h.high >= high {
                return Err("linker hint spans release parts".into());
            }
            self.hints
                .push((h.kind, h.addresses.iter().map(|a| a - low).collect()));
        }
        self.compact_unwind()?;
        self.unwind()
    }

    /// The out section of source `source`, made on first use.
    fn section(&mut self, source: usize) -> usize {
        if let Some(&out) = self.placed.get(&source) {
            return out;
        }
        let s = &self.input.sections[source];
        self.sections.push(OutSection {
            header: s.header.to_vec(),
            align: 0,
            data: Vec::new(),
            relocations: Vec::new(),
            addresses: Vec::new(),
        });
        self.placed.insert(source, self.sections.len() - 1);
        self.sections.len() - 1
    }

    /// Append `bytes` to out section `out` at alignment `align` (log2).
    fn append(&mut self, out: usize, align: u32, bytes: &[u8]) -> u64 {
        let section = &mut self.sections[out];
        section.align = section.align.max(align);
        let at = section.data.len().next_multiple_of(1 << align);
        section.data.resize(at, 0);
        section.data.extend_from_slice(bytes);
        at as u64
    }

    /// Copy the atom of `source` that starts at `start` with its symbols;
    /// its relocations follow from the queue.
    fn copy(&mut self, source: usize, start: u64) -> Result<u64, String> {
        if let Some(&at) = self.atoms.get(&(source, start)) {
            return Ok(at);
        }
        let s = &self.input.sections[source];
        let (_, end) = s.atom(start);
        if s.text() && (source != self.input.text || start != self.atom.0) {
            return Err("release part reaches into another definition's atom".into());
        }
        if s.literal().is_some() || s.flags & S_ATTR_DEBUG != 0 || Some(source) == self.input.unwind
        {
            return Err(format!("release part references {} whole", s.name()));
        }
        // A split module aligns every definition to 16 (`macho_part_layout`
        // in parts.cpp), so an atom runs to its size rounded up to 16 and
        // the padding a neighbour leaves is LLVM's own: nops in code.
        if s.align > ATOM_ALIGN || start % (1 << ATOM_ALIGN) != 0 {
            return Err(format!(
                "release part's {} atom at {start:#x} is not on its own 16-byte boundary (section aligned 2^{})",
                s.name(),
                s.align
            ));
        }
        let out = self.section(source);
        let mut bytes = match s.data {
            [] => vec![0; (end - start) as usize],
            data => data[start as usize..end as usize].to_vec(),
        };
        let padded = bytes.len().next_multiple_of(1 << ATOM_ALIGN);
        if s.text() {
            let nops = NOP.iter().cycle().skip(bytes.len() % 4);
            bytes.extend(nops.take(padded - bytes.len()));
        } else {
            bytes.resize(padded, 0);
        }
        let at = self.append(out, ATOM_ALIGN, &bytes);
        self.atoms.insert((source, start), at);
        self.queue.push_back((source, start, end));
        let text = s.text();
        let input = self.input;
        let symbols = &input.sections[source].symbols;
        let first = symbols.partition_point(|&(value, _)| value < start);
        for &(_, i) in symbols[first..]
            .iter()
            .take_while(|&&(value, _)| value < end)
        {
            let y = &input.symbols[i];
            if y.section_start() {
                continue;
            }
            let (kind, desc) = copied(y, text, input.owned_by(i, self.atom.0));
            self.mapped.insert(i, self.symbols.len());
            self.symbols.push(OutSymbol {
                name: y.name.to_vec(),
                kind,
                section: Some(out),
                desc,
                value: at + y.value - start,
            });
        }
        Ok(at)
    }

    fn undefined(&mut self, symbol: usize) -> usize {
        let y = &self.input.symbols[symbol];
        if let Some(&index) = self.undefined.get(y.name) {
            return index;
        }
        let index = self.symbols.len();
        self.symbols.push(OutSymbol {
            name: y.name.to_vec(),
            kind: N_UNDF | N_EXT,
            section: None,
            // A reference keeps its weak-reference bit only.
            desc: if y.section.is_none() { y.desc } else { 0 },
            value: 0,
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
            Some(_) if y.section_start() => {
                return Err("release part addresses a section by its start".into());
            }
            Some(section) if self.input.sections[section].literal().is_some() => {
                self.piece(section, symbol)?
            }
            Some(section) if self.input.sections[section].text() => {
                if !y.external() {
                    return Err(format!(
                        "release part references local {} of another definition",
                        String::from_utf8_lossy(y.name)
                    ));
                }
                self.undefined(symbol)
            }
            Some(section) => {
                let (start, _) = self.input.sections[section].atom(y.value);
                self.copy(section, start)?;
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

    /// One literal: a fixed-size entry or a C string with its terminator.
    fn piece(&mut self, section: usize, symbol: usize) -> Result<usize, String> {
        let s = &self.input.sections[section];
        let y = &self.input.symbols[symbol];
        let size = s.literal().unwrap();
        let start = y.value as usize;
        let len = match size {
            0 => s
                .data
                .get(start..)
                .and_then(|d| d.iter().position(|&b| b == 0))
                .map(|n| n + 1),
            n => Some(n as usize).filter(|_| start % n as usize == 0),
        };
        let Some(bytes) = len.and_then(|len| s.data.get(start..start + len)) else {
            return Err(format!(
                "release part cannot take {} out of {}",
                String::from_utf8_lossy(y.name),
                s.name()
            ));
        };
        let align = match size {
            0 => 0,
            n => n.trailing_zeros(),
        };
        let out = self.section(section);
        let at = match self.atoms.get(&(section, y.value)) {
            Some(&at) => at,
            None => {
                let at = self.append(out, align, bytes);
                self.atoms.insert((section, y.value), at);
                at
            }
        };
        let (kind, desc) = copied(y, false, None);
        let name = if y.external() {
            y.name.to_vec()
        } else {
            // Pool labels are numbered per program; a part's are its own.
            self.pool += 1;
            format!("lCPI0_{}", self.pool - 1).into_bytes()
        };
        self.symbols.push(OutSymbol {
            name,
            kind,
            section: Some(out),
            desc,
            value: at,
        });
        Ok(self.symbols.len() - 1)
    }

    /// The part's compact unwind entries: each whose function lies in its
    /// atom, its address rewritten to the part's text.
    fn compact_unwind(&mut self) -> Result<(), String> {
        let Some(source) = self.input.compact else {
            return Ok(());
        };
        let input = self.input;
        let s = &input.sections[source];
        let (start, end) = self.atom;
        let first = input
            .entries
            .partition_point(|&(address, _)| address < start);
        let entries = &input.entries[first..];
        let entries = &entries[..entries.partition_point(|&(address, _)| address < end)];
        if entries.is_empty() {
            return Ok(());
        }
        let out = self.section(source);
        for &(address, entry) in entries {
            let mut bytes = s.data[entry..entry + COMPACT_ENTRY].to_vec();
            bytes[..8].fill(0);
            let at = self.append(out, 3, &bytes);
            self.sections[out].relocations.push(Reloc {
                address: at as u32,
                symbol: 0,
                pcrel: false,
                length: 3,
                external: false,
                kind: ARM64_RELOC_UNSIGNED,
            });
            self.sections[out]
                .addresses
                .push((at as usize, 0, address - start));
        }
        self.sections[out].align = s.align;
        Ok(())
    }

    /// The part's DWARF unwind records: each FDE of its atom with its CIE,
    /// its pc-begin made relative to the part's own section start.
    fn unwind(&mut self) -> Result<(), String> {
        let Some(source) = self.input.unwind else {
            return Ok(());
        };
        let input = self.input;
        let s = &input.sections[source];
        let (start, end) = self.atom;
        let first = input.fdes.partition_point(|f| f.function < start);
        let fdes = input.fdes[first..].iter().take_while(|f| f.function < end);
        let mut data = Vec::new();
        let mut places = Vec::new();
        let mut cies: HashMap<usize, usize> = HashMap::new();
        for f in fdes {
            let cie = match cies.get(&f.cie) {
                Some(&c) => c,
                None => {
                    let len = word(s.data, f.cie)? as usize + 4;
                    let bytes = s
                        .data
                        .get(f.cie..f.cie + len)
                        .ok_or("truncated unwind CIE")?;
                    let c = super::super::repad(&mut data, bytes, cie_header(bytes)?);
                    cies.insert(f.cie, c);
                    c
                }
            };
            let bytes = &s.data[f.record.0..f.record.1];
            // Header, address and range (8 bytes each, as `cie_header`
            // requires), then the augmentation data its length counts.
            let (augmentation, n) = uleb(bytes, 24)?;
            let fde = super::super::repad(&mut data, bytes, 24 + n + augmentation as usize);
            let pointer = (fde + 4 - cie) as u32;
            data[fde + 4..fde + 8].copy_from_slice(&pointer.to_le_bytes());
            let place = (fde + 8) as u64;
            data[fde + 8..fde + 16].copy_from_slice(&f.past.wrapping_sub(place).to_le_bytes());
            places.push((place, f.symbol));
        }
        if places.is_empty() {
            return Ok(());
        }
        let out = self.section(source);
        self.sections[out].align = s.align;
        self.sections[out].data = data;
        let begin = self.symbols.len();
        self.symbols.push(OutSymbol {
            name: format!("ltmp{out}").into_bytes(),
            kind: N_SECT,
            section: Some(out),
            desc: 0,
            value: 0,
        });
        for (place, target) in places {
            let target = self.target(target)?;
            for (symbol, kind) in [
                (begin, ARM64_RELOC_SUBTRACTOR),
                (target, ARM64_RELOC_UNSIGNED),
            ] {
                self.sections[out].relocations.push(Reloc {
                    address: place as u32,
                    symbol: symbol as u32,
                    pcrel: false,
                    length: 3,
                    external: true,
                    kind,
                });
            }
        }
        Ok(())
    }
}
