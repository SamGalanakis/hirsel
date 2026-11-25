//! Writing a cut part as a Mach-O relocatable object.
use super::*;

impl Part<'_, '_> {
    /// Put zero-filled sections after those with file bytes, as the
    /// segment's file size counts only the latter.
    fn zerofill_last(&mut self) {
        let mut order: Vec<usize> = (0..self.sections.len()).collect();
        order.sort_by_key(|&i| zerofill(word(&self.sections[i].header, 64).unwrap_or(0)));
        let mut rank = vec![0; order.len()];
        for (at, &i) in order.iter().enumerate() {
            rank[i] = at;
        }
        let mut sections: Vec<_> = std::mem::take(&mut self.sections)
            .into_iter()
            .map(Some)
            .collect();
        self.sections = order.iter().map(|&i| sections[i].take().unwrap()).collect();
        for s in &mut self.sections {
            s.addresses.iter_mut().for_each(|a| a.1 = rank[a.1]);
        }
        for y in &mut self.symbols {
            y.section = y.section.map(|s| rank[s]);
        }
    }

    /// The part as a Mach-O relocatable object: header, one segment, the
    /// build version and hints, the symbol tables; then section contents,
    /// relocations, hints, symbols, and the string table last.
    pub(super) fn write(mut self) -> Result<Vec<u8>, String> {
        self.zerofill_last();
        // Locals in creation order, then definitions, then references, each
        // by name, as LC_DYSYMTAB groups them.
        let group = |y: &OutSymbol| match (y.kind & N_EXT != 0, y.section.is_some()) {
            (false, _) => 0,
            (true, true) => 1,
            (true, false) => 2,
        };
        let mut order: Vec<usize> = (0..self.symbols.len()).collect();
        order.sort_by(|&a, &b| {
            let (x, y) = (&self.symbols[a], &self.symbols[b]);
            group(x).cmp(&group(y)).then_with(|| match group(x) {
                0 => a.cmp(&b),
                _ => x.name.cmp(&y.name),
            })
        });
        let mut position = vec![0u32; self.symbols.len()];
        for (at, &i) in order.iter().enumerate() {
            position[i] = at as u32;
        }
        let counts = [0, 1, 2].map(|g| {
            order
                .iter()
                .filter(|&&i| group(&self.symbols[i]) == g)
                .count() as u32
        });
        let hints = {
            let mut bytes = Vec::new();
            let mut push = |mut v: u64| loop {
                let b = (v & 0x7f) as u8;
                v >>= 7;
                bytes.push(if v == 0 { b } else { b | 0x80 });
                if v == 0 {
                    break;
                }
            };
            for (kind, addresses) in &self.hints {
                push(*kind);
                push(addresses.len() as u64);
                addresses.iter().for_each(|&a| push(a));
            }
            bytes.resize(bytes.len().next_multiple_of(8), 0);
            bytes
        };
        let build = self.input.build.unwrap_or(&[]);
        let segment = 72 + 80 * self.sections.len();
        let commands = segment + build.len() + if hints.is_empty() { 0 } else { 16 } + 24 + 80;
        let start = HEADER + commands;
        // Section addresses: each aligned, contents back to back.
        let mut addresses = Vec::with_capacity(self.sections.len());
        let mut size = 0u64;
        for s in &self.sections {
            size = size.next_multiple_of(1 << s.align);
            addresses.push(size);
            size += s.data.len() as u64;
        }
        let mut file = vec![0u8; start];
        let mut relocations = Vec::new();
        let mut headers = Vec::new();
        for (i, s) in self.sections.iter().enumerate() {
            let empty = zerofill(word(&s.header, 64)?);
            if !empty {
                file.resize(start + addresses[i] as usize, 0);
                let mut data = s.data.clone();
                for &(at, section, offset) in &s.addresses {
                    data[at..at + 8].copy_from_slice(&(addresses[section] + offset).to_le_bytes());
                }
                file.extend_from_slice(&data);
            }
            let mut header = s.header[..32].to_vec();
            header.extend_from_slice(&addresses[i].to_le_bytes());
            header.extend_from_slice(&(s.data.len() as u64).to_le_bytes());
            let offset = if empty {
                0
            } else {
                (start as u64 + addresses[i]) as u32
            };
            header.extend_from_slice(&offset.to_le_bytes());
            header.extend_from_slice(&s.align.to_le_bytes());
            relocations.push(
                s.relocations
                    .iter()
                    .map(|r| {
                        let symbol = match (r.kind, r.external) {
                            (ARM64_RELOC_ADDEND, _) => r.symbol,
                            (_, true) => position[r.symbol as usize],
                            // The ordinal of the out section a non-extern names.
                            (_, false) => {
                                let named = s
                                    .addresses
                                    .iter()
                                    .find(|a| a.0 == r.address as usize)
                                    .map_or(0, |a| a.1);
                                named as u32 + 1
                            }
                        };
                        let info = symbol
                            | u32::from(r.pcrel) << 24
                            | u32::from(r.length) << 25
                            | u32::from(r.external) << 27
                            | u32::from(r.kind) << 28;
                        (r.address, info)
                    })
                    .collect::<Vec<_>>(),
            );
            headers.push(header);
        }
        let filesize = file.len() - start;
        file.resize(file.len().next_multiple_of(8), 0);
        for (header, (s, list)) in headers
            .iter_mut()
            .zip(self.sections.iter().zip(&relocations))
        {
            let at = if list.is_empty() {
                0
            } else {
                file.len() as u32
            };
            for &(address, info) in list {
                file.extend_from_slice(&address.to_le_bytes());
                file.extend_from_slice(&info.to_le_bytes());
            }
            header.extend_from_slice(&at.to_le_bytes());
            header.extend_from_slice(&(list.len() as u32).to_le_bytes());
            header.extend_from_slice(&s.header[64..80]);
        }
        let hints_at = file.len();
        file.extend_from_slice(&hints);
        let symoff = file.len();
        let mut strings = vec![0u8];
        for &i in &order {
            let y = &self.symbols[i];
            let name = strings.len() as u32;
            strings.extend_from_slice(&y.name);
            strings.push(0);
            file.extend_from_slice(&name.to_le_bytes());
            file.push(y.kind | if y.section.is_some() { N_SECT } else { N_UNDF });
            file.push(y.section.map_or(0, |s| s as u8 + 1));
            file.extend_from_slice(&y.desc.to_le_bytes());
            let value = y.section.map_or(y.value, |s| addresses[s] + y.value);
            file.extend_from_slice(&value.to_le_bytes());
        }
        strings.resize(strings.len().next_multiple_of(8), 0);
        let stroff = file.len();
        file.extend_from_slice(&strings);
        // Header and load commands.
        let mut c = Vec::with_capacity(commands);
        let w = |c: &mut Vec<u8>, v: u32| c.extend_from_slice(&v.to_le_bytes());
        w(&mut c, LC_SEGMENT_64);
        w(&mut c, segment as u32);
        c.extend_from_slice(&[0; 16]);
        c.extend_from_slice(&0u64.to_le_bytes());
        c.extend_from_slice(&size.to_le_bytes());
        c.extend_from_slice(&(start as u64).to_le_bytes());
        c.extend_from_slice(&(filesize as u64).to_le_bytes());
        w(&mut c, 7);
        w(&mut c, 7);
        w(&mut c, self.sections.len() as u32);
        w(&mut c, 0);
        headers.iter().for_each(|h| c.extend_from_slice(h));
        c.extend_from_slice(build);
        if !hints.is_empty() {
            for v in [
                LC_LINKER_OPTIMIZATION_HINT,
                16,
                hints_at as u32,
                hints.len() as u32,
            ] {
                w(&mut c, v);
            }
        }
        for v in [
            LC_SYMTAB,
            24,
            symoff as u32,
            order.len() as u32,
            stroff as u32,
            strings.len() as u32,
        ] {
            w(&mut c, v);
        }
        for v in [
            LC_DYSYMTAB,
            80,
            0,
            counts[0],
            counts[0],
            counts[1],
            counts[0] + counts[1],
            counts[2],
        ] {
            w(&mut c, v);
        }
        c.extend_from_slice(&[0; 48]);
        debug_assert_eq!(c.len(), commands);
        file[HEADER..start].copy_from_slice(&c);
        file[..16].copy_from_slice(&self.input.header[..16]);
        let ncmds = 3 + u32::from(!build.is_empty()) + u32::from(!hints.is_empty());
        file[16..20].copy_from_slice(&ncmds.to_le_bytes());
        file[20..24].copy_from_slice(&(commands as u32).to_le_bytes());
        file[24..32].copy_from_slice(&self.input.header[24..32]);
        Ok(file)
    }
}
