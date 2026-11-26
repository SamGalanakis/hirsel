//! The native platform host packed into the release compiler's executable.
use object::{Object as _, ObjectSection as _, read::ReadCache};
use std::io;
use std::path::PathBuf;

pub(super) fn stage() -> io::Result<Option<(tempfile::TempDir, PathBuf)>> {
    let image = ReadCache::new(hirsel_core_ids::launch::running_image()?);
    let image = object::File::parse(&image).map_err(io::Error::other)?;
    let section = match image.format() {
        object::BinaryFormat::Elf => ".hirsel_host",
        object::BinaryFormat::MachO => "__hirsel_host",
        _ => return Ok(None),
    };
    let Some(section) = image.section_by_name(section) else {
        return Ok(None);
    };
    let directory = tempfile::Builder::new()
        .prefix("hirsel-platform-")
        .tempdir()?;
    let program = directory.path().join("platform_main");
    crate::executable::write(&program, section.data().map_err(io::Error::other)?, 0o500)?;
    Ok(Some((directory, program)))
}
