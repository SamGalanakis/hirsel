//! The ld64.lld inputs of a captured Mach-O recipe.
use super::{Closure, missing};
use object::read::macho::{LoadCommandVariant, MachHeader as _};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

const DYLIB_EXTS: &[&str] = &[".tbd", ".dylib", ".so"];

#[derive(Clone, Copy)]
enum Input<'a> {
    Path(&'a str),
    Library(&'a str),
    Framework(&'a str),
    Data(&'a str),
    List(&'a str),
}

fn refuse(what: impl std::fmt::Display) -> io::Error {
    io::Error::other(format!("link recipe refused: {what}"))
}

pub(super) fn closure(args: &[String], marker: &str) -> io::Result<Closure> {
    let mut inputs = Vec::new();
    let (mut roots, mut libraries, mut frameworks) = (Vec::new(), Vec::new(), Vec::new());
    let (mut standard_paths, mut dylibs_first) = (true, false);
    let mut args = args.iter().map(String::as_str);
    while let Some(arg) = args.next() {
        if arg == marker {
            continue;
        }
        let mut value = || {
            args.next()
                .ok_or_else(|| refuse(format!("{arg} has no operand")))
        };
        match arg {
            "-syslibroot" => roots.push(absolute(value()?)?),
            "-L" => libraries.push(value()?),
            "-F" => frameworks.push(value()?),
            "-Z" => standard_paths = false,
            "-search_dylibs_first" => dylibs_first = true,
            "-search_paths_first" => dylibs_first = false,
            "-l" => inputs.push(Input::Library(value()?)),
            "-framework" | "-weak_framework" | "-needed_framework" | "-reexport_framework" => {
                inputs.push(Input::Framework(value()?));
            }
            "-force_load" | "-load_hidden" | "-weak_library" | "-needed_library"
            | "-reexport_library" => inputs.push(Input::Path(value()?)),
            "-filelist" => inputs.push(Input::List(value()?)),
            "-order_file" | "-exported_symbols_list" | "-unexported_symbols_list" => {
                inputs.push(Input::Data(value()?));
            }
            "-sectcreate" | "-segcreate" => {
                value()?;
                value()?;
                inputs.push(Input::Data(value()?));
            }
            "-platform_version" => {
                value()?;
                value()?;
                value()?;
            }
            "-mllvm" => match value()? {
                "-enable-linkonceodr-outlining"
                | "-enable-machine-outliner"
                | "-enable-machine-outliner=never" => {}
                option => return Err(refuse(format!("unknown Mach-O -mllvm option {option}"))),
            },
            "-arch"
            | "-flavor"
            | "-macos_version_min"
            | "-iphoneos_version_min"
            | "-ios_simulator_version_min"
            | "-e"
            | "-u"
            | "-rpath"
            | "-undefined"
            | "-exported_symbol"
            | "-unexported_symbol" => {
                value()?;
            }
            "-demangle" | "-no_deduplicate" | "-dynamic" | "-pie" | "-no_pie"
            | "-adhoc_codesign" | "-dead_strip" | "-dead_strip_dylibs" | "-all_load" | "-ObjC"
            | "-S" | "-s" => {}
            "-lto_library" | "-lazy_library" | "-lazy_framework" => {
                return Err(refuse(format!("unsupported Mach-O link option {arg}")));
            }
            _ if arg.starts_with('@') => return Err(refuse(format!("response file {arg}"))),
            _ if !arg.starts_with('-') => inputs.push(Input::Path(arg)),
            _ => {
                if let Some(dir) = arg.strip_prefix("-L") {
                    libraries.push(dir);
                } else if let Some(dir) = arg.strip_prefix("-F") {
                    frameworks.push(dir);
                } else if let Some(name) =
                    ["-l", "-weak-l", "-needed-l", "-reexport-l", "-hidden-l"]
                        .iter()
                        .find_map(|prefix| arg.strip_prefix(prefix))
                {
                    inputs.push(Input::Library(name));
                } else {
                    return Err(refuse(format!("unknown Mach-O link option {arg}")));
                }
            }
        }
    }
    if roots.last().is_some_and(|root| *root == Path::new("/")) {
        roots.clear();
    }
    let mut walk = Walk {
        roots,
        libraries: Vec::new(),
        frameworks: Vec::new(),
        result: Closure {
            files: Vec::new(),
            binaries: Vec::new(),
            absent: Vec::new(),
            directories: Vec::new(),
        },
    };
    for root in &walk.roots {
        // Clang reads these settings to select the SDK version in its argv.
        for name in ["SDKSettings.json", "SDKSettings.plist"] {
            let path = root.join(name);
            if path.is_file() {
                walk.result.files.push(path);
            } else if !path.exists() {
                walk.result.absent.push(path);
            }
        }
    }
    walk.libraries = walk.search(
        libraries,
        standard_paths.then_some(["/usr/lib", "/usr/local/lib"]),
    )?;
    walk.frameworks = walk.search(
        frameworks,
        standard_paths.then_some(["/Library/Frameworks", "/System/Library/Frameworks"]),
    )?;
    for input in inputs {
        match input {
            Input::Path(path) => walk.input(path)?,
            Input::Library(name) => {
                let path = walk
                    .library(name, dylibs_first)
                    .ok_or_else(|| missing(format!("-l{name}")))?;
                walk.binary(path)?;
            }
            Input::Framework(name) => {
                let path = walk
                    .framework(name)?
                    .ok_or_else(|| missing(format!("-framework {name}")))?;
                walk.binary(path)?;
            }
            Input::Data(path) => walk.read(absolute(path)?)?,
            Input::List(path) => {
                let path = absolute(path)?;
                let list = std::fs::read_to_string(&path)?;
                walk.read(path)?;
                for line in list
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty() && !line.starts_with('#'))
                {
                    walk.input(line)?;
                }
            }
        }
    }
    for paths in [
        &mut walk.result.files,
        &mut walk.result.binaries,
        &mut walk.result.absent,
        &mut walk.result.directories,
    ] {
        paths.sort();
        paths.dedup();
    }
    Ok(walk.result)
}

fn absolute(path: &str) -> io::Result<PathBuf> {
    let path = PathBuf::from(path);
    if path.is_absolute() {
        Ok(path)
    } else {
        Err(refuse(format!(
            "relative Mach-O link input {}",
            path.display()
        )))
    }
}

struct Walk {
    roots: Vec<PathBuf>,
    libraries: Vec<PathBuf>,
    frameworks: Vec<PathBuf>,
    result: Closure,
}

impl Walk {
    fn search(&mut self, paths: Vec<&str>, system: Option<[&str; 2]>) -> io::Result<Vec<PathBuf>> {
        let mut result = Vec::new();
        for path in paths {
            let plain = absolute(path)?;
            let before = result.len();
            for root in &self.roots {
                add_directory(
                    root.join(path.trim_start_matches('/')),
                    &mut result,
                    &mut self.result,
                )?;
            }
            if result.len() == before {
                add_directory(plain, &mut result, &mut self.result)?;
            }
        }
        for path in system.into_iter().flatten() {
            if self.roots.is_empty() {
                add_directory(path.into(), &mut result, &mut self.result)?;
            }
            for root in &self.roots {
                add_directory(
                    root.join(path.trim_start_matches('/')),
                    &mut result,
                    &mut self.result,
                )?;
            }
        }
        Ok(result)
    }

    fn library(&mut self, name: &str, dylibs_first: bool) -> Option<PathBuf> {
        let (stem, exts) = if name.ends_with(".o") {
            (name.to_owned(), &[""][..])
        } else if dylibs_first {
            (format!("lib{name}"), DYLIB_EXTS)
        } else {
            (format!("lib{name}"), &[".tbd", ".dylib", ".so", ".a"][..])
        };
        let found = variants(&mut self.result.absent, &self.libraries, &stem, exts);
        if found.is_some() || !dylibs_first || name.ends_with(".o") {
            found
        } else {
            variants(&mut self.result.absent, &self.libraries, &stem, &[".a"])
        }
    }

    fn framework(&mut self, name: &str) -> io::Result<Option<PathBuf>> {
        if name.contains(',') {
            return Err(refuse(format!("suffixed Mach-O framework {name}")));
        }
        Ok(self.frameworks.iter().find_map(|dir| {
            let path = dir.join(format!("{name}.framework/{name}"));
            stub_or_binary(&mut self.result.absent, path)
        }))
    }

    fn input(&mut self, path: &str) -> io::Result<()> {
        let plain = absolute(path)?;
        let path = if path.ends_with(".o") {
            plain
        } else {
            first(
                &mut self.result.absent,
                self.roots
                    .iter()
                    .map(|root| root.join(path.trim_start_matches('/'))),
            )
            .unwrap_or(plain)
        };
        self.binary(path)
    }

    fn read(&mut self, path: PathBuf) -> io::Result<()> {
        if !path.is_file() {
            return Err(missing(path.display()));
        }
        self.result.files.push(path);
        Ok(())
    }

    fn binary(&mut self, path: PathBuf) -> io::Result<()> {
        if self.result.binaries.contains(&path) {
            return Ok(());
        }
        self.read(path.clone())?;
        self.result.binaries.push(path.clone());
        let mut file = std::fs::File::open(&path)?;
        let mut head = [0; 16];
        let size = file.read(&mut head)?;
        if head.starts_with(b"!<thin>\n") {
            return Err(refuse(format!(
                "thin archive {}, whose members are other files",
                path.display()
            )));
        }
        if head.starts_with(b"---") || head.starts_with(b"{") {
            self.stub(&path)?;
        } else if let Ok(kind) = object::FileKind::parse(&head[..size])
            && matches!(
                kind,
                object::FileKind::MachO64
                    | object::FileKind::MachOFat32
                    | object::FileKind::MachOFat64
            )
        {
            self.macho(&path)?;
        }
        Ok(())
    }

    fn macho(&mut self, path: &Path) -> io::Result<()> {
        use object::macho::{LC_LINKER_OPTION, LC_REEXPORT_DYLIB, MH_DYLIB, MH_OBJECT};
        let data = std::fs::read(path)?;
        let malformed =
            |error: object::Error| refuse(format!("Mach-O input {}: {error}", path.display()));
        let image = arm64_slice(&data)
            .ok_or_else(|| refuse(format!("no readable arm64 slice in {}", path.display())))?;
        // Clang ships its compiler runtime as a universal archive.
        if image.starts_with(b"!<arch>\n") {
            return Ok(());
        }
        if image.starts_with(b"!<thin>\n") {
            return Err(refuse(format!("thin archive slice in {}", path.display())));
        }
        let header = object::macho::MachHeader64::<object::Endianness>::parse(image, 0)
            .map_err(malformed)?;
        let endian = header.endian().map_err(malformed)?;
        let file_type = header.filetype(endian);
        let mut commands = header.load_commands(endian, image, 0).map_err(malformed)?;
        while let Some(command) = commands.next().map_err(malformed)? {
            match command.cmd() {
                LC_LINKER_OPTION if file_type == MH_OBJECT => {
                    return Err(refuse(format!(
                        "autolink directive in Mach-O recipe input {}",
                        path.display()
                    )));
                }
                LC_REEXPORT_DYLIB if file_type == MH_DYLIB => {
                    let LoadCommandVariant::Dylib(dylib) = command.variant().map_err(malformed)?
                    else {
                        return Err(refuse(format!("malformed reexport in {}", path.display())));
                    };
                    let name = command
                        .string(endian, dylib.dylib.name)
                        .map_err(malformed)?;
                    let name = std::str::from_utf8(name)
                        .map_err(|_| refuse("non-UTF-8 Mach-O reexport"))?;
                    let input = self.reexport(name)?.ok_or_else(|| missing(name))?;
                    self.binary(input)?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn stub(&mut self, path: &Path) -> io::Result<()> {
        let text = std::fs::read_to_string(path)?;
        for name in super::macho_stub::reexports(&text)? {
            if let Some(input) = self.reexport(&name)? {
                self.binary(input)?;
            }
        }
        Ok(())
    }

    fn reexport(&mut self, name: &str) -> io::Result<Option<PathBuf>> {
        let path = Path::new(name);
        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| refuse(format!("invalid reexport {name}")))?;
        let framework = format!("/{stem}.framework/");
        if let Some((_, rest)) = name.rsplit_once(&framework) {
            for dir in &self.frameworks {
                let candidate = dir.join(format!("{stem}.framework/{rest}"));
                if let Some(found) = stub_or_binary(&mut self.result.absent, candidate) {
                    return Ok(Some(found));
                }
            }
        } else if let Some(found) =
            variants(&mut self.result.absent, &self.libraries, stem, DYLIB_EXTS)
        {
            return Ok(Some(found));
        }
        if !path.is_absolute() {
            return Err(refuse(format!("relative Mach-O reexport {name}")));
        }
        for root in &self.roots {
            if let Some(found) = stub_or_binary(
                &mut self.result.absent,
                root.join(name.trim_start_matches('/')),
            ) {
                return Ok(Some(found));
            }
        }
        // SDK stubs may define this install name in another document in the
        // same file. The already keyed stub then supplies it.
        Ok(stub_or_binary(&mut self.result.absent, path.to_owned()))
    }
}

pub(crate) fn arm64_slice(bytes: &[u8]) -> Option<&[u8]> {
    use object::read::macho::{FatArch as _, MachOFatFile32, MachOFatFile64};
    match object::FileKind::parse(bytes).ok() {
        Some(object::FileKind::MachOFat32) => MachOFatFile32::parse(bytes)
            .ok()?
            .arches()
            .iter()
            .find(|arch| arch.architecture() == object::Architecture::Aarch64)?
            .data(bytes)
            .ok(),
        Some(object::FileKind::MachOFat64) => MachOFatFile64::parse(bytes)
            .ok()?
            .arches()
            .iter()
            .find(|arch| arch.architecture() == object::Architecture::Aarch64)?
            .data(bytes)
            .ok(),
        _ => Some(bytes),
    }
}

fn add_directory(path: PathBuf, search: &mut Vec<PathBuf>, result: &mut Closure) -> io::Result<()> {
    if path.is_dir() {
        result.directories.push(path.clone());
        search.push(path);
    } else if path.exists() {
        return Err(refuse(format!(
            "Mach-O search path {} is not a directory",
            path.display()
        )));
    } else {
        result.absent.push(path);
    }
    Ok(())
}

fn first(
    absent: &mut Vec<PathBuf>,
    candidates: impl IntoIterator<Item = PathBuf>,
) -> Option<PathBuf> {
    candidates.into_iter().find(|path| {
        path.exists() || {
            absent.push(path.clone());
            false
        }
    })
}

fn variants(
    absent: &mut Vec<PathBuf>,
    dirs: &[PathBuf],
    stem: &str,
    exts: &[&str],
) -> Option<PathBuf> {
    first(
        absent,
        dirs.iter()
            .flat_map(|dir| exts.iter().map(move |ext| dir.join(format!("{stem}{ext}")))),
    )
}

fn stub_or_binary(absent: &mut Vec<PathBuf>, path: PathBuf) -> Option<PathBuf> {
    first(absent, [path.with_extension("tbd"), path])
}
