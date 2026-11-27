//! Capture and normalize the linker command selected by a C driver.
use super::*;

/// Capture the link editor command once beside the selected toolchain, with
/// the driver's file key, hashed once for both. The capture is the only
/// C-driver use in the direct-link path; subsequent builds invoke the recorded
/// lld argv themselves.
pub(super) fn keyed_link_recipe(
    cc: &Path,
    target: &str,
    force: bool,
    search: &OsStr,
) -> io::Result<(LinkRecipe, String, ContentHash)> {
    let driver = cc.to_owned();
    let driver_key = driver_file_key(&driver)?;
    let target_kind = Target::from_triple(target)
        .ok_or_else(|| io::Error::other(format!("no link target {target}")))?;
    let environment = CaptureEnvironment::new(search, target_kind)?;
    let file_key = environment.key(&driver_key)?;
    link_recipe(cc, target, force, &environment, &file_key)
        .map(|r| (r, file_key, environment.digest()))
}

fn link_recipe(
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
    let format = Target::from_triple(target)
        .ok_or_else(|| io::Error::other(format!("no link target {target}")))?
        .object_format();
    let editor = crate::link_closure::editor_name(format);
    let gcc = match format {
        ObjectFormat::Elf => true,
        ObjectFormat::MachO => false,
    };
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
        .arg(format!("-print-prog-name={editor}"))
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
    let search_editor = |words: io::Result<Vec<String>>| match words {
        Ok(words)
            if words.first().is_some_and(|word| {
                Path::new(word)
                    .file_name()
                    .is_some_and(|name| name == editor || (gcc && name == "collect2"))
            }) =>
        {
            Some(Ok(words))
        }
        Err(error) => Some(Err(error)),
        _ => None,
    };
    let mut argv = stderr
        .lines()
        .rev()
        .filter(|line| line.contains(editor) || line.contains("collect2"))
        .map(shell_words)
        .find_map(search_editor)
        .transpose()?
        .ok_or_else(|| io::Error::other("link recipe capture emitted no link editor argv"))?;
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
        None => find_program(&name, search)?,
    };
    // clang looks beside itself, then on PATH: an editor it found on PATH is
    // PATH's choice, so the record keeps it bare and each request selects its own.
    let name = match name.file_name() {
        Some(file)
            if !collect2
                && name.is_absolute()
                && linker.parent() != driver.canonicalize()?.parent()
                && find_program(Path::new(file), search).ok().as_ref() == Some(&linker) =>
        {
            PathBuf::from(file)
        }
        _ => name,
    };
    let key = recipe_key(target, file_key, &linker)?;
    let mut source_files = Vec::new();
    let mut sdk_settings = Vec::new();
    let mut synthetic_object = None;
    let mut compiled_objects = std::collections::HashMap::new();
    for (index, line) in stderr.lines().enumerate() {
        let words = shell_words(line)?;
        if !words.iter().any(|word| word == "-cc1") {
            continue;
        }
        sdk_settings.extend(words.iter().filter_map(|word| {
            let path = Path::new(word.strip_prefix("-fdepfile-entry=")?);
            path.is_file().then(|| path.to_owned())
        }));
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
            synthetic_object = Some(transient_output);
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
        // Keep fixed support objects as inputs to every executable.
        if let Some(stable) = compiled_objects.get(Path::new(arg)) {
            args.push(stable.display().to_string());
            i += 1;
            continue;
        }
        // A driver that finds no startup file for the target (clang without a
        // GCC installation or sysroot for it) names the file bare, while every
        // object a driver compiles itself is a path into its temporary
        // directory. The bare name is a link input the host lacks.
        let object = !arg.starts_with('-') && arg.ends_with(".o");
        if object && Path::new(arg).components().count() == 1 && !Path::new(arg).exists() {
            return Err(host_link_input_error(
                format,
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!(
                        "missing link input: {} found no {arg} for {target}",
                        driver.display()
                    ),
                ),
            ));
        }
        let transient = match &synthetic_object {
            Some(path) => path == Path::new(arg),
            None => object && !Path::new(arg).exists(),
        };
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
    match format {
        ObjectFormat::Elf => {
            let support = ["-lgcc_s", "-lpthread", "-ldl", "-lm", "-lrt", "-lutil"];
            args.splice(marker + 1..marker + 1, support.map(str::to_owned));
        }
        ObjectFormat::MachO => {
            let support = ["-framework", "CoreFoundation", "-framework", "Security"];
            args.splice(marker + 1..marker + 1, support.map(str::to_owned));
            if installed_linker.is_some() || linker.file_name().is_some_and(|name| name == "lld") {
                args.splice(..0, ["-flavor".into(), "darwin".into()]);
            }
        }
    }
    // The driver's configuration files decide its argv: a changed one
    // captures again.
    let configuration = stderr
        .lines()
        .filter_map(|line| line.strip_prefix("Configuration file: "))
        .map(PathBuf::from);
    let (files, closure) = recipe_files(format, &linker, &args, configuration.chain(sdk_settings))?;
    let identity = recipe_identity(
        target,
        ContentHash::of_bytes(&std::fs::read(&driver)?),
        &linker,
        &args,
        &files,
        &source_files,
        &closure,
    )?;
    let recipe = LinkRecipe {
        schema: LINK_RECIPE_SCHEMA,
        target: target.into(),
        linker,
        args,
        files,
        source_files,
        absent: closure.absent,
        directories: closure.directories,
        binaries: closure.binaries,
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
    format: ObjectFormat,
    linker: &Path,
    args: &[String],
    configuration: impl Iterator<Item = PathBuf>,
) -> io::Result<(Vec<RecipeFile>, crate::link_closure::Closure)> {
    let mut closure =
        crate::link_closure::for_format(format, args, INPUT_MARKER).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                host_link_input_error(format, error)
            } else {
                error
            }
        })?;
    let mut paths = std::mem::take(&mut closure.files);
    paths.push(linker.to_owned());
    paths.extend(configuration);
    let linker_library = match format {
        ObjectFormat::Elf => None,
        ObjectFormat::MachO => crate::host::INSTALLED_LINKER_LIBRARY,
    };
    if let Some(library) = linker_library
        && let Some(root) = linker.parent().and_then(Path::parent)
        && linker.ends_with(INSTALLED_LINKER)
        && root.join(library).is_file()
    {
        paths.push(root.join(library));
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
    let files = paths
        .into_iter()
        .map(|path| recipe_file(&path))
        .collect::<io::Result<_>>()?;
    Ok((files, closure))
}

fn host_link_input_error(format: ObjectFormat, error: io::Error) -> io::Error {
    let help = match format {
        ObjectFormat::Elf => {
            "install the target's glibc and GCC development files (libc6-dev and libgcc-N-dev on Debian/Ubuntu, glibc-devel and gcc on Fedora). Match the GCC version and paths in the link recipe, or select a configured C driver with --cc"
        }
        ObjectFormat::MachO => {
            "set SDKROOT to the target's Apple SDK or select a C driver configured for that SDK with --cc"
        }
    };
    io::Error::new(error.kind(), format!("{error}; {help}"))
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
    closure: &crate::link_closure::Closure,
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
            &closure.absent,
            &closure.directories,
        ))?
        .as_slice()],
    ))
}
