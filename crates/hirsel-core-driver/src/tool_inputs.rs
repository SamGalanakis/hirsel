//! Identity of host tools selected for linking. Selection is repeated on every
//! request, so a newly installed earlier PATH candidate cannot reuse a later one.
use crate::{PackageProduct, Store};
use hirsel_core_ids::ContentHash;
use serde::Serialize;
use std::ffi::OsStr;
use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Serialize)]
pub(crate) struct Tool {
    pub(crate) path: PathBuf,
    content: ContentHash,
}

pub(crate) const INPUT_MARKER: &str = "__HIRSEL_LINK_INPUTS__";
const LINK_RECIPE_SCHEMA: u32 = 4;
const TOOLCHAIN_SCHEMA: u32 = 1;
#[derive(Clone, Debug, Serialize, serde::Deserialize, PartialEq, Eq)]
pub(crate) struct RecipeFile {
    pub(crate) path: PathBuf,
    pub(crate) content: ContentHash,
    pub(crate) stat: RecipeStat,
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
    pub(crate) identity: ContentHash,
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
    selected(program, &std::env::var_os("PATH").unwrap_or_default())
}

/// Where the driver's link recipe and toolchain manifest live: beside its
/// toolchain when that tree is ours to write, else in the user's cache (a
/// system `/usr/bin/cc` has an unwritable `/usr`). The driver key names the
/// files, so both homes hold distinct entries for distinct drivers.
fn recipe_directory(driver: &Path) -> io::Result<PathBuf> {
    if let Some(root) = driver.parent().and_then(Path::parent) {
        let beside = root.join(".hirsel-link-recipes");
        if std::fs::create_dir_all(&beside).is_ok() && tempfile::tempfile_in(&beside).is_ok() {
            return Ok(beside);
        }
    }
    let cache = PackageProduct::default_cache().join("link-recipes");
    std::fs::create_dir_all(&cache)?;
    Ok(cache)
}

fn identity(program: &Path, search: &OsStr, store: &Store) -> io::Result<Tool> {
    let path = selected(program, search)?;
    let m = path.metadata()?;
    let revision = (
        &path,
        m.dev(),
        m.ino(),
        m.len(),
        m.mode(),
        m.mtime(),
        m.mtime_nsec(),
        m.ctime(),
        m.ctime_nsec(),
    );
    let key = ContentHash::of_parts(
        "hirsel.host-tool-revision.v1",
        [crate::encode(&revision)?.as_slice()],
    );
    let selection = format!(
        "tool-{}",
        key.0.iter().map(|b| format!("{b:02x}")).collect::<String>()
    );
    let cached = store
        .selection(&selection)?
        .map(|hash| store.get(hash))
        .transpose()?
        .flatten();
    let content = if let Some(bytes) = cached.filter(|bytes| bytes.len() == 32) {
        ContentHash(
            bytes
                .as_slice()
                .try_into()
                .expect("external digest length checked"),
        )
    } else {
        let content = ContentHash::of_parts(
            "hirsel.host-tool-content.v1",
            [std::fs::read(&path)?.as_slice()],
        );
        let hash = store.put(&content.0)?;
        store.select(&selection, hash)?;
        content
    };
    Ok(Tool { path, content })
}
pub(crate) fn link_tools(cc: &Path) -> io::Result<(Tool, Tool)> {
    let search = std::env::var_os("PATH").unwrap_or_default();
    let store = Store::new(&PackageProduct::default_cache().join("host-tools"))?;
    let driver = identity(cc, &search, &store)?;
    // Resolve the pinned LLVM link editor rather than the host `ld`.  The
    // absolute path is part of the tool identity and is passed back to clang
    // at link time, so a host linker cannot silently enter the cache.
    let result = Command::new(&driver.path)
        .arg("-print-prog-name=ld.lld")
        .output()?;
    if !result.status.success() {
        return Err(io::Error::other(format!(
            "cannot select pinned link editor through {}: {}",
            driver.path.display(),
            String::from_utf8_lossy(&result.stderr)
        )));
    }
    let linker = std::str::from_utf8(&result.stdout)
        .map_err(io::Error::other)?
        .trim();
    if linker.is_empty() || linker.contains('\n') {
        return Err(io::Error::other(
            "C driver did not name one pinned link editor",
        ));
    }
    Ok((driver, identity(Path::new(linker), &search, &store)?))
}

/// Capture the link editor command once beside the selected toolchain. The
/// capture is the only C-driver use in the direct-link path; subsequent builds
/// invoke the recorded lld argv themselves.
pub(crate) fn link_recipe(cc: &Path, target: &str) -> io::Result<LinkRecipe> {
    capture_link_recipe(cc, target, false)
}

/// Recapture is reserved for a recipe whose previously recorded toolchain
/// input disappeared.  Ordinary builds never invoke the C driver.
pub(crate) fn recapture_link_recipe(cc: &Path, target: &str) -> io::Result<LinkRecipe> {
    capture_link_recipe(cc, target, true)
}

/// One cached identity for every toolchain input. Stat signatures avoid
/// re-reading unchanged runtime, linker and CRT bytes on every request. The
/// recipe belongs to the C driver and target alone; the runtime archive is
/// this request's own input, so one recipe serves every runtime.
pub(crate) fn toolchain_identity(
    cc: &Path,
    target: &str,
    runtime: &Path,
) -> io::Result<ContentHash> {
    let recipe = link_recipe(cc, target)?;
    let root = recipe_directory(cc)?;
    let key = driver_file_key(cc)?;
    let path = root.join(format!("toolchain-{target}-{key}.json"));
    let mut paths = vec![cc.canonicalize()?, runtime.to_owned()];
    if let Some(lib) = find_llvm_library(cc) {
        paths.push(lib);
    }
    paths.extend(recipe.files.iter().map(|file| file.path.clone()));
    paths.sort();
    paths.dedup();
    let old = std::fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<ToolchainManifest>(&bytes).ok())
        .filter(|manifest| manifest.schema == TOOLCHAIN_SCHEMA && manifest.target == target);
    let mut files = Vec::with_capacity(paths.len());
    for file in paths {
        if let Some(previous) = old
            .as_ref()
            .and_then(|manifest| manifest.files.iter().find(|entry| entry.path == file))
        {
            if recipe_stat(&file)? == previous.stat {
                files.push(previous.clone());
                continue;
            }
        }
        files.push(recipe_file(&file)?);
    }
    let identity = ContentHash::of_parts(
        "hirsel.toolchain-image.v2",
        [crate::encode(&(TOOLCHAIN_SCHEMA, target, image(&files)))?.as_slice()],
    );
    // A moved stat over equal bytes is a change to the record: rewrite it so
    // the next request reads nothing. An unchanged record is not rewritten.
    if old.is_none_or(|old| old.files != files) {
        let manifest = ToolchainManifest {
            schema: TOOLCHAIN_SCHEMA,
            target: target.into(),
            files,
            identity,
        };
        publish(&root, &path, &manifest)?;
    }
    Ok(identity)
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

fn capture_link_recipe(cc: &Path, target: &str, force: bool) -> io::Result<LinkRecipe> {
    let mut driver = cc.to_owned();
    let mut directory = recipe_directory(&driver)?;
    let mut driver_key = driver_file_key(&driver)?;
    let mut path = directory.join(format!("{target}-{driver_key}.json"));
    if !force
        && let Ok(bytes) = std::fs::read(&path)
        && let Ok(mut recipe) = serde_json::from_slice::<LinkRecipe>(&bytes)
        && recipe.schema == LINK_RECIPE_SCHEMA
        && recipe.target == target
        && recipe.linker.is_file()
    {
        match refresh_recipe_inputs(&mut recipe)? {
            RecipeInputs::Current => return Ok(recipe),
            RecipeInputs::Restated => {
                publish(&directory, &path, &recipe)?;
                return Ok(recipe);
            }
            RecipeInputs::Changed => {}
        }
    }
    let capture_dir = tempfile::tempdir_in(&directory)?;
    let mut synthetic_source = capture_dir.path().join("recipe.c");
    std::fs::write(&synthetic_source, "int main(void) { return 0; }\n")?;
    let capture = |driver: &Path, source: &Path, output: &Path| {
        let mut command = Command::new(driver);
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
    let mut output = capture(
        &driver,
        &synthetic_source,
        &capture_dir.path().join("recipe"),
    )?;
    if !output.status.success()
        && let Some(prefix) = std::env::var_os("LLVM_SYS_231_PREFIX")
    {
        let candidate = PathBuf::from(prefix).join("bin/clang");
        if candidate.is_file() && candidate != driver {
            driver = candidate;
            directory = recipe_directory(&driver)?;
            driver_key = driver_file_key(&driver)?;
            path = directory.join(format!("{target}-{driver_key}.json"));
            let fallback_source = capture_dir.path().join("recipe-fallback.c");
            std::fs::write(&fallback_source, "int main(void) { return 0; }\n")?;
            output = capture(
                &driver,
                &fallback_source,
                &capture_dir.path().join("recipe"),
            )?;
            synthetic_source = fallback_source;
        }
    }
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
        let stable = directory.join(format!("extra-{driver_key}-{index}.o"));
        let mut command = Command::new(&words[0]);
        let mut args = words[1..].to_vec();
        if let Some(output) = args.iter_mut().position(|word| word == "-o") {
            args[output + 1] = stable.display().to_string();
        }
        let status = command.args(&args).status()?;
        if !status.success() {
            return Err(io::Error::other(format!(
                "compiling link source {} failed",
                source
            )));
        }
        compiled_objects.insert(transient_output, stable.clone());
        let source = PathBuf::from(source);
        source_files.push(recipe_file(&source)?);
    }
    let linker = if argv.first().is_some_and(|arg| arg.contains("collect2")) {
        argv.remove(0);
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
        link_tools(cc)?.1.path
    } else {
        let linker = PathBuf::from(argv.remove(0));
        if linker.is_file() {
            linker
        } else if linker.is_absolute() || linker.components().count() > 1 {
            return Err(io::Error::other(format!(
                "missing link editor input: {}",
                linker.display()
            )));
        } else {
            link_tools(cc)?.1.path
        }
    };
    let mut args = Vec::with_capacity(argv.len() + 1);
    let mut marker = false;
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
        let transient = arg.ends_with(".o")
            && Path::new(arg)
                .file_stem()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("--") || !Path::new(arg).exists());
        if transient {
            if !marker {
                args.push(INPUT_MARKER.into());
                marker = true;
            }
        } else {
            args.push(arg.clone());
        }
        i += 1;
    }
    if !marker {
        let at = args
            .iter()
            .position(|arg| arg == "-lc" || arg == "--as-needed")
            .unwrap_or(args.len());
        args.insert(at, INPUT_MARKER.into());
    }
    let files = recipe_files(&linker, &args)?;
    let identity = recipe_identity(target, &driver_key, &linker, &args, &files, &source_files)?;
    let recipe = LinkRecipe {
        schema: LINK_RECIPE_SCHEMA,
        target: target.into(),
        linker,
        args,
        files,
        source_files,
        identity,
    };
    publish(&directory, &path, &recipe)?;
    Ok(recipe)
}

fn recipe_files(linker: &Path, args: &[String]) -> io::Result<Vec<RecipeFile>> {
    let mut paths = vec![linker.to_owned()];
    let mut skip_dynamic_linker = false;
    for arg in args {
        if skip_dynamic_linker {
            skip_dynamic_linker = false;
            continue;
        }
        if arg == "-dynamic-linker" {
            skip_dynamic_linker = true;
            continue;
        }
        if arg != INPUT_MARKER && !arg.starts_with('-') && arg.contains('/') {
            paths.push(PathBuf::from(arg));
        }
    }
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
    paths.into_iter().map(|path| recipe_file(&path)).collect()
}

fn recipe_identity(
    target: &str,
    driver_key: &str,
    linker: &Path,
    args: &[String],
    files: &[RecipeFile],
    source_files: &[RecipeFile],
) -> io::Result<ContentHash> {
    Ok(ContentHash::of_parts(
        "hirsel.direct-link-recipe.v2",
        [crate::encode(&(
            LINK_RECIPE_SCHEMA,
            target,
            driver_key,
            linker,
            args,
            image(files),
            image(source_files),
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
    for input in recipe.files.iter_mut().chain(&mut recipe.source_files) {
        let Ok(stat) = recipe_stat(&input.path) else {
            return Ok(RecipeInputs::Changed);
        };
        if stat == input.stat {
            continue;
        }
        let Ok(bytes) = std::fs::read(&input.path) else {
            return Ok(RecipeInputs::Changed);
        };
        if ContentHash::of_bytes(&bytes) != input.content {
            return Ok(RecipeInputs::Changed);
        }
        input.stat = stat;
        inputs = RecipeInputs::Restated;
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
    let bytes = std::fs::read(path)?;
    Ok(RecipeFile {
        path: path.to_owned(),
        content: ContentHash::of_bytes(&bytes),
        stat: recipe_stat(path)?,
    })
}

fn recipe_stat(path: &Path) -> io::Result<RecipeStat> {
    let metadata = path.metadata()?;
    Ok(RecipeStat {
        dev: metadata.dev(),
        ino: metadata.ino(),
        len: metadata.len(),
        mode: metadata.mode(),
        mtime: metadata.mtime(),
        mtime_nsec: metadata.mtime_nsec(),
        ctime: metadata.ctime(),
        ctime_nsec: metadata.ctime_nsec(),
    })
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
mod tests {
    use super::*;
    #[test]
    fn earlier_path_candidate_and_replaced_tool_change_selection() {
        let work = tempfile::tempdir().unwrap();
        let store = Store::new(&work.path().join("cache")).unwrap();
        let first = work.path().join("first");
        let second = work.path().join("second");
        std::fs::create_dir(&first).unwrap();
        std::fs::create_dir(&second).unwrap();
        let write = |p: &Path| {
            std::fs::write(p, b"tool").unwrap();
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        write(&second.join("cc"));
        let search = std::env::join_paths([&first, &second]).unwrap();
        let initial = identity(Path::new("cc"), &search, &store).unwrap();
        assert_eq!(initial.path, second.join("cc"));
        write(&first.join("cc"));
        let earlier = identity(Path::new("cc"), &search, &store).unwrap();
        assert_eq!(earlier.path, first.join("cc"));
        let replacement = first.join("new-cc");
        write(&replacement);
        std::fs::write(&replacement, b"changed").unwrap();
        std::fs::rename(replacement, first.join("cc")).unwrap();
        let replaced = identity(Path::new("cc"), &search, &store).unwrap();
        assert_eq!(earlier.path, replaced.path);
        assert_ne!(earlier.content, replaced.content);
    }

    #[test]
    fn a_driver_whose_toolchain_tree_is_read_only_still_links() {
        // A system driver (/usr/bin/cc) has a toolchain root nobody but root may write.
        let work = tempfile::tempdir().unwrap();
        let tree = work.path().join("usr");
        let bin = tree.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let linker = work.path().join("ld.lld");
        let crt = work.path().join("crt1.o");
        std::fs::write(&linker, b"linker").unwrap();
        std::fs::write(&crt, b"crt").unwrap();
        let cc = bin.join("cc");
        std::fs::write(
            &cc,
            format!(
                "#!/bin/sh\necho '\"{}\" \"-pie\" \"-o\" \"out\" \"{}\" \"-lc\"' >&2\n",
                linker.display(),
                crt.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&cc, std::fs::Permissions::from_mode(0o755)).unwrap();
        let runtime = work.path().join("runtime.a");
        std::fs::write(&runtime, b"runtime").unwrap();
        let read_only =
            |mode| std::fs::set_permissions(&tree, std::fs::Permissions::from_mode(mode));
        read_only(0o555).unwrap();
        let target = "x86_64-unknown-linux-gnu";
        let key = driver_file_key(&cc).unwrap();
        let identity = toolchain_identity(&cc, target, &runtime);
        let recipe = link_recipe(&cc, target);
        read_only(0o755).unwrap();
        let cache = PackageProduct::default_cache().join("link-recipes");
        for name in [
            format!("{target}-{key}.json"),
            format!("toolchain-{target}-{key}.json"),
        ] {
            let _ = std::fs::remove_file(cache.join(name));
        }
        identity.unwrap();
        assert_eq!(recipe.unwrap().linker, linker);
    }

    #[test]
    fn a_rematerialised_toolchain_file_is_read_once() {
        // Buck re-links toolchain files: same bytes, new inode and ctime. The request
        // that sees it re-hashes once and records the new stat; later requests read
        // nothing and rewrite nothing.
        let work = tempfile::tempdir().unwrap();
        let bin = work.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let linker = bin.join("ld.lld");
        let crt = work.path().join("crt1.o");
        std::fs::write(&linker, b"linker").unwrap();
        std::fs::write(&crt, b"crt").unwrap();
        let cc = bin.join("cc");
        std::fs::write(
            &cc,
            format!(
                "#!/bin/sh\necho '\"{}\" \"-pie\" \"-o\" \"out\" \"{}\" \"-lc\"' >&2\n",
                linker.display(),
                crt.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&cc, std::fs::Permissions::from_mode(0o755)).unwrap();
        let runtime = work.path().join("runtime.a");
        std::fs::write(&runtime, b"runtime").unwrap();
        let target = "x86_64-unknown-linux-gnu";
        let identity = toolchain_identity(&cc, target, &runtime).unwrap();
        for file in [&linker, &crt, &runtime] {
            let copy = file.with_extension("rematerialised");
            std::fs::copy(file, &copy).unwrap();
            std::fs::rename(&copy, file).unwrap();
        }
        assert_eq!(toolchain_identity(&cc, target, &runtime).unwrap(), identity);
        let directory = recipe_directory(&cc).unwrap();
        let key = driver_file_key(&cc).unwrap();
        let recipe: LinkRecipe = serde_json::from_slice(
            &std::fs::read(directory.join(format!("{target}-{key}.json"))).unwrap(),
        )
        .unwrap();
        let manifest_path = directory.join(format!("toolchain-{target}-{key}.json"));
        let manifest: ToolchainManifest =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        for file in recipe.files.iter().chain(&manifest.files) {
            assert_eq!(
                file.stat,
                recipe_stat(&file.path).unwrap(),
                "{} keeps a stat the next request must re-hash",
                file.path.display()
            );
        }
        let written = std::fs::metadata(&manifest_path).unwrap().ino();
        assert_eq!(toolchain_identity(&cc, target, &runtime).unwrap(), identity);
        assert_eq!(
            std::fs::metadata(&manifest_path).unwrap().ino(),
            written,
            "an unchanged toolchain record is not rewritten"
        );
    }

    #[test]
    fn each_runtime_archive_keys_its_own_toolchain_identity() {
        // Two builds share one C driver but link different runtime archives (the pool and
        // openat2 host targets do). A recipe captured for one must not stand in for the other.
        let work = tempfile::tempdir().unwrap();
        let bin = work.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let linker = bin.join("ld.lld");
        let crt = work.path().join("crt1.o");
        std::fs::write(&linker, b"linker").unwrap();
        std::fs::write(&crt, b"crt").unwrap();
        let cc = bin.join("cc");
        std::fs::write(
            &cc,
            format!(
                "#!/bin/sh\necho '\"{}\" \"-pie\" \"-o\" \"out\" \"{}\" \"-lc\"' >&2\n",
                linker.display(),
                crt.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&cc, std::fs::Permissions::from_mode(0o755)).unwrap();
        let target = "x86_64-unknown-linux-gnu";
        let (first, second) = (work.path().join("first.a"), work.path().join("second.a"));
        std::fs::write(&first, b"first runtime").unwrap();
        std::fs::write(&second, b"second runtime").unwrap();
        let first_identity = toolchain_identity(&cc, target, &first).unwrap();
        let second_identity = toolchain_identity(&cc, target, &second).unwrap();
        assert_ne!(
            first_identity, second_identity,
            "different runtime bytes link differently"
        );
        std::fs::write(&second, b"second runtime, rebuilt").unwrap();
        assert_ne!(
            toolchain_identity(&cc, target, &second).unwrap(),
            second_identity,
            "a rebuilt runtime archive must miss every object keyed by the old one"
        );
        assert_eq!(
            toolchain_identity(&cc, target, &first).unwrap(),
            first_identity
        );
    }

    #[test]
    fn recipe_input_stat_change_rehashes_only_when_content_changes() {
        let work = tempfile::tempdir().unwrap();
        let input = work.path().join("crt.o");
        std::fs::write(&input, b"same bytes").unwrap();
        let mut recipe = LinkRecipe {
            schema: LINK_RECIPE_SCHEMA,
            target: "x86_64-unknown-linux-gnu".into(),
            linker: input.clone(),
            args: vec![INPUT_MARKER.into()],
            files: vec![recipe_file(&input).unwrap()],
            source_files: vec![],
            identity: ContentHash::of_bytes(b"recipe"),
        };
        assert_eq!(
            refresh_recipe_inputs(&mut recipe).unwrap(),
            RecipeInputs::Current
        );
        std::fs::write(&input, b"same bytes").unwrap();
        assert_eq!(
            refresh_recipe_inputs(&mut recipe).unwrap(),
            RecipeInputs::Restated,
            "touch-only replacement must reuse"
        );
        std::fs::write(&input, b"changed bytes").unwrap();
        assert_eq!(
            refresh_recipe_inputs(&mut recipe).unwrap(),
            RecipeInputs::Changed,
            "changed content must miss"
        );
    }
}
