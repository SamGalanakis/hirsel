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

/// The Darwin LLVM prefix supplies lld next to its dylib. Its `@loader_path/../lib`
/// search path also works after both are copied to the installation.
pub(crate) fn installation_linker(_kit: &Path, library: &Path) -> io::Result<PathBuf> {
    let prefix = library
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| io::Error::other("the LLVM library has no toolchain prefix"))?;
    Ok(prefix.join("bin/ld.lld"))
}

pub(crate) const INSTALLED_LINKER_LIBRARY: Option<&str> =
    Some(crate::product::bundled::DARWIN_LLVM_LIBRARY);

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

#[cfg(test)]
pub(crate) fn test_link(runtime: &Path) -> crate::native::Link {
    let compiler = test_compiler(runtime);
    let recipe = crate::installation::Installed::discover(&compiler)
        .unwrap()
        .unwrap()
        .link(super::HOST_TARGET.triple())
        .unwrap();
    crate::native::Link::Installed(recipe)
}

#[cfg(test)]
pub(crate) fn test_compiler(runtime: &Path) -> PathBuf {
    thread_local! {
        // The prefix is shared by a law's builds and request children.
        static INSTALLATION: std::cell::OnceCell<tempfile::TempDir> =
            const { std::cell::OnceCell::new() };
    }
    INSTALLATION.with(|installation| {
        let dir = installation.get_or_init(|| {
            let dir = tempfile::tempdir().unwrap();
            let target = super::HOST_TARGET.triple();
            let kit = PathBuf::from(std::env::var_os("HIRSEL_LINK_KIT").expect("link kit"));
            crate::installation::publish(
                dir.path(),
                false,
                [(crate::installation::Flavour::PLAIN, (runtime, target))].into(),
                Some(&kit),
                &mut crate::Meter::new(false),
            )
            .unwrap();
            dir
        });
        dir.path().join("hirsel")
    })
}
