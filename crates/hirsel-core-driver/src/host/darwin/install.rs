//! How an installation's compiler finds its shipped libraries on Darwin. The
//! compiler opens the LLVM engine by path from the prefix's `lib/` itself, so the
//! installed executable is the signed image unchanged; a shipped library's own
//! dependencies are the `@rpath` names in its load commands.
use object::read::macho::{LoadCommandVariant, MachHeader as _};
use std::{
    io,
    path::{Path, PathBuf},
};

/// The installed compiler's bytes: `bytes` unchanged, so its code signature holds.
pub(crate) fn installed_executable(bytes: Vec<u8>, _source: &Path) -> io::Result<Vec<u8>> {
    Ok(bytes)
}

/// The directories a dylib's LC_RPATH commands name, with `@loader_path` as
/// `origin`, and the names of the dylibs it loads through `@rpath`. System dylibs,
/// named by absolute path, are not shipped; nor is the dylib's own LC_ID_DYLIB.
pub(crate) fn loaded_libraries(
    bytes: &[u8],
    origin: &Path,
) -> io::Result<(Vec<PathBuf>, Vec<String>)> {
    let malformed = |error: object::Error| io::Error::other(format!("malformed Mach-O: {error}"));
    let header =
        object::macho::MachHeader64::<object::LittleEndian>::parse(bytes, 0).map_err(malformed)?;
    let endian = header.endian().map_err(malformed)?;
    let mut commands = header.load_commands(endian, bytes, 0).map_err(malformed)?;
    let (mut search, mut needed) = (Vec::new(), Vec::new());
    let origin = origin.to_string_lossy();
    while let Some(command) = commands.next().map_err(malformed)? {
        match command.variant().map_err(malformed)? {
            LoadCommandVariant::Rpath(rpath) => {
                let path = command.string(endian, rpath.path).map_err(malformed)?;
                let path = String::from_utf8_lossy(path).replace("@loader_path", &origin);
                search.push(PathBuf::from(path));
            }
            LoadCommandVariant::Dylib(dylib) => {
                let name = command
                    .string(endian, dylib.dylib.name)
                    .map_err(malformed)?;
                let name = String::from_utf8_lossy(name);
                if let Some(file) = name.strip_prefix("@rpath/") {
                    needed.push(file.to_owned());
                }
            }
            _ => {}
        }
    }
    Ok((search, needed))
}
