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
const LINK_RECIPE_SCHEMA: u32 = 3;
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
pub(crate) fn link_recipe(cc: &Path, target: &str, runtime: &Path) -> io::Result<LinkRecipe> {
    capture_link_recipe(cc, target, runtime, false)
}

/// Recapture is reserved for a recipe whose previously recorded toolchain
/// input disappeared.  Ordinary builds never invoke the C driver.
pub(crate) fn recapture_link_recipe(
    cc: &Path,
    target: &str,
    runtime: &Path,
) -> io::Result<LinkRecipe> {
    capture_link_recipe(cc, target, runtime, true)
}

fn capture_link_recipe(
    cc: &Path,
    target: &str,
    runtime: &Path,
    force: bool,
) -> io::Result<LinkRecipe> {
    let mut driver = cc.to_owned();
    let mut root = driver
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| io::Error::other("link driver has no toolchain root"))?;
    let mut directory = root.join(".hirsel-link-recipes");
    let mut driver_key = driver_file_key(&driver)?;
    let mut path = directory.join(format!("{target}-{driver_key}.json"));
    if !force
        && let Ok(bytes) = std::fs::read(&path)
        && let Ok(recipe) = serde_json::from_slice::<LinkRecipe>(&bytes)
        && recipe.schema == LINK_RECIPE_SCHEMA
        && recipe.target == target
        && recipe.linker.is_file()
        && recipe_inputs_match(&recipe)?
    {
        return Ok(recipe);
    }
    std::fs::create_dir_all(&directory)?;
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
            root = driver
                .parent()
                .and_then(Path::parent)
                .ok_or_else(|| io::Error::other("fallback clang has no toolchain root"))?;
            directory = root.join(".hirsel-link-recipes");
            driver_key = driver_file_key(&driver)?;
            path = directory.join(format!("{target}-{driver_key}.json"));
            std::fs::create_dir_all(&directory)?;
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
    let mut files = recipe_files(&linker, &args)?;
    if !files.iter().any(|file| file.path == runtime) {
        files.push(recipe_file(runtime)?);
    }
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
    std::fs::create_dir_all(&directory)?;
    let temp = tempfile::NamedTempFile::new_in(&directory)?;
    serde_json::to_writer(&temp, &recipe).map_err(io::Error::other)?;
    temp.persist(path).map_err(|e| e.error)?;
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
        "hirsel.direct-link-recipe.v1",
        [crate::encode(&(
            LINK_RECIPE_SCHEMA,
            target,
            driver_key,
            linker,
            args,
            files,
            source_files,
        ))?
        .as_slice()],
    ))
}

fn recipe_inputs_match(recipe: &LinkRecipe) -> io::Result<bool> {
    // The recipe is a published toolchain product. Compare metadata first;
    // only an input whose stat signature moved is re-hashed.
    for input in recipe.files.iter().chain(&recipe.source_files) {
        let Ok(stat) = recipe_stat(&input.path) else {
            return Ok(false);
        };
        if stat == input.stat {
            continue;
        }
        let Ok(bytes) = std::fs::read(&input.path) else {
            return Ok(false);
        };
        if ContentHash::of_bytes(&bytes) != input.content {
            return Ok(false);
        }
    }
    Ok(true)
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
    fn recipe_input_stat_change_rehashes_only_when_content_changes() {
        let work = tempfile::tempdir().unwrap();
        let input = work.path().join("crt.o");
        std::fs::write(&input, b"same bytes").unwrap();
        let recipe = LinkRecipe {
            schema: LINK_RECIPE_SCHEMA,
            target: "x86_64-unknown-linux-gnu".into(),
            linker: input.clone(),
            args: vec![INPUT_MARKER.into()],
            files: vec![recipe_file(&input).unwrap()],
            source_files: vec![],
            identity: ContentHash::of_bytes(b"recipe"),
        };
        assert!(recipe_inputs_match(&recipe).unwrap());
        std::fs::write(&input, b"same bytes").unwrap();
        assert!(
            recipe_inputs_match(&recipe).unwrap(),
            "touch-only replacement must reuse"
        );
        std::fs::write(&input, b"changed bytes").unwrap();
        assert!(
            !recipe_inputs_match(&recipe).unwrap(),
            "changed content must miss"
        );
    }
}
