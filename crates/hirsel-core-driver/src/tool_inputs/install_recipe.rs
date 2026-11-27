use super::*;

/// Install the host linker and the target's dynamic link inputs.
pub(crate) fn install_recipe(
    kit: &Path,
    linker: &[u8],
    archive_bin: &Path,
    target: &str,
    root: &Path,
) -> io::Result<PathBuf> {
    #[derive(serde::Deserialize)]
    struct KitTarget {
        start: Vec<String>,
        unwind: String,
        unwind_script: String,
        libraries: Vec<String>,
        nonshared: String,
        end: Vec<String>,
    }
    /// The system stubs in a Mach-O link kit.
    #[derive(serde::Deserialize)]
    struct DarwinTarget {
        libraries: Vec<String>,
    }
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
    let stubs = |row| -> io::Result<Vec<String>> {
        serde_json::from_value::<DarwinTarget>(row)
            .map_err(io::Error::other)?
            .libraries
            .iter()
            .map(|path| copy(path))
            .collect()
    };
    let target_kind =
        Target::from_triple(target).ok_or_else(|| io::Error::other("no installed link target"))?;
    let args = match target_kind.os() {
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
            match target_kind.arch() {
                Arch::Aarch64 => args.push("--fix-cortex-a53-843419".into()),
                Arch::X86_64 => {}
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
        Os::Macos => {
            let mut args = ["-arch", "arm64"]
                .into_iter()
                .chain(target_kind.apple().unwrap().platform_version_args())
                .chain(["-adhoc_codesign", INPUT_MARKER])
                .map(str::to_owned)
                .collect::<Vec<_>>();
            args.extend(stubs(row)?);
            args
        }
        Os::Ios => stubs(row)?,
    };
    crate::installation::install_file(linker, &root.join(INSTALLED_LINKER), 0o755)?;
    let relative = Path::new("link").join(format!("{target}.json"));
    let installed = InstalledRecipe {
        schema: INSTALLED_RECIPE_SCHEMA,
        target: target.into(),
        args,
        archive_tools: archive_tools::install(archive_bin, root)?,
    };
    publish(&root.join("link"), &root.join(&relative), &installed)?;
    Ok(relative)
}
