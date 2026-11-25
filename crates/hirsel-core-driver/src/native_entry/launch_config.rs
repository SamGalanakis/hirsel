//! The launch word shared by native startup and the offline CLI check.
use super::*;
use hirsel_core_layout::ObjectFormat;
use object::{Object as _, ObjectSection as _, read::ReadCache};

const ELF_SECTION: &str = ".hirsel_launch";
const MACHO_SECTION: &str = "__hirsel_launch";
const PLACE: [u8; 8] = 1u64.to_le_bytes();

pub(super) fn emit(object: &mut ObjectModule, format: ObjectFormat, place: bool) -> Result<DataId> {
    let word = object.declare_anonymous_data(false, false).map_err(error)?;
    let mut data = DataDescription::new();
    data.define(Box::new(if place { PLACE } else { [0; 8] }));
    data.set_align(8);
    data.set_custom_section(match format {
        ObjectFormat::Elf => ELF_SECTION,
        ObjectFormat::MachO => "__TEXT,__hirsel_launch",
    });
    object.define_data(word, &data).map_err(error)?;
    Ok(word)
}

pub(crate) fn is_place(executable: &std::fs::File) -> bool {
    let cache = ReadCache::new(executable);
    let Ok(image) = object::File::parse(&cache) else {
        return false;
    };
    let section = match image.format() {
        object::BinaryFormat::Elf => ELF_SECTION,
        object::BinaryFormat::MachO => MACHO_SECTION,
        _ => return false,
    };
    image.section_by_name(section).is_some_and(|section| {
        section.size() == PLACE.len() as u64 && section.data().is_ok_and(|bytes| bytes == PLACE)
    })
}
