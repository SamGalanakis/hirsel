//! Identity of host tools selected for linking. Selection is repeated on every
//! request, so a newly installed earlier PATH candidate cannot reuse a later one.
use crate::{PackageProduct, Store};
use hirsel_core_ids::ContentHash;
use serde::Serialize;
use std::ffi::OsStr;
use std::io::{self, Read};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Serialize)]
pub(crate) struct Tool {
    pub(crate) path: PathBuf,
    content: ContentHash,
}

pub(crate) const INPUT_MARKER: &str = "__HIRSEL_LINK_INPUTS__";
const LINK_RECIPE_SCHEMA: u32 = 5;
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
    let content = recorded(&path, store)?.content;
    Ok(Tool { path, content })
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
pub(crate) fn link_tools(cc: &Path) -> io::Result<(Tool, Tool)> {
    let search = std::env::var_os("PATH").unwrap_or_default();
    let store = host_tools()?;
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

/// What native products are keyed by, besides the Hirsel compiler itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Toolchain {
    /// Every link input: C driver, libLLVM, linker, CRT and libc files, runtime archive.
    pub(crate) image: ContentHash,
    /// The runtime archive alone: objects are emitted in process against its
    /// ABI, so a different C driver or linker relinks them but compiles none.
    pub(crate) runtime: ContentHash,
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
    cc: &Path,
    target: &str,
    runtime: &Path,
) -> io::Result<(Toolchain, LinkRecipe)> {
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
        if let Some(seen) = recipe.files.iter().find(|entry| entry.path == file) {
            files.push(seen.clone());
            continue;
        }
        let previous = old
            .as_ref()
            .and_then(|manifest| manifest.files.iter().find(|entry| entry.path == file));
        files.push(observe(&file, previous)?);
    }
    let identity = ContentHash::of_parts(
        "hirsel.toolchain-image.v2",
        [crate::encode(&(TOOLCHAIN_SCHEMA, target, image(&files)))?.as_slice()],
    );
    let archive = files
        .iter()
        .find(|file| file.path == runtime)
        .expect("the runtime archive is a recorded toolchain input")
        .content;
    let toolchain = Toolchain {
        image: identity,
        runtime: ContentHash::of_parts("hirsel.core-runtime.v2", [archive.0.as_slice()]),
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
        publish(&root, &path, &manifest)?;
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
    // A driver that names a link editor which does not exist must not be
    // captured: clang would silently fall back to some other ld.lld.
    let selection = Command::new(&driver)
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
            // Only the synthetic source's object stands for the build's
            // own inputs. Another transient object is a source the driver
            // adds itself (a wrapper's support file) whose compile job this
            // capture could not replay (a gcc `cc1` and `as` pair rather
            // than a clang `-cc1`): folding it into the marker would drop a
            // link input and link a different program.
            if marker {
                return Err(io::Error::other(format!(
                    "link recipe capture through {} cannot replay the compile of a link source the driver adds; name a clang driver",
                    driver.display()
                )));
            }
            args.push(INPUT_MARKER.into());
            marker = true;
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
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    #[cfg(test)]
    {
        READS.set(READS.get() + 1);
        if let Some(mut after_read) = AFTER_READ.take() {
            after_read();
        }
    }
    let content = ContentHash::of_bytes(&bytes);
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
mod tests {
    use super::*;
    /// Law: LLVM keys take the loaded libLLVM's bytes, so a library one byte
    /// apart (same name and size) is another identity, and a copy the same.
    /// Oracle: the files' bytes.
    #[test]
    fn a_libllvm_one_byte_apart_is_another_llvm_identity() {
        let work = tempfile::tempdir().unwrap();
        let mut bytes = vec![0x5a; 4096];
        let file = |name: &str, bytes: &[u8]| {
            let path = work.path().join(name).join("libLLVM.so.23.1");
            std::fs::create_dir(path.parent().unwrap()).unwrap();
            std::fs::write(&path, bytes).unwrap();
            LLVM_LIBRARY.set(Some(path));
            let identity = llvm_library().unwrap();
            LLVM_LIBRARY.set(None);
            identity
        };
        let first = file("first", &bytes);
        let copy = file("copy", &bytes);
        bytes[2048] ^= 1;
        let apart = file("apart", &bytes);
        assert_eq!(first, copy);
        assert_ne!(first, apart);
    }

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
        let identity = toolchain(&cc, target, &runtime);
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
        CLOCK.set(Some(0));
        let identity = toolchain(&cc, target, &runtime).unwrap().0;
        for file in [&linker, &crt, &runtime] {
            let copy = file.with_extension("rematerialised");
            std::fs::copy(file, &copy).unwrap();
            std::fs::rename(&copy, file).unwrap();
        }
        // The new stats are sighted, then settled by a sighting a window later.
        // A request reads each moved file once, though both the recipe and the
        // toolchain record name the linker and the CRT.
        CLOCK.set(Some(1));
        READS.set(0);
        assert_eq!(toolchain(&cc, target, &runtime).unwrap().0, identity);
        assert_eq!(READS.get(), 4, "cc, linker, CRT and runtime, once each");
        CLOCK.set(Some(1 + WINDOW));
        assert_eq!(toolchain(&cc, target, &runtime).unwrap().0, identity);
        let directory = recipe_directory(&cc).unwrap();
        let key = driver_file_key(&cc).unwrap();
        let manifest_path = directory.join(format!("toolchain-{target}-{key}.json"));
        let written = std::fs::metadata(&manifest_path).unwrap().ino();
        READS.set(0);
        assert_eq!(toolchain(&cc, target, &runtime).unwrap().0, identity);
        assert_eq!(READS.get(), 0, "settled toolchain files are not read again");
        assert_eq!(
            std::fs::metadata(&manifest_path).unwrap().ino(),
            written,
            "an unchanged toolchain record is not rewritten"
        );
        CLOCK.set(None);
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
        let first_identity = toolchain(&cc, target, &first).unwrap().0;
        let second_identity = toolchain(&cc, target, &second).unwrap().0;
        assert_ne!(
            first_identity, second_identity,
            "different runtime bytes link differently"
        );
        std::fs::write(&second, b"second runtime, rebuilt").unwrap();
        assert_ne!(
            toolchain(&cc, target, &second).unwrap().0,
            second_identity,
            "a rebuilt runtime archive must miss every object keyed by the old one"
        );
        assert_eq!(toolchain(&cc, target, &first).unwrap().0, first_identity);
    }

    #[test]
    fn a_link_source_the_driver_adds_and_the_capture_cannot_replay_refuses() {
        // A wrapper `cc "$@" support.c -Wl,--wrap=f` over gcc: gcc compiles support.c
        // through cc1 and as, which the capture does not replay, so the link line names
        // two transient objects. Folding both into the one input marker kept --wrap=f
        // and dropped __wrap_f's definition (the 922 Arc recording sink).
        let work = tempfile::tempdir().unwrap();
        let bin = work.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let linker = bin.join("ld.lld");
        std::fs::write(&linker, b"linker").unwrap();
        let cc = bin.join("cc");
        let (own, added) = (work.path().join("cc1.o"), work.path().join("cc2.o"));
        std::fs::write(
            &cc,
            format!(
                "#!/bin/sh\necho '\"{}\" \"-pie\" \"-o\" \"out\" \"{}\" \"{}\" \"--wrap=f\" \"-lc\"' >&2\n",
                linker.display(),
                own.display(),
                added.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&cc, std::fs::Permissions::from_mode(0o755)).unwrap();
        let refused = link_recipe(&cc, "x86_64-unknown-linux-gnu").unwrap_err();
        assert!(refused.to_string().contains("cannot replay"), "{refused}");
    }

    fn recipe(input: &Path) -> LinkRecipe {
        LinkRecipe {
            schema: LINK_RECIPE_SCHEMA,
            target: "x86_64-unknown-linux-gnu".into(),
            linker: input.to_owned(),
            args: vec![INPUT_MARKER.into()],
            files: vec![recipe_file(input).unwrap()],
            source_files: vec![],
            identity: ContentHash::of_bytes(b"recipe"),
        }
    }

    /// One step of a stat-trust case: at `at` on the test clock, the file holds
    /// `bytes` stamped `time` (mtime and ctime), and a request asks for the
    /// recipe's inputs; `during` runs between that request's read and its
    /// classification.
    struct Step {
        at: u64,
        time: (i64, i64),
        bytes: &'static [u8],
        during: Option<(&'static [u8], u64)>,
        /// The boot id this request runs under; the record's when unset.
        boot: Option<Option<&'static str>>,
        expect: RecipeInputs,
        reads: usize,
    }

    fn step(
        at: u64,
        time: (i64, i64),
        bytes: &'static [u8],
        expect: RecipeInputs,
        reads: usize,
    ) -> Step {
        Step {
            at,
            time,
            bytes,
            during: None,
            boot: None,
            expect,
            reads,
        }
    }

    const BOOT: Option<&str> = Some("3a0fcf86-35cb-41c6-8348-cdf6518ffb7a\n");
    const OTHER_BOOT: Option<&str> = Some("5d2e9a41-0b7c-4f3e-9a8d-61c0ffee0042\n");
    const NAMESPACE: Option<&str> = Some("time:[4026531834]");

    /// Record the file as `first` bytes at clock 0, stamped `time`, under boot
    /// id `boot` and namespace `namespace`; then run `steps` in order. A step
    /// stamped like the record asserts that its stat really collides with the
    /// recorded one. Returns the first step that went otherwise.
    fn stat_trust_in(
        case: &str,
        (boot, namespace): (Option<&'static str>, Option<&'static str>),
        time: (i64, i64),
        first: &'static [u8],
        steps: Vec<Step>,
    ) -> Result<(), String> {
        SOURCE.set(Some((boot, namespace)));
        let result = stat_trust_steps(case, time, first, steps, namespace);
        SOURCE.set(None);
        CLOCK.set(None);
        TIMES.set(None);
        result
    }

    fn stat_trust(
        case: &str,
        time: (i64, i64),
        first: &'static [u8],
        steps: Vec<Step>,
    ) -> Result<(), String> {
        stat_trust_in(case, (BOOT, NAMESPACE), time, first, steps)
    }

    fn stat_trust_steps(
        case: &str,
        time: (i64, i64),
        first: &'static [u8],
        steps: Vec<Step>,
        namespace: Option<&'static str>,
    ) -> Result<(), String> {
        let work = tempfile::tempdir().unwrap();
        let input = work.path().join("crt.o");
        std::fs::write(&input, first).unwrap();
        CLOCK.set(Some(0));
        TIMES.set(Some(time));
        let mut recipe = recipe(&input);
        let recorded = recipe.files[0].stat.clone();
        for (index, step) in steps.into_iter().enumerate() {
            std::fs::write(&input, step.bytes).unwrap();
            TIMES.set(Some(step.time));
            if step.time == time {
                assert_eq!(
                    recipe_stat(&input).unwrap(),
                    recorded,
                    "{case} step {index}: no collision"
                );
            }
            CLOCK.set(Some(step.at));
            if let Some(boot) = step.boot {
                SOURCE.set(Some((boot, namespace)));
            }
            if let Some((bytes, at)) = step.during {
                let input = input.clone();
                AFTER_READ.set(Some(Box::new(move || {
                    std::fs::write(&input, bytes).unwrap();
                    CLOCK.set(Some(at));
                })));
            }
            READS.set(0);
            let inputs = refresh_recipe_inputs(&mut recipe).unwrap();
            AFTER_READ.set(None);
            let got = (inputs, READS.get());
            let want = (step.expect, step.reads);
            if got != want {
                return Err(format!("{case} step {index}: got {got:?}, want {want:?}"));
            }
        }
        Ok(())
    }

    #[test]
    fn a_stat_stands_for_bytes_only_after_two_sightings_a_window_apart() {
        use RecipeInputs::{Changed, Current, Restated};
        let tick = (1_700_000_000, 4_000_000);
        let later = (1_700_000_001, 4_000_000);
        let future = (4_102_444_800, 0); // 2100-01-01, ahead of any clock here
        let ms = 1_000_000;
        let mut results = vec![];
        results.push(stat_trust(
            "same-tick equal-length edit",
            tick,
            b"first",
            vec![step(ms, tick, b"later", Changed, 1)],
        ));
        results.push(stat_trust(
            "edit after promotion in a later tick",
            tick,
            b"first",
            vec![
                step(WINDOW, tick, b"first", Restated, 1),
                step(WINDOW + ms, tick, b"first", Current, 0),
                step(WINDOW + 2 * ms, later, b"later", Changed, 1),
            ],
        ));
        results.push(stat_trust(
            "sightings exactly a window apart",
            tick,
            b"first",
            vec![
                step(WINDOW, tick, b"first", Restated, 1),
                step(WINDOW + ms, tick, b"first", Current, 0),
            ],
        ));
        results.push(stat_trust(
            "sightings just under a window apart",
            tick,
            b"first",
            vec![
                step(WINDOW - 1, tick, b"first", Current, 1),
                step(WINDOW + 5 * ms, tick, b"later", Changed, 1),
            ],
        ));
        results.push(stat_trust(
            "an edit between sightings in the recorded tick",
            tick,
            b"first",
            vec![step(WINDOW, tick, b"later", Changed, 1)],
        ));
        results.push(stat_trust(
            "a ctime in the future",
            future,
            b"first",
            vec![step(ms, future, b"later", Changed, 1)],
        ));
        results.push(stat_trust(
            "an edit after the promoting read",
            tick,
            b"first",
            vec![
                Step {
                    during: Some((b"later", WINDOW + 10 * ms)),
                    ..step(WINDOW - 1, tick, b"first", Current, 1)
                },
                step(WINDOW + 20 * ms, tick, b"later", Changed, 1),
            ],
        ));
        let failed: Vec<String> = results.into_iter().filter_map(Result::err).collect();
        assert!(failed.is_empty(), "{failed:#?}");
    }

    #[test]
    fn a_settled_stat_holds_in_another_boot_and_a_sighting_does_not() {
        use RecipeInputs::{Current, Restated};
        let tick = (1_700_000_000, 4_000_000);
        let ms = 1_000_000;
        let failed: Vec<String> = [
            stat_trust(
                "settled in one boot, read in another and without a clock",
                tick,
                b"first",
                vec![
                    step(WINDOW, tick, b"first", Restated, 1),
                    Step {
                        boot: Some(OTHER_BOOT),
                        ..step(ms, tick, b"first", Current, 0)
                    },
                    Step {
                        boot: Some(None),
                        ..step(2 * ms, tick, b"first", Current, 0)
                    },
                ],
            ),
            stat_trust(
                "sighted in one boot, read a window later in another",
                tick,
                b"first",
                vec![
                    Step {
                        boot: Some(OTHER_BOOT),
                        ..step(WINDOW, tick, b"first", Restated, 1)
                    },
                    step(WINDOW + ms, tick, b"first", Current, 1),
                ],
            ),
            stat_trust_in(
                "an empty boot id is no clock",
                (Some("\n"), NAMESPACE),
                tick,
                b"first",
                vec![
                    step(WINDOW, tick, b"first", Current, 1),
                    step(3 * WINDOW, tick, b"first", Current, 1),
                ],
            ),
            stat_trust_in(
                "a malformed boot id is no clock",
                (Some("not-a-boot-id"), NAMESPACE),
                tick,
                b"first",
                vec![step(WINDOW, tick, b"first", Current, 1)],
            ),
            stat_trust_in(
                "an unreadable time namespace is no clock",
                (BOOT, None),
                tick,
                b"first",
                vec![step(WINDOW, tick, b"first", Current, 1)],
            ),
        ]
        .into_iter()
        .filter_map(Result::err)
        .collect();
        assert!(failed.is_empty(), "{failed:#?}");
    }

    #[test]
    fn a_record_written_by_the_earlier_stat_rule_is_not_read() {
        // e4b65f6c recorded stats it could not trust. A manifest it wrote names
        // runtime bytes that the file no longer holds, under the live stat; it
        // is shaped like a current record, so only its schema keeps it out.
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
        let directory = recipe_directory(&cc).unwrap();
        let key = driver_file_key(&cc).unwrap();
        let stale = |path: &Path| {
            serde_json::json!({
                "path": path,
                "content": ContentHash::of_bytes(b"bytes the file held before"),
                "stat": recipe_stat(path).unwrap(),
                "trust": "Settled",
            })
        };
        let manifest = serde_json::json!({
            "schema": 1,
            "target": target,
            "files": [stale(&cc.canonicalize().unwrap()), stale(&runtime), stale(&linker), stale(&crt)],
            "identity": ContentHash::of_bytes(b"identity"),
        });
        std::fs::write(
            directory.join(format!("toolchain-{target}-{key}.json")),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        assert_eq!(
            toolchain(&cc, target, &runtime).unwrap().0.runtime,
            ContentHash::of_parts(
                "hirsel.core-runtime.v2",
                [ContentHash::of_bytes(b"runtime").0.as_slice()]
            ),
        );
    }
}
