//! An installation is a self-contained prefix: the compiler, every native
//! tool and library a request loads, the runtimes and the core/std images,
//! found relative to the installed executable. Its selection is a small
//! relocatable configuration, published last. Package bytes retain the
//! ordinary image/store format and cache-miss rules.
//!
//! ```text
//! ROOT/hirsel                       the compiler, RPATH $ORIGIN/lib
//! ROOT/lib/libLLVM.so.23.1 ...      the LLVM engine and the libraries it loads
//! ROOT/bin/ld.lld                   the link editor
//! ROOT/link/<target>.json           the link recipe captured at install
//! ROOT/runtime/<target>/<hash>/     runtime archives
//! ROOT/core-packages/               core and std images
//! ROOT/hirsel-core-toolchain.json   this selection
//! ```
//!
//! A request finds all of it from the executable alone: no environment
//! variable, no C driver and no checkout. Libraries the system provides
//! (glibc, libstdc++, libgcc_s) stay the system's.
use crate::{CoreImage, Meter, PackageProduct};
use hirsel_core_ids::ContentHash;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

const FILE: &str = "hirsel-core-toolchain.json";
const VERSION: u32 = 2;
/// The installed compiler's name in its prefix.
const EXECUTABLE: &str = "hirsel";
/// The host this compiler runs on: the one target an installation links
/// itself. Another target's runtime is installed; its link takes `--cc`.
const HOST: &str = if cfg!(target_arch = "aarch64") {
    "aarch64-unknown-linux-gnu"
} else {
    "x86_64-unknown-linux-gnu"
};
#[derive(Default, Serialize, Deserialize)]
struct Registration {
    version: u32,
    products: BTreeMap<String, PathBuf>,
    runtimes: BTreeMap<String, PathBuf>,
    #[serde(default)]
    recording_runtimes: BTreeMap<String, PathBuf>,
    /// Each linked target's recipe, relative to the root.
    links: BTreeMap<String, PathBuf>,
}
pub struct Installed {
    pub root: PathBuf,
    registration: Registration,
}
impl Installed {
    fn at(root: &Path) -> io::Result<Option<Self>> {
        let path = root.join(FILE);
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let registration: Registration =
            serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        if registration.version != VERSION {
            return Err(io::Error::other(
                "unsupported core toolchain registration version",
            ));
        }
        Ok(Some(Self {
            root: root.to_owned(),
            registration,
        }))
    }
    pub fn discover(executable: &Path) -> io::Result<Option<Self>> {
        let directory = executable
            .parent()
            .ok_or_else(|| io::Error::other("compiler has no installation directory"))?;
        let installed = Self::at(directory)?;
        if installed.is_none()
            && directory.file_name().is_some_and(|name| name == "bin")
            && let Some(prefix) = directory.parent()
        {
            return Self::at(prefix);
        }
        Ok(installed)
    }
    pub fn cache(&self) -> PathBuf {
        self.root.join("core-packages")
    }
    pub fn runtime(&self, target: &str) -> Option<PathBuf> {
        self.registration
            .runtimes
            .get(target)
            .map(|path| self.root.join(path))
    }
    pub fn recording_runtime(&self, target: &str) -> Option<PathBuf> {
        self.registration
            .recording_runtimes
            .get(target)
            .map(|path| self.root.join(path))
    }
    /// The link recipe this installation captured for `target`. A target
    /// without one links only through an explicit `--cc`.
    pub fn link(&self, target: &str) -> Option<PathBuf> {
        self.registration
            .links
            .get(target)
            .map(|path| self.root.join(path))
    }
    /// Open a registered library product by its package identity. A registered
    /// path that is missing or damaged is an installation error; callers must
    /// not silently fall back to source for a named installed product.
    pub fn product(&self, name: &str) -> io::Result<Option<std::sync::Arc<CoreImage>>> {
        let Some(path) = self.registration.products.get(name) else {
            return Ok(None);
        };
        let directory = self.root.join(path);
        let image = CoreImage::open(&directory)?.ok_or_else(|| {
            io::Error::other(format!(
                "installed {name} product is missing or damaged: {}",
                directory.display()
            ))
        })?;
        let has_package = image
            .directory()?
            .inputs
            .packages
            .iter()
            .any(|package| package.package == crate::package(name));
        if !has_package {
            return Err(io::Error::other(format!(
                "installed {name} product has the wrong package identity"
            )));
        }
        Ok(Some(image))
    }
}
/// Publish the produced image and optional runtime selection, not a second
/// representation of the image. A failed publication leaves the old selection.
pub fn register(
    root: &Path,
    name: &str,
    image: &CoreImage,
    runtime: Option<(&[u8], &str)>,
    recording_runtime: Option<(&[u8], &str)>,
    link: Option<(&str, &Path)>,
) -> io::Result<()> {
    let root = crate::product::normalized(root);
    std::fs::create_dir_all(&root)?;
    let mut registration = Installed::at(&root)?
        .map_or_else(Registration::default, |installed| installed.registration);
    registration.version = VERSION;
    let image_path = crate::product::normalized(&image.store_root());
    registration.products.insert(
        name.to_owned(),
        image_path
            .strip_prefix(&root)
            .map_err(io::Error::other)?
            .to_owned(),
    );
    let install_runtime = |bytes: &[u8], target: &str| -> io::Result<PathBuf> {
        let hash = hex(ContentHash::of_bytes(bytes));
        let relative = Path::new("runtime")
            .join(target)
            .join(hash)
            .join("libhirsel_core_rt.a");
        let path = root.join(&relative);
        let parent = path.parent().expect("runtime path has a parent");
        std::fs::create_dir_all(parent)?;
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        file.write_all(bytes)?;
        file.persist(path)?;
        Ok(relative)
    };
    if let Some((bytes, target)) = runtime {
        let relative = install_runtime(bytes, target)?;
        registration.runtimes.insert(target.to_owned(), relative);
    }
    if let Some((bytes, target)) = recording_runtime {
        let relative = install_runtime(bytes, target)?;
        registration
            .recording_runtimes
            .insert(target.to_owned(), relative);
    }
    if let Some((target, recipe)) = link {
        registration
            .links
            .insert(target.to_owned(), recipe.to_owned());
    }
    let mut file = tempfile::NamedTempFile::new_in(&root)?;
    serde_json::to_writer_pretty(&mut file, &registration).map_err(io::Error::other)?;
    file.write_all(b"\n")?;
    file.persist(root.join(FILE))?;
    Ok(())
}
fn hex(hash: ContentHash) -> String {
    hash.0.iter().map(|b| format!("{b:02x}")).collect()
}
#[derive(Serialize)]
pub struct ProductEnvironment {
    pub directory: PathBuf,
    pub image: Option<String>,
    pub registered: bool,
}
#[derive(Serialize)]
pub struct Environment {
    pub executable: PathBuf,
    pub installed_root: Option<PathBuf>,
    pub package_cache: PathBuf,
    pub products: BTreeMap<&'static str, ProductEnvironment>,
    pub target: String,
    pub runtime: Option<PathBuf>,
    pub recording_runtime: Option<PathBuf>,
}
pub fn environment(
    executable: &Path,
    installed: Option<&Installed>,
    cache: &Path,
    target: &str,
    runtime: Option<&Path>,
    recording_runtime: Option<&Path>,
) -> io::Result<Environment> {
    let mut products = BTreeMap::new();
    for name in ["core", "std"] {
        let directory = PackageProduct::cache_directory(cache, name == "std")?;
        let image = if directory.is_dir() {
            CoreImage::open(&directory)?.map(|image| hex(image.identity))
        } else {
            None
        };
        let registered = installed.is_some_and(|installed| {
            installed
                .registration
                .products
                .get(name)
                .is_some_and(|path| {
                    crate::product::normalized(&installed.root.join(path))
                        == crate::product::normalized(&directory)
                })
        });
        products.insert(
            name,
            ProductEnvironment {
                directory,
                image,
                registered,
            },
        );
    }
    Ok(Environment {
        executable: executable.to_owned(),
        installed_root: installed.map(|i| i.root.clone()),
        package_cache: crate::product::normalized(cache),
        products,
        target: target.to_owned(),
        runtime: runtime.map(Path::to_owned),
        recording_runtime: recording_runtime.map(Path::to_owned),
    })
}
/// Keep user-supplied archives in the runtime slot they implement.
pub(crate) fn check_runtime(path: &Path, bytes: &[u8], recording: bool) -> io::Result<()> {
    let defines_recording = archive_defines_symbol(bytes, b"hirsel_core_os_run_recording");
    if defines_recording == recording {
        return Ok(());
    }
    let required = if recording {
        "a recording runtime archive"
    } else {
        "a runtime archive without recording"
    };
    Err(io::Error::other(format!(
        "this build requires {required}: {}",
        path.display()
    )))
}

/// Check whether a Unix static archive (`.a`) defines `symbol` in its symbol table.
fn archive_defines_symbol(bytes: &[u8], symbol: &[u8]) -> bool {
    if !bytes.starts_with(b"!<arch>\n") {
        return false;
    }
    let mut at = 8usize;
    while at + 60 <= bytes.len() {
        let header = &bytes[at..at + 60];
        if &header[58..60] != b"`\n" {
            break;
        }
        let name = &header[0..16];
        let size = std::str::from_utf8(&header[48..58])
            .ok()
            .and_then(|s| s.trim().parse::<usize>().ok())
            .unwrap_or(0);
        let data_start = at + 60;
        let data_end = data_start.saturating_add(size).min(bytes.len());
        let data = &bytes[data_start..data_end];
        if name.starts_with(b"/ ") && data.len() >= 4 {
            let count = u32::from_be_bytes(data[0..4].try_into().unwrap()) as usize;
            let strings_start = 4 + count * 4;
            if strings_start <= data.len() {
                return data[strings_start..]
                    .split(|&b| b == 0)
                    .any(|s| s == symbol);
            }
        } else if name.starts_with(b"/SYM64/ ") && data.len() >= 8 {
            let count = usize::try_from(u64::from_be_bytes(data[0..8].try_into().unwrap()));
            if let Ok(count) = count
                && let Some(strings_start) = count.checked_add(1).and_then(|n| n.checked_mul(8))
                && let Some(names) = data.get(strings_start..)
            {
                return names.split(|&b| b == 0).any(|s| s == symbol);
            }
        }
        at = data_end + (size & 1);
    }
    false
}

/// The same producer used by the CLI and the distribution assembler. Every
/// outside input is read and checked before anything is written; the
/// selection is written last, so a failed install keeps the previous one.
///
/// Installing the host's runtime installs the native toolchain with it: the
/// LLVM engine the compiler loads, and the link editor and recipe captured
/// through `cc`, which runs only here.
pub fn publish(
    root: &Path,
    std: bool,
    runtime: Option<(&Path, &str)>,
    recording_runtime: Option<(&Path, &str)>,
    cc: Option<&Path>,
    meter: &mut Meter,
) -> io::Result<PackageProduct> {
    let root = crate::product::normalized(root);
    fn read_runtime<'a>(
        input: Option<(&Path, &'a str)>,
        recording: bool,
    ) -> io::Result<Option<(Vec<u8>, &'a str)>> {
        input
            .map(|(path, target)| {
                if !["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"].contains(&target) {
                    return Err(io::Error::other("unsupported installed runtime target"));
                }
                let bytes = std::fs::read(path)?;
                check_runtime(path, &bytes, recording)?;
                Ok((bytes, target))
            })
            .transpose()
    }
    let runtime = read_runtime(runtime, false)?;
    let recording_runtime = read_runtime(recording_runtime, true)?;
    let linked = runtime
        .as_ref()
        .map(|(_, target)| *target)
        .filter(|target| *target == HOST);
    let driver = linked
        .map(|target| {
            cc.ok_or_else(|| {
                io::Error::other(format!(
                    "installing the {target} toolchain captures its link recipe through a C driver: pass --cc"
                ))
            })
        })
        .transpose()?;
    std::fs::create_dir_all(&root)?;
    install_executable(&root)?;
    let cache = root.join("core-packages");
    let product = if std {
        PackageProduct::bundled_std(&cache, meter)?
    } else {
        PackageProduct::bundled(&cache, meter)?
    };
    let link = match (linked, driver) {
        (Some(target), Some(driver)) => {
            let library = hirsel_llvm_lazy::llvm_library().map_err(|error| {
                io::Error::other(format!("cannot install the LLVM engine: {error}"))
            })?;
            install_libraries(library, &root.join("lib"))?;
            let (recipe, linker) = crate::tool_inputs::install_recipe(driver, target, &root)?;
            let installed = root.join(crate::tool_inputs::INSTALLED_LINKER);
            install_file(&std::fs::read(&linker)?, &installed, 0o755)?;
            install_dependencies(&installed, &root.join("lib"))?;
            meter.finish("install/native toolchain");
            Some((target, recipe))
        }
        _ => None,
    };
    register(
        &root,
        if std { "std" } else { "core" },
        &product.image,
        runtime
            .as_ref()
            .map(|(bytes, target)| (bytes.as_slice(), *target)),
        recording_runtime
            .as_ref()
            .map(|(bytes, target)| (bytes.as_slice(), *target)),
        link.as_ref()
            .map(|(target, recipe)| (*target, recipe.as_path())),
    )?;
    // Sight the installed toolchain's inputs now, as a request finds them,
    // so the first request a window after the install promotes them instead
    // of sighting them itself. A toolchain that cannot be read here is a
    // build's error to report, not the install's.
    if let Some((target, _)) = link
        && let Some(installed) = Installed::at(&root)?
        && let (Some(recipe), Some(archive)) = (installed.link(target), installed.runtime(target))
    {
        let _ = crate::tool_inputs::toolchain(
            &crate::native::Link::Installed(recipe.clone()),
            target,
            &archive,
        );
        meter.finish("install/tool input sighting");
        if std {
            prebuild(&installed, target, &archive, recipe, meter)?;
        }
    }
    Ok(product)
}

/// Prebuild the installed std image's plain instances for a development
/// request on the linked host, through the check and emission that request
/// runs, into the image's store.
fn prebuild(
    installed: &Installed,
    target: &str,
    archive: &Path,
    recipe: PathBuf,
    meter: &mut Meter,
) -> io::Result<()> {
    let Some(image) = installed.product("std")? else {
        return Ok(());
    };
    let scratch = tempfile::tempdir()?;
    let request = crate::CheckRequest {
        manifest: None,
        inputs: crate::CorpusManifest {
            core: image.directory()?.inputs.core.clone(),
            packages: vec![],
        },
        store: scratch.path().join("request"),
        library: crate::LibraryInput::Image(image),
        entry: None,
    };
    let product = crate::Driver::default().check(request, meter)?;
    product
        .prebuild(
            target.to_owned(),
            archive.to_owned(),
            crate::native::Link::Installed(recipe),
            &scratch.path().join("prebuild"),
        )
        .map_err(|stop| io::Error::other(format!("prebuilding the installed std: {stop}")))?;
    meter.finish("install/prebuild");
    Ok(())
}

/// Copy this compiler into the prefix with an RPATH for its shipped libraries,
/// including when an installer puts the executable in the prefix's `bin/`.
fn install_executable(root: &Path) -> io::Result<()> {
    let source = std::env::current_exe()?;
    let installed = root.join(EXECUTABLE);
    let mut bytes = std::fs::read(&source)?;
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
    install_file(&bytes, &installed, 0o755)
}

/// Copy a shared library and every library it loads from its own directory
/// or its RUNPATH into `directory`, under the names they are loaded by.
fn install_libraries(library: &Path, directory: &Path) -> io::Result<()> {
    let name = library
        .file_name()
        .ok_or_else(|| io::Error::other(format!("{} names no library", library.display())))?;
    let installed = directory.join(name);
    install_file(&std::fs::read(library)?, &installed, 0o644)?;
    install_dependencies_from(library, &installed, directory)
}

/// Copy the libraries an installed object loads that its source's
/// directories provide; the rest are the system's.
fn install_dependencies(installed: &Path, directory: &Path) -> io::Result<()> {
    install_dependencies_from(installed, installed, directory)
}

fn install_dependencies_from(source: &Path, installed: &Path, directory: &Path) -> io::Result<()> {
    let home = source
        .canonicalize()?
        .parent()
        .map(Path::to_owned)
        .unwrap_or_default();
    let mut done = std::collections::BTreeSet::new();
    let mut pending = vec![(source.canonicalize()?, std::fs::read(installed)?)];
    while let Some((path, bytes)) = pending.pop() {
        let origin = path.parent().unwrap_or(Path::new("/"));
        let entries = dynamic_strings(&bytes)?;
        // Where this object's loader looks besides the system: its own
        // RUNPATH or RPATH, and the directory the first object came from
        // (the executable's RPATH serves libraries that name none).
        let mut search = entries
            .iter()
            .filter(|entry| [object::elf::DT_RUNPATH, object::elf::DT_RPATH].contains(&entry.tag))
            .flat_map(|entry| {
                String::from_utf8_lossy(&entry.value)
                    .split(':')
                    .map(|dir| PathBuf::from(dir.replace("$ORIGIN", &origin.to_string_lossy())))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        search.push(home.clone());
        for entry in entries
            .iter()
            .filter(|entry| entry.tag == object::elf::DT_NEEDED)
        {
            let name = String::from_utf8_lossy(&entry.value).into_owned();
            if !done.insert(name.clone()) {
                continue;
            }
            let Some(found) = search
                .iter()
                .map(|dir| dir.join(&name))
                .find(|path| path.is_file())
            else {
                continue;
            };
            let bytes = std::fs::read(&found)?;
            install_file(&bytes, &directory.join(&name), 0o644)?;
            pending.push((found.canonicalize()?, bytes));
        }
    }
    Ok(())
}

/// Write `bytes` at `path` atomically, unless the file already holds them.
fn install_file(bytes: &[u8], path: &Path, mode: u32) -> io::Result<()> {
    if std::fs::read(path).is_ok_and(|existing| existing == bytes) {
        return Ok(());
    }
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other(format!("{} has no directory", path.display())))?;
    std::fs::create_dir_all(parent)?;
    crate::executable::write(path, bytes, mode)
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
