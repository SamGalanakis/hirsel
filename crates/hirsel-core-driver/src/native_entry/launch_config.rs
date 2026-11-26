//! The launch word shared by native startup and the offline CLI check.
use super::*;
use hirsel_core_layout::ObjectFormat;
use object::{Object as _, ObjectSection as _, read::ReadCache};

const ELF_SECTION: &str = ".hirsel_launch";
const MACHO_SECTION: &str = "__hirsel_launch";
const PLACE: [u8; 8] = 1u64.to_le_bytes();
const PLACE_WITNESS_BYTES: u64 = 8 + 1 + 32;

pub(super) fn emit(
    object: &mut ObjectModule,
    format: ObjectFormat,
    backend: Option<Backend>,
    revision: &[u8; 32],
) -> Result<DataId> {
    let word = object.declare_anonymous_data(false, false).map_err(error)?;
    let mut data = DataDescription::new();
    let bytes = match backend {
        Some(backend) => {
            let mut bytes = PLACE.to_vec();
            bytes.push(match backend {
                Backend::Cranelift => 0,
                Backend::Llvm => 1,
            });
            bytes.extend_from_slice(revision);
            bytes
        }
        None => vec![0; 8],
    };
    data.define(bytes.into_boxed_slice());
    data.set_align(8);
    data.set_custom_section(match format {
        ObjectFormat::Elf => ELF_SECTION,
        ObjectFormat::MachO => "__TEXT,__hirsel_launch",
    });
    object.define_data(word, &data).map_err(error)?;
    Ok(word)
}

pub(crate) fn is_place(executable: &std::fs::File) -> bool {
    read(executable, |_| ()).is_some()
}

pub(crate) fn witness_identity(executable: &std::fs::File) -> Option<(Backend, [u8; 32])> {
    read(executable, |tail| {
        let (backend, revision) = tail.split_first()?;
        let backend = match backend {
            0 => Backend::Cranelift,
            1 => Backend::Llvm,
            _ => return None,
        };
        let revision: [u8; 32] = revision.try_into().ok()?;
        (revision != [0; 32]).then_some((backend, revision))
    })
    .flatten()
}

fn read<T>(executable: &std::fs::File, decode: impl FnOnce(&[u8]) -> T) -> Option<T> {
    let cache = ReadCache::new(executable);
    let image = object::File::parse(&cache).ok()?;
    let section = match image.format() {
        object::BinaryFormat::Elf => ELF_SECTION,
        object::BinaryFormat::MachO => MACHO_SECTION,
        _ => return None,
    };
    let section = image.section_by_name(section)?;
    if section.size() != PLACE.len() as u64 && section.size() != PLACE_WITNESS_BYTES {
        return None;
    }
    let bytes = section.data().ok()?;
    Some(decode(bytes.strip_prefix(&PLACE)?))
}
