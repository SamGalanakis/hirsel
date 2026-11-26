//! How an installation's compiler finds its shipped libraries on Linux: the ELF
//! loader's RPATH, which the installed copy points at the prefix.
use std::{
    io,
    path::{Path, PathBuf},
};

/// The installed compiler's bytes: `bytes` with its RPATH rewritten to the
/// prefix's `lib/` beside it or above it.
pub(crate) fn installed_executable(mut bytes: Vec<u8>, source: &Path) -> io::Result<Vec<u8>> {
    let rpath = dynamic_strings(&bytes)?
        .into_iter()
        .find(|entry| entry.tag == object::elf::DT_RPATH)
        .ok_or_else(|| {
            io::Error::other(format!(
                "the compiler {} carries no RPATH for its installed libraries",
                source.display()
            ))
        })?;
    const ORIGIN_LIB: &[u8] = b"$ORIGIN/lib:$ORIGIN/../lib";
    if rpath.value.len() < ORIGIN_LIB.len() {
        return Err(io::Error::other(format!(
            "the compiler {}'s RPATH is too short for the installed library paths",
            source.display()
        )));
    }
    let field = &mut bytes[rpath.offset..rpath.offset + rpath.value.len()];
    field.fill(0);
    field[..ORIGIN_LIB.len()].copy_from_slice(ORIGIN_LIB);
    Ok(bytes)
}

/// The directories a shared object's RUNPATH or RPATH names, with `$ORIGIN`
/// as `origin`, and the names of the libraries it needs.
pub(crate) fn loaded_libraries(
    bytes: &[u8],
    origin: &Path,
) -> io::Result<(Vec<PathBuf>, Vec<String>)> {
    let entries = dynamic_strings(bytes)?;
    let search = entries
        .iter()
        .filter(|entry| [object::elf::DT_RUNPATH, object::elf::DT_RPATH].contains(&entry.tag))
        .flat_map(|entry| {
            String::from_utf8_lossy(&entry.value)
                .split(':')
                .map(|dir| PathBuf::from(dir.replace("$ORIGIN", &origin.to_string_lossy())))
                .collect::<Vec<_>>()
        })
        .collect();
    let needed = entries
        .iter()
        .filter(|entry| entry.tag == object::elf::DT_NEEDED)
        .map(|entry| String::from_utf8_lossy(&entry.value).into_owned())
        .collect();
    Ok((search, needed))
}

/// One string-valued entry of an ELF object's dynamic section.
struct DynamicString {
    tag: object::elf::DynamicTag,
    /// The string's file offset.
    offset: usize,
    value: Vec<u8>,
}

/// The NEEDED, RPATH and RUNPATH strings of a 64-bit little-endian ELF.
fn dynamic_strings(bytes: &[u8]) -> io::Result<Vec<DynamicString>> {
    use object::read::elf::{Dyn as _, FileHeader as _, SectionHeader as _};
    let malformed = |error: object::Error| io::Error::other(format!("malformed ELF: {error}"));
    let header =
        object::elf::FileHeader64::<object::LittleEndian>::parse(bytes).map_err(malformed)?;
    let endian = header.endian().map_err(malformed)?;
    let sections = header.sections(endian, bytes).map_err(malformed)?;
    let mut strings = Vec::new();
    for section in sections.iter() {
        let Some((entries, link)) = section.dynamic(endian, bytes).map_err(malformed)? else {
            continue;
        };
        let table = sections.section(link).map_err(malformed)?;
        let base = table.sh_offset(endian) as usize;
        let data = table.data(endian, bytes).map_err(malformed)?;
        for entry in entries {
            let tag = entry.d_tag(endian);
            if ![
                object::elf::DT_NEEDED,
                object::elf::DT_RPATH,
                object::elf::DT_RUNPATH,
            ]
            .contains(&tag)
            {
                continue;
            }
            let start = entry.d_val(endian) as usize;
            let value = data
                .get(start..)
                .and_then(|rest| rest.split(|b| *b == 0).next())
                .ok_or_else(|| io::Error::other("malformed ELF: dynamic string out of range"))?;
            strings.push(DynamicString {
                tag,
                offset: base + start,
                value: value.to_vec(),
            });
        }
    }
    Ok(strings)
}
