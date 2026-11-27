//! The writer: [`Image::encode`].

use crate::{ENTRY, FIXUP, Fixup, HEADER, IMPORT, Image, Import, MAGIC, SECTIONS, SPAN};

impl Image {
    /// The file bytes. Tables are written in canonical order: fixups by site, entries
    /// by identity, spans by start (names follow their spans).
    pub fn encode(mut self) -> Vec<u8> {
        self.text_fixups.sort_by_key(|f| f.site);
        self.data_fixups.sort_by_key(|f| f.site);
        self.entries.sort_by_key(|e| e.identity);
        let mut order: Vec<usize> = (0..self.spans.len()).collect();
        order.sort_by_key(|&i| self.spans[i].start);

        let mut data = vec![0; 8 * self.imports.len()];
        data.extend_from_slice(&self.data);
        let mut imports = Vec::with_capacity(IMPORT * self.imports.len());
        for import in &self.imports {
            let (kind, row, instance) = match import {
                Import::Row(row) => (0u32, *row, [0; 32]),
                Import::Instance(instance) => (1, 0, instance.0),
            };
            imports.extend_from_slice(&kind.to_le_bytes());
            imports.extend_from_slice(&row.to_le_bytes());
            imports.extend_from_slice(&instance);
        }
        let mut entries = Vec::with_capacity(ENTRY * self.entries.len());
        for entry in &self.entries {
            entries.extend_from_slice(&entry.identity.0);
            entries.extend_from_slice(&entry.signature.0);
            entries.extend_from_slice(&entry.text.to_le_bytes());
            entries.extend_from_slice(&entry.descriptor.to_le_bytes());
            entries.extend_from_slice(&u32::from(entry.waits).to_le_bytes());
            entries.extend_from_slice(&[0; 4]);
        }
        let mut spans = Vec::with_capacity(SPAN * self.spans.len());
        let mut ends = Vec::new();
        let mut names = Vec::new();
        for &i in &order {
            spans.extend_from_slice(&self.spans[i].start.to_le_bytes());
            spans.extend_from_slice(&self.spans[i].length.to_le_bytes());
            if let Some(name) = self.names.get(i) {
                names.extend_from_slice(name.as_bytes());
            }
            ends.extend_from_slice(&(names.len() as u32).to_le_bytes());
        }
        ends.extend_from_slice(&names);

        let sections: [&[u8]; SECTIONS] = [
            &self.text,
            &data,
            &imports,
            &fixups(&self.text_fixups),
            &fixups(&self.data_fixups),
            &entries,
            &spans,
            &self.cfi,
            &self.sites,
            &ends,
            &[],
            &[],
            &[],
        ];
        let header = &self.header;
        let mut out = Vec::new();
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&(header.target.index() as u32).to_le_bytes());
        out.extend_from_slice(&[header.backend as u8, 0, 0, 0]);
        out.extend_from_slice(&header.policy.to_le_bytes());
        out.extend_from_slice(&header.abi.0);
        out.extend_from_slice(&header.compiler.0);
        out.extend_from_slice(&header.coreir.0);
        out.extend_from_slice(&header.text_align.to_le_bytes());
        out.extend_from_slice(&header.data_align.to_le_bytes());
        let mut offset = HEADER;
        for section in sections {
            out.extend_from_slice(&(offset as u32).to_le_bytes());
            out.extend_from_slice(&(section.len() as u32).to_le_bytes());
            offset += section.len().next_multiple_of(8);
        }
        for section in sections {
            out.extend_from_slice(section);
            out.resize(out.len().next_multiple_of(8), 0);
        }
        out
    }
}

fn fixups(list: &[Fixup]) -> Vec<u8> {
    let mut out = Vec::with_capacity(FIXUP * list.len());
    for fixup in list {
        out.extend_from_slice(&fixup.site.to_le_bytes());
        out.extend_from_slice(&[fixup.kind as u8, fixup.bias, 0, 0]);
        out.extend_from_slice(&fixup.addend.to_le_bytes());
    }
    out
}
