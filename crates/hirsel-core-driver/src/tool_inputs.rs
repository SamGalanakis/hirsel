//! Identity of host tools selected for linking. The link editor is selected
//! on every request, so a request whose PATH names another ld.lld (an earlier
//! candidate installed since, a test's private LLVM copy) never links through
//! a recipe another request captured with its own.
use crate::native::Link;
use crate::{PackageProduct, Store};
use hirsel_core_ids::ContentHash;
use hirsel_core_layout::{ObjectFormat, Os, Target};
use serde::Serialize;
use std::ffi::OsStr;
use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub(crate) const INPUT_MARKER: &str = "__HIRSEL_LINK_INPUTS__";
const LINK_RECIPE_SCHEMA: u32 = 7;
const TOOLCHAIN_SCHEMA: u32 = 2;
#[derive(Clone, Debug, Serialize, serde::Deserialize, PartialEq, Eq)]
pub(crate) struct RecipeFile {
    pub(crate) path: PathBuf,
    pub(crate) content: ContentHash,
    pub(crate) stat: RecipeStat,
    pub(crate) trust: Trust,
}
/// Whether a record's stat may stand for its bytes without a read.
#[derive(Clone, Debug, Serialize, serde::Deserialize, PartialEq, Eq)]
pub(crate) enum Trust {
    /// First seen at `at` nanoseconds of `clock`'s count; every request re-hashes.
    Sighted { clock: String, at: u64 },
    /// Seen with equal bytes twice, a window apart: the stat stands for the bytes.
    Settled,
}
#[derive(Clone, Debug, Serialize, serde::Deserialize, PartialEq, Eq)]
pub(crate) struct RecipeStat {
    pub(crate) dev: u64,
    pub(crate) ino: u64,
    pub(crate) len: u64,
    pub(crate) mode: u32,
    pub(crate) mtime: i64,
    pub(crate) mtime_nsec: i64,
    pub(crate) ctime: i64,
    pub(crate) ctime_nsec: i64,
}
#[derive(Clone, Debug, Serialize, serde::Deserialize)]
struct ToolchainManifest {
    schema: u32,
    target: String,
    files: Vec<RecipeFile>,
    identity: ContentHash,
}
#[derive(Clone, Debug, Serialize, serde::Deserialize)]
pub(crate) struct LinkRecipe {
    #[serde(default)]
    pub(crate) schema: u32,
    pub(crate) target: String,
    pub(crate) linker: PathBuf,
    pub(crate) args: Vec<String>,
    pub(crate) files: Vec<RecipeFile>,
    #[serde(default)]
    pub(crate) source_files: Vec<RecipeFile>,
    /// The paths the link's searches tried before each match
    /// ([`crate::link_closure`]): one that appears is read instead.
    #[serde(default)]
    pub(crate) absent: Vec<PathBuf>,
    pub(crate) identity: ContentHash,
}

impl LinkRecipe {
    /// The generated start is the only entry to Hirsel when the surrounding
    /// link contains just the platform's startup objects and support libraries.
    pub(crate) fn uses_hirsel_launcher(&self, link: &Link) -> bool {
        self.source_files.is_empty()
            && self.files.iter().all(|file| {
                file.path == self.linker
                    || matches!(
                        file.path.file_name().and_then(OsStr::to_str),
                        Some(
                            "Scrt1.o"
                                | "crt1.o"
                                | "crti.o"
                                | "crtn.o"
                                | "crtbeginS.o"
                                | "crtendS.o"
                                | "crtbegin.o"
                                | "crtend.o"
                                | "libc.so"
                                | "libc.so.6"
                                | "libc_nonshared.a"
                                | "libdl.a"
                                | "libgcc.a"
                                | "libgcc_s.so"
                                | "libgcc_s.so.1"
                                | "libm.so"
                                | "libm.so.6"
                                | "libmvec.so.1"
                                | "libpthread.a"
                                | "librt.a"
                                | "libutil.a"
                                | "ld-linux-x86-64.so.2"
                                | "ld-linux-aarch64.so.1",
                        )
                    )
                    || matches!(link, Link::Installed(_))
                        && matches!(
                            file.path.file_name().and_then(OsStr::to_str),
                            Some(
                                "libunwind.a"
                                    | "libpthread.so.0"
                                    | "libdl.so.2"
                                    | "librt.so.1"
                                    | "libutil.so.1"
                                    | "libresolv.so.2"
                                    | "libSystem.tbd"
                            )
                        )
            })
            && !self.args.iter().any(|arg| {
                let name = arg.trim_start_matches('-').split('=').next().unwrap();
                matches!(
                    name,
                    "E" | "e"
                        | "entry"
                        | "shared"
                        | "wrap"
                        | "defsym"
                        | "export-dynamic"
                        | "export-dynamic-symbol"
                        | "dynamic-list"
                ) || (arg.starts_with("-e") && name != "eh-frame-hdr" && name != "enable-new-dtags")
            })
    }
}
fn selected(program: &Path, search: &OsStr) -> io::Result<PathBuf> {
    if program.components().count() > 1 || program.is_absolute() {
        return std::fs::canonicalize(program);
    }
    std::env::split_paths(search)
        .map(|directory| directory.join(program))
        .find(|candidate| {
            candidate.is_file()
                && candidate
                    .metadata()
                    .is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
        })
        .map(std::fs::canonicalize)
        .transpose()?
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("link tool {} is not on PATH", program.display()),
            )
        })
}
/// The C driver a request names, resolved once at the command line: a bare
/// name such as the default `cc` is looked up on `PATH`, like the shell does.
pub fn link_driver(program: &Path) -> io::Result<PathBuf> {
    selected(program, &search())
}

/// Link recipes, the link editor a driver names, and toolchain manifests are
/// records in the user's content store under keys of what selects them; an
/// object a capture compiles is a blob there, which the recipe names by path.
/// No toolchain tree is written, so a read-only one (a system `/usr/bin/cc`)
/// captures as any other.
fn recipes() -> io::Result<Store> {
    Store::cas(&crate::store::collect::user())
}

fn record_key(domain: &str, parts: &impl Serialize) -> io::Result<ContentHash> {
    Ok(ContentHash::of_parts(
        domain,
        [crate::encode(parts)?.as_slice()],
    ))
}

fn read_record<T: serde::de::DeserializeOwned>(store: &Store, key: ContentHash) -> Option<T> {
    let bytes = store.get(key).ok()??;
    serde_json::from_slice(&bytes).ok()
}

fn write_record(store: &Store, key: ContentHash, value: &impl Serialize) -> io::Result<()> {
    store.put_keyed(key, &serde_json::to_vec(value).map_err(io::Error::other)?)
}

/// The toolchain manifest of `target` through the capture keyed `key`.
fn toolchain_key(target: &str, key: &str) -> io::Result<ContentHash> {
    record_key("hirsel.toolchain-manifest.v1", &(target, key))
}

/// One host file's content under its own record: the stat rule decides
/// whether the bytes are read again.
fn recorded(path: &Path, store: &Store) -> io::Result<RecipeFile> {
    let key = ContentHash::of_parts(
        "hirsel.host-tool-revision.v2",
        [crate::encode(&path)?.as_slice()],
    );
    let selection = format!(
        "tool-{}",
        key.0.iter().map(|b| format!("{b:02x}")).collect::<String>()
    );
    let previous = store
        .selection(&selection)?
        .map(|hash| store.get(hash))
        .transpose()?
        .flatten()
        .and_then(|bytes| serde_json::from_slice::<RecipeFile>(&bytes).ok())
        .filter(|record| record.path == path);
    let seen = observe(path, previous.as_ref())?;
    if previous.as_ref() != Some(&seen) {
        let hash = store.put(&serde_json::to_vec(&seen).map_err(io::Error::other)?)?;
        store.select(&selection, hash)?;
    }
    Ok(seen)
}
fn host_tools() -> io::Result<Store> {
    Store::new(&PackageProduct::default_cache().join("host-tools"))
}

#[cfg(test)]
thread_local! {
    /// Tests stand in for another libLLVM: the file whose content LLVM keys take.
    pub(crate) static LLVM_LIBRARY: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}
/// The content of the libLLVM this process's LLVM backend loads: the file
/// `hirsel_llvm_lazy::llvm_library` resolves, which the loader then opens by
/// that path. Every key of bytes LLVM emits takes it, as the executable's
/// identity does not cover a library it opens at run time; Cranelift keys do
/// not. Read under the stat rule, once per process.
pub(crate) fn llvm_library() -> io::Result<ContentHash> {
    #[cfg(test)]
    if let Some(path) = LLVM_LIBRARY.with_borrow(Clone::clone) {
        return Ok(recorded(&path, &host_tools()?)?.content);
    }
    static CONTENT: std::sync::LazyLock<io::Result<ContentHash>> = std::sync::LazyLock::new(|| {
        let path = hirsel_llvm_lazy::llvm_library().map_err(io::Error::other)?;
        Ok(recorded(path, &host_tools()?)?.content)
    });
    CONTENT
        .as_ref()
        .copied()
        .map_err(|e| io::Error::new(e.kind(), e.to_string()))
}

/// Recapture is reserved for a recipe whose previously recorded toolchain
/// input disappeared.  Ordinary builds never invoke the C driver. An
/// installation has no driver to recapture through: its recipe is its own.
pub(crate) fn recapture_link_recipe(link: &Link, target: &str) -> io::Result<LinkRecipe> {
    match link {
        Link::Driver(cc) => capture_link_recipe(cc, target, true, &search()),
        Link::Installed(recipe) => Err(io::Error::other(format!(
            "the installation's link recipe {} names inputs that are gone; reinstall the toolchain",
            recipe.display()
        ))),
    }
}

/// An installed link's argv. File operands are relative to the bundle root.
#[derive(Serialize, serde::Deserialize)]
struct InstalledRecipe {
    schema: u32,
    target: String,
    args: Vec<String>,
}
const INSTALLED_RECIPE_SCHEMA: u32 = 2;
/// The link editor's place in an installation.
pub(crate) const INSTALLED_LINKER: &str = "bin/ld.lld";

/// The arguments an installed link starts with: an ELF link's interpreter, or
/// the flavour ld.lld must be told first to link Mach-O. The second names the
/// recipe in its identity.
fn installed_prefix(target: &str) -> io::Result<[&'static str; 2]> {
    match Target::from_triple(target) {
        Some(Target::X86_64Linux) => Ok(["-dynamic-linker", "/lib64/ld-linux-x86-64.so.2"]),
        Some(Target::Aarch64Linux) => Ok(["-dynamic-linker", "/lib/ld-linux-aarch64.so.1"]),
        Some(Target::Aarch64Darwin) => Ok(["-flavor", "darwin"]),
        None => Err(io::Error::other("no installed link target")),
    }
}

fn kit_path(path: &str) -> io::Result<&Path> {
    let path = Path::new(path);
    if path.as_os_str().is_empty()
        || !path
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_)))
    {
        return Err(io::Error::other(format!(
            "invalid link kit path {}",
            path.display()
        )));
    }
    Ok(path)
}

/// Install only the dynamic link inputs from the pinned kit.
pub(crate) fn install_recipe(kit: &Path, target: &str, root: &Path) -> io::Result<PathBuf> {
    #[derive(serde::Deserialize)]
    struct KitTarget {
        start: Vec<String>,
        unwind: String,
        unwind_script: String,
        libraries: Vec<String>,
        nonshared: String,
        end: Vec<String>,
    }
    /// A Mach-O row: the macOS floor and the text stubs of the system's
    /// libraries; libSystem carries the C runtime and the unwinder.
    #[derive(serde::Deserialize)]
    struct DarwinTarget {
        floor: String,
        libraries: Vec<String>,
    }
    // Rows differ in shape (Darwin's has no CRT files); read only the requested one.
    #[derive(serde::Deserialize)]
    struct KitManifest {
        targets: std::collections::BTreeMap<String, serde_json::Value>,
    }
    let manifest: KitManifest = serde_json::from_slice(&std::fs::read(kit.join("manifest.json"))?)
        .map_err(io::Error::other)?;
    let row = manifest.targets.get(target).cloned();
    let row = row.ok_or_else(|| io::Error::other("link kit has no installed target"))?;
    let placed = format!("link/kit/{target}");
    let copy = |path: &str| -> io::Result<String> {
        let source = kit.join(target).join(kit_path(path)?);
        let relative = format!("{placed}/{path}");
        crate::installation::install_file(&std::fs::read(source)?, &root.join(&relative), 0o644)?;
        Ok(relative)
    };
    let target_os = Target::from_triple(target)
        .ok_or_else(|| io::Error::other("no installed link target"))?
        .os();
    let args = match target_os {
        Os::Linux => {
            let inputs = &serde_json::from_value::<KitTarget>(row).map_err(io::Error::other)?;
            let mut args = [
                "-pie",
                "--eh-frame-hdr",
                "--hash-style=gnu",
                "-z",
                "noexecstack",
            ]
            .map(str::to_owned)
            .to_vec();
            match Target::from_triple(target) {
                Some(Target::Aarch64Linux) => args.push("--fix-cortex-a53-843419".into()),
                Some(Target::X86_64Linux | Target::Aarch64Darwin) | None => {}
            }
            for path in &inputs.start {
                args.push(copy(path)?);
            }
            args.push(INPUT_MARKER.into());
            copy(&inputs.unwind)?;
            args.push(copy(&inputs.unwind_script)?);
            args.push("--as-needed".into());
            for path in &inputs.libraries {
                args.push(copy(path)?);
            }
            args.push(copy(&inputs.nonshared)?);
            for path in &inputs.end {
                args.push(copy(path)?);
            }
            args
        }
        // The image's minimum and SDK versions are both the floor; ld64
        // signs an arm64 image ad hoc, which macOS requires to run it.
        Os::Macos => {
            let inputs = &serde_json::from_value::<DarwinTarget>(row).map_err(io::Error::other)?;
            let mut args = ["-arch", "arm64", "-platform_version", "macos"]
                .map(str::to_owned)
                .to_vec();
            args.extend([inputs.floor.clone(), inputs.floor.clone()]);
            args.push("-adhoc_codesign".into());
            args.push(INPUT_MARKER.into());
            for path in &inputs.libraries {
                args.push(copy(path)?);
            }
            args
        }
    };
    crate::installation::install_file(
        &std::fs::read(kit.join("ld.lld"))?,
        &root.join(INSTALLED_LINKER),
        0o755,
    )?;
    let relative = Path::new("link").join(format!("{target}.json"));
    let directory = root.join("link");
    let installed = InstalledRecipe {
        schema: INSTALLED_RECIPE_SCHEMA,
        target: target.into(),
        args,
    };
    publish(&directory, &root.join(&relative), &installed)?;
    Ok(relative)
}

/// What native products are keyed by, besides the Hirsel compiler itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Toolchain {
    /// Every link input: C driver, libLLVM, linker, CRT and libc files, runtime archive.
    pub(crate) image: ContentHash,
    /// The runtime archive alone: objects are emitted in process against its
    /// ABI, so a different C driver or linker relinks them but compiles none.
    pub(crate) runtime: ContentHash,
    /// The keyed environment digest: the (name, value) pairs a capture's
    /// child tools run under ([`CAPTURE_VARIABLES`]; an installation's link
    /// takes none) and the compiler's own output reads
    /// ([`COMPILER_VARIABLES`]), and nothing else of the environment.
    pub(crate) environment: ContentHash,
}

/// The identity native objects are keyed by for a runtime archive whose bytes
/// hash to `content`: objects are emitted against its ABI.
pub(crate) fn runtime_identity(content: ContentHash) -> ContentHash {
    ContentHash::of_parts("hirsel.core-runtime.v2", [content.0.as_slice()])
}

/// One cached record of every toolchain input. Stat signatures avoid
/// re-reading unchanged runtime, linker and CRT bytes on every request. The
/// recipe belongs to the C driver and target alone; the runtime archive is
/// this request's own input, so one recipe serves every runtime.
///
/// The request's link recipe comes back with it, so the request observes
/// each file once: the recipe's files are recorded here as the recipe saw
/// them, and the link uses this recipe rather than observing them again.
pub(crate) fn toolchain(
    link: &Link,
    target: &str,
    runtime: &Path,
) -> io::Result<(Toolchain, LinkRecipe)> {
    match link {
        Link::Driver(cc) => driver_toolchain(cc, target, runtime),
        Link::Installed(recipe) => installed_toolchain(recipe, target, runtime),
    }
}

/// Hash the installed argv, resolved bundle files, and runtime. Resolve
/// relative paths only for the link so the key survives relocation.
fn installed_toolchain(
    path: &Path,
    target: &str,
    runtime: &Path,
) -> io::Result<(Toolchain, LinkRecipe)> {
    let refused = |what: String| {
        io::Error::other(format!(
            "the installation's link recipe {} {what}; reinstall the toolchain",
            path.display()
        ))
    };
    let bytes = std::fs::read(path).map_err(|error| refused(format!("cannot be read: {error}")))?;
    let installed: InstalledRecipe =
        serde_json::from_slice(&bytes).map_err(|error| refused(format!("is damaged: {error}")))?;
    if installed.schema != INSTALLED_RECIPE_SCHEMA || installed.target != target {
        return Err(refused(format!(
            "is not a {target} recipe of this compiler"
        )));
    }
    let root = path
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| refused("has no installation around it".into()))?;
    let linker = root.join(INSTALLED_LINKER);
    if !linker.is_file() {
        return Err(refused(format!(
            "names missing inputs: {}",
            linker.display()
        )));
    }
    let [flag, interpreter] = installed_prefix(target)?;
    let mut args = vec![flag.to_owned(), interpreter.to_owned()];
    for arg in &installed.args {
        if arg.starts_with("link/kit/") {
            let path = root.join(kit_path(arg)?);
            args.push(
                path.to_str()
                    .ok_or_else(|| refused("has a non-UTF-8 path".into()))?
                    .to_owned(),
            );
        } else {
            args.push(arg.clone());
        }
    }
    let format = Target::from_triple(target)
        .ok_or_else(|| refused("names no target".into()))?
        .object_format();
    let closure = match format {
        ObjectFormat::Elf => crate::link_closure::closure(&args, INPUT_MARKER, true)
            .map_err(|error| refused(error.to_string()))?,
        // ld64 searches nothing for this recipe: its files are its operands.
        ObjectFormat::MachO => {
            let files: Vec<PathBuf> = installed
                .args
                .iter()
                .filter(|arg| arg.starts_with("link/kit/"))
                .map(|arg| root.join(arg))
                .collect();
            crate::link_closure::Closure {
                binaries: files.clone(),
                files,
                absent: Vec::new(),
            }
        }
    };
    let mut paths = closure.files;
    paths.push(linker.clone());
    let real_root = root.canonicalize()?;
    if paths.iter().any(|path| {
        !path
            .canonicalize()
            .is_ok_and(|real| real.starts_with(&real_root))
    }) {
        return Err(refused("names inputs outside the installation".into()));
    }
    let store = host_tools()?;
    let files = paths
        .iter()
        .map(|path| recorded(path, &store))
        .collect::<io::Result<Vec<_>>>()?;
    let archive = recorded(runtime, &store)?;
    // A relocated installation keeps its identity and its prebuilt objects.
    let placed = files
        .iter()
        .map(|file| {
            (
                file.path.strip_prefix(root).unwrap_or(&file.path),
                &file.content,
            )
        })
        .collect::<Vec<_>>();
    let identity = ContentHash::of_parts(
        "hirsel.installed-link-recipe.v4",
        [crate::encode(&(
            INSTALLED_RECIPE_SCHEMA,
            target,
            interpreter,
            Path::new(INSTALLED_LINKER),
            &installed.args,
            placed,
            &closure.absent,
        ))?
        .as_slice()],
    );
    let toolchain = Toolchain {
        image: ContentHash::of_parts(
            "hirsel.installed-toolchain-image.v2",
            [crate::encode(&(identity, &archive.content))?.as_slice()],
        ),
        runtime: runtime_identity(archive.content),
        environment: keyed_environment(&[]),
    };
    let recipe = LinkRecipe {
        schema: LINK_RECIPE_SCHEMA,
        target: target.into(),
        linker,
        args,
        files,
        source_files: Vec::new(),
        absent: closure.absent,
        identity,
    };
    Ok((toolchain, recipe))
}

fn driver_toolchain(
    cc: &Path,
    target: &str,
    runtime: &Path,
) -> io::Result<(Toolchain, LinkRecipe)> {
    let (recipe, key, environment) = capture_keyed_link_recipe(cc, target, false, &search())?;
    let store = recipes()?;
    let record = toolchain_key(target, &key)?;
    let mut paths = vec![cc.canonicalize()?, runtime.to_owned()];
    if let Some(lib) = find_llvm_library(cc) {
        paths.push(lib);
    }
    paths.extend(recipe.files.iter().map(|file| file.path.clone()));
    paths.sort();
    paths.dedup();
    let old = read_record::<ToolchainManifest>(&store, record)
        .filter(|manifest| manifest.schema == TOOLCHAIN_SCHEMA && manifest.target == target);
    let mut files = Vec::with_capacity(paths.len());
    for file in paths {
        if let Some(seen) = recipe.files.iter().find(|entry| entry.path == file) {
            files.push(seen.clone());
            continue;
        }
        let previous = old
            .as_ref()
            .and_then(|manifest| manifest.files.iter().find(|entry| entry.path == file));
        files.push(observe(&file, previous)?);
    }
    let archive = files
        .iter()
        .find(|file| file.path == runtime)
        .expect("the runtime archive is a recorded toolchain input")
        .content;
    // The runtime archive is this request's input, wherever its build put it,
    // and the C driver and libLLVM reach the link only through what they
    // run: their bytes are their identity. The recipe's identity holds what
    // the link reads.
    let mut tools = files
        .iter()
        .filter(|file| {
            file.path != runtime && !recipe.files.iter().any(|seen| seen.path == file.path)
        })
        .map(|file| file.content)
        .collect::<Vec<_>>();
    tools.sort();
    let identity = ContentHash::of_parts(
        "hirsel.toolchain-image.v4",
        [crate::encode(&(TOOLCHAIN_SCHEMA, target, recipe.identity, tools, archive))?.as_slice()],
    );
    let toolchain = Toolchain {
        image: identity,
        runtime: runtime_identity(archive),
        environment,
    };
    // A moved stat over equal bytes is a change to the record: rewrite it so
    // the next request reads nothing. An unchanged record is not rewritten.
    if old.is_none_or(|old| old.files != files) {
        let manifest = ToolchainManifest {
            schema: TOOLCHAIN_SCHEMA,
            target: target.into(),
            files,
            identity,
        };
        write_record(&store, record, &manifest)?;
    }
    Ok((toolchain, recipe))
}

fn find_llvm_library(cc: &Path) -> Option<PathBuf> {
    let lib = cc.parent()?.parent()?.join("lib");
    std::fs::read_dir(lib)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("libLLVM") && name.contains(".so"))
        })
}

/// An installed compiler's own link editor. A recipe captured through a C
/// driver handed to it with `--cc` links with that ld.lld, never with one the
/// driver finds on PATH.
fn installed_linker() -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    let installed = crate::installation::Installed::discover(&executable).ok()??;
    Some(installed.root.join(INSTALLED_LINKER)).filter(|linker| linker.is_file())
}

/// The name a C driver gives its link editor, as a capture learnt it: a path
/// the driver chose itself, or a bare name it leaves to `PATH` (gcc names
/// `ld.lld` bare when its own tree has none). It is the driver's, so one
/// record serves every request through that driver; the `PATH` lookup of a
/// bare name is each request's own.
#[derive(Serialize, serde::Deserialize)]
struct EditorName {
    schema: u32,
    name: PathBuf,
}
const EDITOR_NAME_SCHEMA: u32 = 1;

fn editor_name_key(target: &str, driver_key: &str) -> io::Result<ContentHash> {
    record_key("hirsel.link-editor-name.v1", &(target, driver_key))
}

/// The recipe record of `driver` linking through `linker`. The link editor is
/// part of the key: two requests whose `PATH`s select different link editors
/// through one driver (a test that puts a private LLVM copy first) never share
/// a recipe, and a recipe captured outside an installation, whose own ld.lld
/// links, is never reused inside one or from a moved one.
fn recipe_key(target: &str, driver_key: &str, linker: &Path) -> io::Result<ContentHash> {
    record_key("hirsel.link-recipe.v1", &(target, driver_key, linker))
}

/// The link editor this request links with: the installation's own, else the
/// one the driver's recorded name selects on this request's `PATH`. `None`
/// when no capture has recorded the driver's name yet.
fn selected_editor(
    store: &Store,
    target: &str,
    driver_key: &str,
    installed: Option<&Path>,
    search: &OsStr,
) -> io::Result<Option<PathBuf>> {
    if let Some(installed) = installed {
        return Ok(Some(installed.to_owned()));
    }
    let recorded = read_record::<EditorName>(store, editor_name_key(target, driver_key)?)
        .filter(|record| record.schema == EDITOR_NAME_SCHEMA);
    Ok(recorded.and_then(|record| selected(&record.name, search).ok()))
}

/// This request's `PATH`, which selects a link editor the driver names bare.
fn search() -> std::ffi::OsString {
    std::env::var_os("PATH").unwrap_or_default()
}

fn capture_link_recipe(
    cc: &Path,
    target: &str,
    force: bool,
    search: &OsStr,
) -> io::Result<LinkRecipe> {
    capture_keyed_link_recipe(cc, target, force, search).map(|(recipe, ..)| recipe)
}

/// Capture the link editor command once beside the selected toolchain, with
/// the driver's file key, hashed once for both. The capture is the only
/// C-driver use in the direct-link path; subsequent builds invoke the recorded
/// lld argv themselves.
fn capture_keyed_link_recipe(
    cc: &Path,
    target: &str,
    force: bool,
    search: &OsStr,
) -> io::Result<(LinkRecipe, String, ContentHash)> {
    let driver = cc.to_owned();
    let driver_key = driver_file_key(&driver)?;
    let environment = CaptureEnvironment::new(search)?;
    let file_key = environment.key(&driver_key)?;
    capture_link_recipe_with(cc, target, force, &environment, &file_key)
        .map(|r| (r, file_key, environment.digest()))
}

/// The variables a capture's child tools run under; nothing else is
/// inherited. `PATH`: a driver finds the tools it runs by it (clang looks
/// beside itself and then on `PATH` for ld.lld, gcc's collect2 runs its
/// editor by name, a wrapper runs `clang`), and only the link editor is a
/// name this compiler resolves itself, so the resolved paths in the key
/// cannot stand for it. `LD_LIBRARY_PATH`: the loader finds a driver's own
/// libraries by it (this repository's LLVM clang does not start without
/// it). No `TMPDIR`: `-###` only names temporaries, and every file a
/// capture writes has an explicit path. No locale: a child runs in the C
/// locale, which changes no argv.
const CAPTURE_VARIABLES: [&str; 2] = ["PATH", "LD_LIBRARY_PATH"];
/// The compiler's own environment reads that can change its output, on the
/// same keyed list as [`CAPTURE_VARIABLES`] but never passed to a child. Its
/// other reads change no output: `HIRSEL_MEMORY_LIMIT` only refuses a
/// request, and a refused result is never published; `XDG_CACHE_HOME`,
/// `HOME` and the default C driver's `LLVM_SYS_231_PREFIX` choose a place,
/// whose bytes key what is read there; `HIRSEL_LLVM_PRINTED_ORACLE` writes a
/// side report, as does `HIRSEL_RETURNABLE_REPORT`.
const COMPILER_VARIABLES: [&str; 2] = [
    hirsel_llvm_lazy::core::NO_FACTS_VARIABLE,
    hirsel_core_ir::returnable::DISABLE,
];

/// What a capture's child tools observe besides their argv and the files
/// they read: the listed variables (`PATH` is the request's search path),
/// the working directory (a wrapper's relative path, clang's
/// `-fdebug-compilation-dir`) and the umask they inherit. It is in the key
/// of the recipe, the editor name and the toolchain manifest a capture
/// writes, so a request under another environment never reads a capture
/// made under this one, and a recipe's relative paths name what they named
/// at its capture.
struct CaptureEnvironment {
    variables: Vec<(&'static str, std::ffi::OsString)>,
    directory: PathBuf,
    umask: u32,
}

impl CaptureEnvironment {
    fn new(search: &OsStr) -> io::Result<Self> {
        let variables = CAPTURE_VARIABLES
            .into_iter()
            .filter_map(|name| {
                // An unset PATH stays unset: a shell then searches its
                // own default, where an empty one would find nothing.
                let value = match name {
                    "PATH" => Some(search.to_owned()).filter(|search| !search.is_empty()),
                    _ => std::env::var_os(name),
                };
                value.map(|value| (name, value))
            })
            .collect();
        Ok(Self {
            variables,
            directory: std::env::current_dir()?,
            umask: umask()?,
        })
    }

    fn pairs(&self) -> Vec<(&'static str, &[u8])> {
        use std::os::unix::ffi::OsStrExt;
        self.variables
            .iter()
            .map(|(name, value)| (*name, value.as_bytes()))
            .collect()
    }

    /// The keyed environment digest over the listed pairs: what a request
    /// takes from the environment. The directory and umask key the capture
    /// file.
    fn digest(&self) -> ContentHash {
        keyed_environment(&self.pairs())
    }

    fn key(&self, driver_key: &str) -> io::Result<String> {
        let variables = self.pairs();
        let hash = ContentHash::of_parts(
            "hirsel.link-driver-environment.v1",
            [
                driver_key.as_bytes(),
                crate::encode(&(variables, &self.directory, self.umask))?.as_slice(),
            ],
        );
        Ok(hash.0.iter().map(|b| format!("{b:02x}")).collect())
    }

    fn command(&self, program: impl AsRef<OsStr>) -> Command {
        let mut command = tool_command(program);
        command.envs(self.variables.iter().map(|(name, value)| (name, value)));
        command
    }
}

/// `hirsel.keyed-environment.v1` over `children`, the (name, value) pairs a
/// toolchain's child tools run under, then the listed compiler reads. A
/// request takes nothing else from the environment.
fn keyed_environment(children: &[(&str, &[u8])]) -> ContentHash {
    use std::os::unix::ffi::OsStrExt;
    let compiler = COMPILER_VARIABLES
        .into_iter()
        .filter_map(|name| std::env::var_os(name).map(|value| (name, value)));
    let mut bytes = Vec::new();
    let mut part = |part: &[u8]| {
        bytes.extend_from_slice(&(part.len() as u64).to_le_bytes());
        bytes.extend_from_slice(part);
    };
    for (name, value) in children {
        part(name.as_bytes());
        part(value);
    }
    for (name, value) in compiler {
        part(name.as_bytes());
        part(value.as_bytes());
    }
    ContentHash::of_parts("hirsel.keyed-environment.v1", [&bytes[..]])
}

/// This process's umask, as the kernel reports it; a child inherits it.
fn umask() -> io::Result<u32> {
    let status = std::fs::read_to_string("/proc/self/status")?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("Umask:"))
        .and_then(|value| u32::from_str_radix(value.trim(), 8).ok())
        .ok_or_else(|| io::Error::other("/proc/self/status names no umask"))
}

/// A child tool: no standard input, an empty environment. A link editor
/// gets nothing more: it reads no variable that changes its output; it runs
/// in the request's working directory, which is in the key of the recipe
/// whose relative paths it resolves; and the files it writes are read back
/// as bytes, so its umask reaches no output. A capture adds
/// [`CAPTURE_VARIABLES`]. Descriptors past stderr are close-on-exec in this
/// process (main), so a child inherits none.
pub(crate) fn tool_command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    command.env_clear().stdin(Stdio::null());
    command
}

fn capture_link_recipe_with(
    cc: &Path,
    target: &str,
    force: bool,
    environment: &CaptureEnvironment,
    file_key: &str,
) -> io::Result<LinkRecipe> {
    let search = environment
        .variables
        .iter()
        .find(|(name, _)| *name == "PATH")
        .map_or(OsStr::new(""), |(_, value)| value.as_os_str());
    let driver = cc.to_owned();
    let store = recipes()?;
    let installed_linker = installed_linker();
    if !force
        && let Some(linker) = selected_editor(
            &store,
            target,
            file_key,
            installed_linker.as_deref(),
            search,
        )?
    {
        let key = recipe_key(target, file_key, &linker)?;
        if let Some(mut recipe) = read_record::<LinkRecipe>(&store, key)
            && recipe.schema == LINK_RECIPE_SCHEMA
            && recipe.target == target
            && recipe.linker == linker
            && recipe.linker.is_file()
        {
            match refresh_recipe_inputs(&mut recipe)? {
                RecipeInputs::Current => return Ok(recipe),
                RecipeInputs::Restated => {
                    write_record(&store, key, &recipe)?;
                    return Ok(recipe);
                }
                RecipeInputs::Changed => {}
            }
        }
    }
    // A driver that names a link editor which does not exist must not be
    // captured: clang would silently fall back to some other ld.lld.
    let selection = environment
        .command(&driver)
        .arg("-print-prog-name=ld.lld")
        .output()?;
    if selection.status.success() {
        let selected = PathBuf::from(String::from_utf8_lossy(&selection.stdout).trim());
        if selected.is_absolute() && !selected.is_file() {
            return Err(io::Error::other(format!(
                "missing link editor input: {}",
                selected.display()
            )));
        }
    }
    let capture_dir = tempfile::tempdir()?;
    let synthetic_source = capture_dir.path().join("recipe.c");
    std::fs::write(&synthetic_source, "int main(void) { return 0; }\n")?;
    let capture = |driver: &Path, source: &Path, output: &Path| {
        let mut command = environment.command(driver);
        command.args(["-###", "-fuse-ld=lld", "-pie"]);
        if driver
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.contains("clang"))
        {
            command.arg(format!("--target={target}"));
        }
        command.arg(source).arg("-o").arg(output).output()
    };
    // The selected driver is the one captured: a driver that cannot capture
    // is refused, never swapped for another clang.
    let output = capture(
        &driver,
        &synthetic_source,
        &capture_dir.path().join("recipe"),
    )?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "link recipe capture failed through {}: {}",
            driver.display(),
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let line = stderr
        .lines()
        .filter(|line| line.contains("ld.lld") || line.contains("collect2"))
        .last()
        .ok_or_else(|| io::Error::other("link recipe capture emitted no link editor argv"))?;
    let mut argv = shell_words(line)?;
    // The driver's name for its link editor. gcc's line starts with collect2,
    // so its answer to -print-prog-name is the name; a clang line starts with
    // the editor clang chose.
    let collect2 = argv[0].contains("collect2");
    let first = PathBuf::from(argv.remove(0));
    let name = if collect2 {
        let answer = String::from_utf8_lossy(&selection.stdout).trim().to_owned();
        if !selection.status.success() || answer.is_empty() || answer.contains('\n') {
            return Err(io::Error::other(format!(
                "C driver {} did not name one link editor",
                driver.display()
            )));
        }
        PathBuf::from(answer)
    } else {
        first
    };
    if name.components().count() > 1 && !name.is_file() {
        return Err(io::Error::other(format!(
            "missing link editor input: {}",
            name.display()
        )));
    }
    let linker = match &installed_linker {
        Some(installed) => installed.clone(),
        None => selected(&name, search)?,
    };
    // clang looks beside itself, then on PATH: an editor it found on PATH is
    // PATH's choice, so the record keeps it bare and each request selects its own.
    let name = match name.file_name() {
        Some(file)
            if !collect2
                && name.is_absolute()
                && linker.parent() != driver.canonicalize()?.parent()
                && selected(Path::new(file), search).ok().as_ref() == Some(&linker) =>
        {
            PathBuf::from(file)
        }
        _ => name,
    };
    let key = recipe_key(target, file_key, &linker)?;
    let mut source_files = Vec::new();
    let mut compiled_objects = std::collections::HashMap::new();
    for (index, line) in stderr.lines().enumerate() {
        let words = shell_words(line)?;
        if !words.iter().any(|word| word == "-cc1") {
            continue;
        }
        let Some(output_index) = words.iter().position(|word| word == "-o") else {
            continue;
        };
        let Some(transient_output) = words.get(output_index + 1).map(PathBuf::from) else {
            continue;
        };
        let Some(source) = words
            .iter()
            .rev()
            .find(|word| is_c_source(word) && Path::new(word).is_file())
        else {
            continue;
        };
        // The synthetic recipe source is only there to expose the normal
        // toolchain command; its object is represented by INPUT_MARKER.
        if Path::new(source) == synthetic_source.as_path() {
            continue;
        }
        let compiled_path = capture_dir.path().join(format!("extra-{index}.o"));
        let mut command = environment.command(&words[0]);
        let mut args = words[1..].to_vec();
        if let Some(output) = args.iter_mut().position(|word| word == "-o") {
            args[output + 1] = compiled_path.display().to_string();
        }
        let compiled = command.args(&args).output()?;
        if !compiled.status.success() {
            return Err(io::Error::other(format!(
                "compiling link source {} failed: {}",
                source,
                String::from_utf8_lossy(&compiled.stderr)
            )));
        }
        // The link reads the object as a blob; the recipe records its bytes
        // among the files the link reads, so a blob since evicted recaptures.
        let blob = store.put_blob(&std::fs::read(&compiled_path)?)?;
        compiled_objects.insert(transient_output, store.blob_path(blob.address));
        let source = PathBuf::from(source);
        source_files.push(recipe_file(&source)?);
    }
    if collect2 {
        let mut filtered = Vec::with_capacity(argv.len());
        let mut skip = false;
        for arg in argv {
            if skip {
                skip = false;
                continue;
            }
            if arg == "-plugin" {
                skip = true;
            } else if arg.starts_with("-plugin-opt=") || arg == "-fuse-ld=lld" {
                continue;
            } else {
                filtered.push(arg);
            }
        }
        argv = filtered;
    }
    let mut args = Vec::with_capacity(argv.len() + 1);
    let mut marker = None;
    let mut i = 0;
    while i < argv.len() {
        let arg = &argv[i];
        if arg == "-o" {
            i += 2;
            continue;
        }
        // clang names the object produced for the stdin source `--*.o`.
        // Keep any other object: provider tests and link recipes may carry a
        // fixed support object that must remain an input to every executable.
        if let Some(stable) = compiled_objects.get(Path::new(arg)) {
            args.push(stable.display().to_string());
            i += 1;
            continue;
        }
        // A driver that finds no startup file for the target (clang without a
        // GCC installation or sysroot for it) names the file bare, while every
        // object a driver compiles itself is a path into its temporary
        // directory. The bare name is a link input the host lacks.
        if arg.ends_with(".o")
            && Path::new(arg).components().count() == 1
            && !Path::new(arg).exists()
        {
            return Err(host_link_input_error(io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "missing link input: {} found no {arg} for {target}",
                    driver.display()
                ),
            )));
        }
        let transient = arg.ends_with(".o")
            && Path::new(arg)
                .file_stem()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("--") || !Path::new(arg).exists());
        if transient {
            // Only the synthetic source's object stands for the build's
            // own inputs. Another transient object is a source the driver
            // adds itself (a wrapper's support file) whose compile job this
            // capture could not replay (a gcc `cc1` and `as` pair rather
            // than a clang `-cc1`): folding it into the marker would drop a
            // link input and link a different program.
            if marker.replace(args.len()).is_some() {
                return Err(io::Error::other(format!(
                    "link recipe capture through {} cannot replay the compile of a link source the driver adds; name a clang driver",
                    driver.display()
                )));
            }
        } else {
            args.push(arg.clone());
        }
        i += 1;
    }
    let marker = marker.unwrap_or_else(|| {
        args.iter()
            .position(|arg| arg == "-lc" || arg == "--as-needed")
            .unwrap_or(args.len())
    });
    args.insert(marker, INPUT_MARKER.into());
    // These libraries follow the request objects in the driver's link order.
    let support = ["-lgcc_s", "-lpthread", "-ldl", "-lm", "-lrt", "-lutil"];
    args.splice(marker + 1..marker + 1, support.map(str::to_owned));
    // The driver's configuration files decide its argv: a changed one
    // captures again.
    let configuration = stderr
        .lines()
        .filter_map(|line| line.strip_prefix("Configuration file: "))
        .map(PathBuf::from);
    let (files, absent) = recipe_files(&linker, &args, configuration)?;
    let identity = recipe_identity(
        target,
        ContentHash::of_bytes(&std::fs::read(&driver)?),
        &linker,
        &args,
        &files,
        &source_files,
        &absent,
    )?;
    let recipe = LinkRecipe {
        schema: LINK_RECIPE_SCHEMA,
        target: target.into(),
        linker,
        args,
        files,
        source_files,
        absent,
        identity,
    };
    write_record(&store, key, &recipe)?;
    if installed_linker.is_none() {
        let record = EditorName {
            schema: EDITOR_NAME_SCHEMA,
            name,
        };
        write_record(&store, editor_name_key(target, file_key)?, &record)?;
    }
    Ok(recipe)
}

/// The link editor, every file the link reads ([`crate::link_closure`]) and
/// `configuration`, each observed, and the paths the link's searches found
/// absent.
fn recipe_files(
    linker: &Path,
    args: &[String],
    configuration: impl Iterator<Item = PathBuf>,
) -> io::Result<(Vec<RecipeFile>, Vec<PathBuf>)> {
    let closure = crate::link_closure::closure(args, INPUT_MARKER, false).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            host_link_input_error(error)
        } else {
            error
        }
    })?;
    let mut paths = closure.files;
    paths.push(linker.to_owned());
    paths.extend(configuration);
    paths.sort();
    paths.dedup();
    let missing = paths
        .iter()
        .filter(|path| !path.is_file())
        .cloned()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(io::Error::other(format!(
            "missing recipe inputs: {}",
            missing
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    let files = paths
        .into_iter()
        .map(|path| recipe_file(&path))
        .collect::<io::Result<_>>()?;
    Ok((files, closure.absent))
}

fn host_link_input_error(error: io::Error) -> io::Error {
    io::Error::new(
        error.kind(),
        format!(
            "{error}; install the target's glibc and GCC development files (libc6-dev and libgcc-N-dev on Debian/Ubuntu, glibc-devel and gcc on Fedora). Match the GCC version and paths in the link recipe, or select a configured C driver with --cc"
        ),
    )
}

/// The recipe's identity: the driver's and the link editor's bytes, wherever
/// they lie (their paths reach the link only through the argv), the argv,
/// every other file the link reads by its path and bytes (the argv and the
/// scripts name them so), and the paths the searches found absent.
fn recipe_identity(
    target: &str,
    driver: ContentHash,
    linker: &Path,
    args: &[String],
    files: &[RecipeFile],
    source_files: &[RecipeFile],
    absent: &[PathBuf],
) -> io::Result<ContentHash> {
    let editor = files
        .iter()
        .find(|file| file.path == linker)
        .map(|file| file.content);
    let named = files
        .iter()
        .filter(|file| file.path != linker)
        .cloned()
        .collect::<Vec<_>>();
    Ok(ContentHash::of_parts(
        "hirsel.direct-link-recipe.v3",
        [crate::encode(&(
            LINK_RECIPE_SCHEMA,
            target,
            driver,
            editor,
            args,
            image(&named),
            image(source_files),
            absent,
        ))?
        .as_slice()],
    ))
}

/// What an input contributes to an identity: its path and bytes. Its stat is
/// only the cue to re-hash; a metadata-only change (a materialiser re-linking
/// the file moves ctime) keeps the identity, so every keyed object stays.
fn image(files: &[RecipeFile]) -> Vec<(&Path, &ContentHash)> {
    files
        .iter()
        .map(|file| (file.path.as_path(), &file.content))
        .collect()
}

#[derive(Debug, PartialEq, Eq)]
enum RecipeInputs {
    Current,
    /// Some stat moved over unchanged bytes; the recipe now records the new stat.
    Restated,
    Changed,
}

/// Compare a recorded recipe's inputs by metadata first; only an input whose
/// stat moved is re-hashed. Equal bytes under a moved stat (Buck re-links a
/// toolchain file) update the recorded stat, so the next request reads none.
fn refresh_recipe_inputs(recipe: &mut LinkRecipe) -> io::Result<RecipeInputs> {
    let mut inputs = RecipeInputs::Current;
    if recipe.absent.iter().any(|path| path.exists()) {
        return Ok(RecipeInputs::Changed);
    }
    for input in recipe.files.iter_mut().chain(&mut recipe.source_files) {
        let Ok(seen) = observe(&input.path, Some(input)) else {
            return Ok(RecipeInputs::Changed);
        };
        if seen.content != input.content {
            return Ok(RecipeInputs::Changed);
        }
        if seen != *input {
            *input = seen;
            inputs = RecipeInputs::Restated;
        }
    }
    Ok(inputs)
}

fn publish(directory: &Path, path: &Path, value: &impl Serialize) -> io::Result<()> {
    let temporary = tempfile::NamedTempFile::new_in(directory)?;
    serde_json::to_writer(&temporary, value).map_err(io::Error::other)?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}

fn recipe_file(path: &Path) -> io::Result<RecipeFile> {
    observe(path, None)
}

/// The rule that lets a stat stand for a file's bytes:
///
/// 1. A revision is recorded `Sighted` the first time it is seen, with the
///    time of that sighting on our own clock; a sighted record is re-hashed on
///    every request and never vouches for bytes.
/// 2. A later request that finds the same stat re-hashes; equal bytes and at
///    least `WINDOW` between the two sightings promote it to `Settled`.
/// 3. A settled record whose stat equals the live one is trusted unread.
/// 4. Stat and bytes come from one opened file. The promoting sighting's time
///    is taken before its stat, the first sighting's after, so the window is a
///    lower bound on real time between the file's last change and the read.
/// 5. Durations are measured on CLOCK_BOOTTIME (/proc/uptime), compared only
///    between sightings of one boot and one time namespace; file times are
///    never compared with our clock. Without a well-formed boot id and time
///    namespace there is no clock, and nothing is promoted.
///
/// Rule 5 governs how a record becomes settled, not what settled means:
/// settled is a fact about the file's stat (unchanged for at least the window,
/// so its timestamp tick is over and any later write moves ctime), so a settled
/// record whose stat equals the live one holds in any boot and without a clock.
///
/// What this does not defend, by contract: a write through an already
/// writable shared mapping that moves no timestamp until writeback; a
/// filesystem whose timestamp granularity exceeds `WINDOW`; a file clock set
/// back so that a later edit repeats an earlier timestamp exactly; and an
/// input changed during the request that reads it.
const WINDOW: u64 = 2_000_000_000;

/// Observe a file against its previous record: trust a settled stat, else
/// re-hash and record the sighting.
fn observe(path: &Path, previous: Option<&RecipeFile>) -> io::Result<RecipeFile> {
    if let Some(previous) = previous.filter(|record| record.trust == Trust::Settled)
        && recipe_stat(path)? == previous.stat
    {
        return Ok(previous.clone());
    }
    let before = clock();
    let mut file = std::fs::File::open(path)?;
    let stat = stat_of(&file.metadata()?);
    let after = clock();
    let content = ContentHash::of_stream("hirsel.text.v1", stat.len, |digest| {
        io::copy(&mut file, digest).map(drop)
    })?;
    #[cfg(test)]
    {
        READS.set(READS.get() + 1);
        if let Some(mut after_read) = AFTER_READ.take() {
            after_read();
        }
    }
    let same = previous.filter(|record| record.stat == stat && record.content == content);
    let trust = match (same.map(|record| &record.trust), &before) {
        (Some(Trust::Settled), _) => Trust::Settled,
        (Some(sighted @ Trust::Sighted { clock, at }), Some(now)) if *clock == now.clock => {
            if now.earliest.saturating_sub(*at) >= WINDOW {
                Trust::Settled
            } else {
                sighted.clone()
            }
        }
        _ => match after {
            Some(now) => Trust::Sighted {
                clock: now.clock,
                at: now.latest,
            },
            None => Trust::Sighted {
                clock: String::new(),
                at: u64::MAX,
            },
        },
    };
    Ok(RecipeFile {
        path: path.to_owned(),
        content,
        stat,
        trust,
    })
}

/// One reading of our own clock: the true time lies in `earliest..=latest`.
struct Reading {
    /// Which count this is: the boot and the time namespace.
    clock: String,
    earliest: u64,
    latest: u64,
}

#[cfg(test)]
thread_local! {
    /// A test's clock, in nanoseconds of one count; the real clock when unset.
    pub(crate) static CLOCK: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
    /// A test's (seconds, nanoseconds) for every observed mtime and ctime, so a
    /// test makes two stats collide on purpose.
    pub(crate) static TIMES: std::cell::Cell<Option<(i64, i64)>> =
        const { std::cell::Cell::new(None) };
    /// Runs between a sighting's read and its classification.
    pub(crate) static AFTER_READ: std::cell::RefCell<Option<Box<dyn FnMut()>>> =
        const { std::cell::RefCell::new(None) };
    /// A test's raw boot id and time namespace link, in place of /proc's.
    pub(crate) static SOURCE: std::cell::Cell<Option<(Option<&'static str>, Option<&'static str>)>> =
        const { std::cell::Cell::new(None) };
    /// How many files this thread has read for their bytes.
    pub(crate) static READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// CLOCK_BOOTTIME through /proc/uptime (10 ms resolution): never stepped,
/// counts suspend, restarts at boot. `None` when it cannot be read, and then
/// no record is ever promoted.
fn clock() -> Option<Reading> {
    static COUNT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    let read = || {
        let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok();
        let namespace = std::fs::read_link("/proc/self/ns/time")
            .ok()
            .and_then(|link| link.into_os_string().into_string().ok());
        clock_identity(boot.as_deref(), namespace.as_deref())
    };
    #[cfg(test)]
    let count = match SOURCE.get() {
        Some((boot, namespace)) => clock_identity(boot, namespace),
        None => COUNT.get_or_init(read).clone(),
    }?;
    #[cfg(not(test))]
    let count = COUNT.get_or_init(read).clone()?;
    #[cfg(test)]
    if let Some(now) = CLOCK.get() {
        return Some(Reading {
            clock: count,
            earliest: now,
            latest: now,
        });
    }
    let uptime = std::fs::read_to_string("/proc/uptime").ok()?;
    let (seconds, hundredths) = uptime.split_whitespace().next()?.split_once('.')?;
    let at =
        seconds.parse::<u64>().ok()? * 1_000_000_000 + hundredths.parse::<u64>().ok()? * 10_000_000;
    Some(Reading {
        clock: count,
        earliest: at,
        latest: at + 10_000_000,
    })
}

/// Which count a clock reading belongs to: a boot id (a UUID) and a time
/// namespace (`time:[inode]`). Anything else is no clock.
fn clock_identity(boot: Option<&str>, namespace: Option<&str>) -> Option<String> {
    let boot = boot?.trim();
    let uuid = boot.len() == 36
        && boot.char_indices().all(|(at, c)| {
            if matches!(at, 8 | 13 | 18 | 23) {
                c == '-'
            } else {
                c.is_ascii_hexdigit()
            }
        });
    let inode = namespace?.strip_prefix("time:[")?.strip_suffix(']')?;
    let namespace = !inode.is_empty() && inode.bytes().all(|b| b.is_ascii_digit());
    (uuid && namespace).then(|| format!("{boot} time:[{inode}]"))
}

fn recipe_stat(path: &Path) -> io::Result<RecipeStat> {
    Ok(stat_of(&path.metadata()?))
}

fn stat_of(metadata: &std::fs::Metadata) -> RecipeStat {
    #[allow(unused_mut)]
    let mut stat = RecipeStat {
        dev: metadata.dev(),
        ino: metadata.ino(),
        len: metadata.len(),
        mode: metadata.mode(),
        mtime: metadata.mtime(),
        mtime_nsec: metadata.mtime_nsec(),
        ctime: metadata.ctime(),
        ctime_nsec: metadata.ctime_nsec(),
    };
    #[cfg(test)]
    if let Some((seconds, nanoseconds)) = TIMES.get() {
        (stat.mtime, stat.mtime_nsec, stat.ctime, stat.ctime_nsec) =
            (seconds, nanoseconds, seconds, nanoseconds);
    }
    stat
}

fn driver_file_key(path: &Path) -> io::Result<String> {
    let hash = ContentHash::of_parts(
        "hirsel.link-driver.v1",
        [
            crate::encode(&path.canonicalize()?)?.as_slice(),
            &std::fs::read(path)?,
        ],
    );
    Ok(hash.0.iter().map(|b| format!("{b:02x}")).collect())
}

fn is_c_source(path: &str) -> bool {
    [".c", ".cc", ".cpp", ".cxx", ".S", ".s"]
        .iter()
        .any(|suffix| path.ends_with(suffix))
}

pub(crate) fn missing_recipe_inputs(recipe: &LinkRecipe) -> Vec<PathBuf> {
    let mut missing = Vec::new();
    let mut expects_path = false;
    let mut skips_path = false;
    for arg in &recipe.args {
        if skips_path {
            skips_path = false;
            continue;
        }
        if expects_path {
            if !Path::new(arg).exists() {
                missing.push(PathBuf::from(arg));
            }
            expects_path = false;
            continue;
        }
        if arg == "-dynamic-linker" {
            // This is the target's runtime interpreter, not a host-side link
            // input; it is intentionally allowed to be absent on a cross
            // linker host.
            skips_path = true;
        } else if arg == "--sysroot" {
            expects_path = true;
        } else if let Some(path) = arg.strip_prefix("--sysroot=") {
            if !path.is_empty() && !Path::new(path).exists() {
                missing.push(PathBuf::from(path));
            }
        } else if arg != INPUT_MARKER
            && !arg.starts_with('-')
            && arg.contains('/')
            && !Path::new(arg).exists()
        {
            missing.push(PathBuf::from(arg));
        }
    }
    for input in recipe.files.iter().chain(&recipe.source_files) {
        if !input.path.is_file() {
            missing.push(input.path.clone());
        }
    }
    missing.sort();
    missing.dedup();
    missing
}

fn shell_words(line: &str) -> io::Result<Vec<String>> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    for ch in line.chars() {
        match (quote, ch) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => current.push(c),
            (None, '\'' | '"') => quote = Some(ch),
            (None, c) if c.is_whitespace() => {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
            }
            (None, c) => current.push(c),
        }
    }
    if quote.is_some() {
        return Err(io::Error::other("unterminated quote in link recipe"));
    }
    if !current.is_empty() {
        words.push(current);
    }
    Ok(words)
}

#[cfg(test)]
mod tests;
