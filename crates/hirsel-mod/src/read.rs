//! The reader: [`Module::parse`] and the checked views it gives a loader.

use crate::{
    Backend, ENTRY, Entry, FIXUP, Fixup, HEADER, Header, IMPORT, Import, Kind, MAGIC,
    NO_DESCRIPTOR, POLICY, ROWS, SECTIONS, SPAN, Span,
};
use hirsel_core_ids::ContentHash;
use hirsel_core_layout::Target;

/// Why bytes are not a well-formed version 1 module.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Malformed {
    Magic,
    Header,
    Sections,
    Imports,
    TextFixups,
    DataFixups,
    Entries,
    Spans,
    Names,
}

/// A checked module over its file bytes.
#[derive(Clone, Copy, Debug)]
pub struct Module<'a> {
    pub header: Header,
    sections: [&'a [u8]; SECTIONS],
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

fn hash_at(bytes: &[u8], at: usize) -> ContentHash {
    ContentHash(bytes[at..at + 32].try_into().unwrap())
}

impl<'a> Module<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Malformed> {
        if bytes.len() < HEADER {
            return Err(Malformed::Header);
        }
        if bytes[..8] != MAGIC {
            return Err(Malformed::Magic);
        }
        let target = *Target::ALL
            .get(u32_at(bytes, 8) as usize)
            .ok_or(Malformed::Header)?;
        let backend = match bytes[12] {
            0 => Backend::Cranelift,
            1 => Backend::Llvm,
            _ => return Err(Malformed::Header),
        };
        let policy = u64::from_le_bytes(bytes[16..24].try_into().unwrap());
        let (text_align, data_align) = (u32_at(bytes, 120), u32_at(bytes, 124));
        let aligned = |a: u32| a.is_power_of_two() && a >= 64;
        if bytes[13..16] != [0; 3] || policy & !POLICY != 0 || !aligned(text_align) {
            return Err(Malformed::Header);
        }
        if !aligned(data_align) {
            return Err(Malformed::Header);
        }
        let header = Header {
            target,
            backend,
            policy,
            abi: hash_at(bytes, 24),
            compiler: hash_at(bytes, 56),
            coreir: hash_at(bytes, 88),
            text_align,
            data_align,
        };

        let mut sections = [&bytes[..0]; SECTIONS];
        let mut next = HEADER;
        for (i, section) in sections.iter_mut().enumerate() {
            let offset = u32_at(bytes, 128 + 8 * i) as usize;
            let length = u32_at(bytes, 132 + 8 * i) as usize;
            let end = offset.checked_add(length).ok_or(Malformed::Sections)?;
            let padded = end.checked_next_multiple_of(8).ok_or(Malformed::Sections)?;
            if offset != next || padded > bytes.len() || (i >= 10 && length != 0) {
                return Err(Malformed::Sections);
            }
            if bytes[end..padded].iter().any(|b| *b != 0) {
                return Err(Malformed::Sections);
            }
            *section = &bytes[offset..end];
            next = padded;
        }
        if next != bytes.len() {
            return Err(Malformed::Sections);
        }
        let module = Self { header, sections };
        module.check()?;
        Ok(module)
    }

    fn check(&self) -> Result<(), Malformed> {
        let (text, data) = (self.text().len() as u64, self.data().len() as u64);
        let arch = self.header.target.arch();

        let imports = self.sections[2];
        if imports.len() % IMPORT != 0 || (imports.len() / IMPORT * 8) as u64 > data {
            return Err(Malformed::Imports);
        }
        let count = imports.len() / IMPORT;
        if self.data()[..8 * count].iter().any(|b| *b != 0) {
            return Err(Malformed::Imports);
        }
        for record in imports.chunks_exact(IMPORT) {
            let (kind, row) = (u32_at(record, 0), u32_at(record, 4));
            let instance = &record[8..40];
            let good = match kind {
                0 => (row as usize) < ROWS.len() && instance.iter().all(|b| *b == 0),
                1 => row == 0,
                _ => false,
            };
            if !good {
                return Err(Malformed::Imports);
            }
        }

        for (table, in_text, error) in [
            (self.sections[3], true, Malformed::TextFixups),
            (self.sections[4], false, Malformed::DataFixups),
        ] {
            if table.len() % FIXUP != 0 {
                return Err(error);
            }
            let section = if in_text { text } else { data };
            let mut free = if in_text { 0 } else { 8 * count as u64 };
            for record in table.chunks_exact(FIXUP) {
                let kind = Kind::of(record[4]).ok_or(error)?;
                let (site, bias) = (u64::from(u32_at(record, 0)), record[5]);
                let addend = i64::from_le_bytes(record[8..16].try_into().unwrap());
                let limit = match kind {
                    Kind::Abs64Text => text,
                    _ => data,
                };
                let placed = match kind {
                    Kind::Abs64Text | Kind::Abs64Data => !in_text && bias == 0,
                    Kind::Rel32Data => in_text && kind.text(arch) && (4..=8).contains(&bias),
                    _ => in_text && kind.text(arch) && bias == 0 && site % 4 == 0,
                };
                let end = site + u64::from(kind.width()).max(u64::from(bias));
                if !placed
                    || record[6..8] != [0; 2]
                    || site < free
                    || end > section
                    || addend < 0
                    || addend as u64 > limit
                {
                    return Err(error);
                }
                free = end;
            }
        }

        let spans = self.sections[6];
        if spans.len() % SPAN != 0 {
            return Err(Malformed::Spans);
        }
        let mut free = 0;
        for span in self.spans() {
            let end = u64::from(span.start) + u64::from(span.length);
            if u64::from(span.start) < free || span.length == 0 || end > text {
                return Err(Malformed::Spans);
            }
            free = end;
        }

        let entries = self.sections[5];
        if entries.len() % ENTRY != 0 {
            return Err(Malformed::Entries);
        }
        let mut last = None;
        let starts: Vec<u32> = self.spans().map(|span| span.start).collect();
        for record in entries.chunks_exact(ENTRY) {
            let entry = entry(record);
            let starts = starts.binary_search(&entry.text).is_ok();
            let described = entry.descriptor == NO_DESCRIPTOR
                || (8 * count as u64..data).contains(&u64::from(entry.descriptor));
            if last >= Some(entry.identity)
                || !starts
                || !described
                || u32_at(record, 72) > 1
                || u32_at(record, 76) != 0
            {
                return Err(Malformed::Entries);
            }
            last = Some(entry.identity);
        }

        let names = self.sections[9];
        let spans = spans.len() / SPAN;
        if names.len() < 4 * spans {
            return Err(Malformed::Names);
        }
        let bytes = &names[4 * spans..];
        let mut from = 0;
        for i in 0..spans {
            let to = u32_at(names, 4 * i) as usize;
            if to < from || to > bytes.len() || std::str::from_utf8(&bytes[from..to]).is_err() {
                return Err(Malformed::Names);
            }
            from = to;
        }
        if from != bytes.len() {
            return Err(Malformed::Names);
        }
        Ok(())
    }

    pub fn text(&self) -> &'a [u8] {
        self.sections[0]
    }
    /// The data image, import slots first.
    pub fn data(&self) -> &'a [u8] {
        self.sections[1]
    }
    pub fn imports(&self) -> impl ExactSizeIterator<Item = Import> + 'a {
        self.sections[2].chunks_exact(IMPORT).map(|record| {
            if u32_at(record, 0) == 0 {
                Import::Row(u32_at(record, 4))
            } else {
                Import::Instance(hash_at(record, 8))
            }
        })
    }
    pub fn text_fixups(&self) -> impl ExactSizeIterator<Item = Fixup> + 'a {
        self.sections[3].chunks_exact(FIXUP).map(fixup)
    }
    pub fn data_fixups(&self) -> impl ExactSizeIterator<Item = Fixup> + 'a {
        self.sections[4].chunks_exact(FIXUP).map(fixup)
    }
    pub fn entries(&self) -> impl ExactSizeIterator<Item = Entry> + 'a {
        self.sections[5].chunks_exact(ENTRY).map(entry)
    }
    /// The entry with this identity, by binary search.
    pub fn entry(&self, identity: &ContentHash) -> Option<Entry> {
        let table = self.sections[5];
        let (mut low, mut high) = (0, table.len() / ENTRY);
        while low < high {
            let middle = (low + high) / 2;
            let record = &table[ENTRY * middle..ENTRY * (middle + 1)];
            match record[..32].cmp(&identity.0[..]) {
                std::cmp::Ordering::Less => low = middle + 1,
                std::cmp::Ordering::Greater => high = middle,
                std::cmp::Ordering::Equal => return Some(entry(record)),
            }
        }
        None
    }
    pub fn spans(&self) -> impl ExactSizeIterator<Item = Span> + 'a {
        self.sections[6].chunks_exact(SPAN).map(|record| Span {
            start: u32_at(record, 0),
            length: u32_at(record, 4),
        })
    }
    pub fn cfi(&self) -> &'a [u8] {
        self.sections[7]
    }
    pub fn sites(&self) -> &'a [u8] {
        self.sections[8]
    }
    /// The name of span `index`.
    pub fn name(&self, index: usize) -> &'a str {
        let names = self.sections[9];
        let spans = self.sections[6].len() / SPAN;
        let from = match index {
            0 => 0,
            _ => u32_at(names, 4 * (index - 1)) as usize,
        };
        let to = u32_at(names, 4 * index) as usize;
        std::str::from_utf8(&names[4 * spans..][from..to]).expect("checked by parse")
    }
}

fn fixup(record: &[u8]) -> Fixup {
    Fixup {
        site: u32_at(record, 0),
        kind: Kind::of(record[4]).expect("checked by parse"),
        bias: record[5],
        addend: i64::from_le_bytes(record[8..16].try_into().unwrap()),
    }
}

fn entry(record: &[u8]) -> Entry {
    Entry {
        identity: hash_at(record, 0),
        signature: hash_at(record, 32),
        text: u32_at(record, 64),
        descriptor: u32_at(record, 68),
        waits: u32_at(record, 72) == 1,
    }
}
