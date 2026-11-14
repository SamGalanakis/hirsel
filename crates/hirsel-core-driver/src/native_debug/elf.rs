//! ELF sections and relocations for the single link-time debug object.
use gimli::LittleEndian;
use gimli::write::{self, EndianVec, RelocateWriter, Relocation, RelocationTarget};
use std::collections::BTreeMap;
pub(super) fn error(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[derive(Clone)]
pub(super) struct Section {
    bytes: EndianVec<LittleEndian>,
    relocations: Vec<Relocation>,
}
impl Default for Section {
    fn default() -> Self {
        Self {
            bytes: EndianVec::new(LittleEndian),
            relocations: vec![],
        }
    }
}
impl RelocateWriter for Section {
    type Writer = EndianVec<LittleEndian>;
    fn writer(&self) -> &Self::Writer {
        &self.bytes
    }
    fn writer_mut(&mut self) -> &mut Self::Writer {
        &mut self.bytes
    }
    fn relocate(&mut self, relocation: Relocation) {
        self.relocations.push(relocation);
    }
}

pub(super) fn write_sections(
    output: &mut object::write::Object<'_>,
    sections: &write::Sections<Section>,
    symbols: &[object::write::SymbolId],
) -> Result<(), String> {
    let mut ids = BTreeMap::new();
    sections.for_each(|id, data| -> Result<(), String> {
        if !data.bytes.slice().is_empty() {
            let section = output.add_section(
                vec![],
                id.name().as_bytes().to_vec(),
                object::SectionKind::Debug,
            );
            output
                .section_mut(section)
                .set_data(data.bytes.slice().to_vec(), 1);
            ids.insert(id, section);
        }
        Ok(())
    })?;
    sections.for_each(|id, data| -> Result<(), String> {
        for relocation in &data.relocations {
            let symbol = match relocation.target {
                RelocationTarget::Symbol(id) => symbols[id],
                RelocationTarget::Section(id) => output.section_symbol(ids[&id]),
            };
            output
                .add_relocation(
                    ids[&id],
                    object::write::Relocation {
                        offset: relocation.offset as u64,
                        symbol,
                        addend: relocation.addend,
                        flags: object::RelocationFlags::Generic {
                            kind: object::RelocationKind::Absolute,
                            encoding: object::RelocationEncoding::Generic,
                            size: relocation.size * 8,
                        },
                    },
                )
                .map_err(error)?;
        }
        Ok(())
    })?;
    let stack = output.add_section(
        vec![],
        b".note.GNU-stack".to_vec(),
        object::SectionKind::Other,
    );
    output.section_mut(stack).set_data(vec![], 1);
    Ok(())
}
