//! The core declarations a library reaches, read from its checked IR (M4-PLAN
//! §3, W5): the deciding experiment fails when a library reaches a core
//! declaration outside the frozen allowlist `core/HOOK-ALLOWLIST`.
//!
//! An item reaches a core declaration two ways, and the inventory takes both:
//! - by name: every core definition its resolution finds (a function, a
//!   header-only operation that lowers to an instruction with no call row, a
//!   type written in a signature);
//! - by checked IR: every core path a checked body's rows hold without a
//!   name: an instance row (a call, a clone or finalizer hook, a compiler
//!   glue operation of `GLUE_OPERATIONS`), a type row's canonical text (a
//!   literal's String, each type argument), a dictionary, and a generic
//!   template's type and dictionary recipes. A callable adapter counts as
//!   its function.
use crate::CheckProduct;
use hirsel_core_front::GenericTemplate;
use hirsel_core_ids::DefPath;
use hirsel_core_resolve::{ResolutionScratch, Target};
use std::collections::BTreeSet;
use std::io;

const CORE: &str = "core@1/";

/// Every core declaration the items of `package` whose module path starts
/// with `modules` reach, as `core@1/module/name`: the names each item
/// resolves to, and the rows each checked body's IR names.
pub fn core_inventory(
    product: &CheckProduct,
    package: &str,
    modules: &[String],
) -> io::Result<BTreeSet<String>> {
    let index = &product.index;
    let selected =
        |path: &DefPath| path.0.package.name == package && path.0.modules.starts_with(modules);
    let mut reached = BTreeSet::new();
    let mut scratch = ResolutionScratch::default();
    let mut items = 0;
    for file in &product.files {
        let module = index.module_path(file.module);
        if module.package.name != package || !module.modules.starts_with(modules) {
            continue;
        }
        for (ordinal, item) in file.syntax.items().enumerate() {
            items += 1;
            let resolution = scratch.resolve(index, file.module, file.file, ordinal as u32, item);
            for used in &resolution.names {
                if let Target::Definition(d) = used.target {
                    let path = index.definition_path(d);
                    if path.0.package.name == "core" {
                        reached.insert(path.to_string());
                    }
                }
            }
        }
    }
    for (path, definition) in &product.definitions {
        if !selected(path) {
            continue;
        }
        let ir = definition.ir()?;
        for key in &ir.instances {
            names(&key.definition.to_string(), &mut reached);
            key.dictionaries.iter().for_each(|d| names(d, &mut reached));
        }
        ir.types.iter().for_each(|t| names(&t.name, &mut reached));
        if let Some(template) = &definition.check.template {
            recipes(template, &mut reached);
        }
    }
    if items == 0 {
        return Err(io::Error::other(format!(
            "no item of `{package}` under `{}`",
            modules.join("/")
        )));
    }
    Ok(reached)
}

/// Every core declaration of the checked program, as `core@1/module/name`.
pub fn core_declarations(product: &CheckProduct) -> BTreeSet<String> {
    let index = &product.index;
    index
        .definitions()
        .map(|(id, _)| index.definition_path(id))
        .filter(|path| path.0.package.name == "core")
        .map(|path| path.to_string())
        .chain(
            hirsel_core_front::GLUE_OPERATIONS
                .iter()
                .map(|(module, name)| format!("{CORE}{module}/{name}")),
        )
        .collect()
}

/// The entries of `allowlist` text: one declaration per line; `#` starts a
/// comment and a `library ...` line names a gated library.
pub fn allowlist(text: &str) -> BTreeSet<String> {
    text.lines()
        .map(|line| line.split('#').next().unwrap_or("").trim())
        .filter(|line| !line.is_empty() && !line.starts_with("library "))
        .map(str::to_owned)
        .collect()
}

fn recipes(template: &GenericTemplate, reached: &mut BTreeSet<String>) {
    let parts = template
        .types
        .values()
        .chain(template.calls.values().flat_map(|c| &c.dictionaries));
    for recipe in parts {
        for part in &recipe.0 {
            if let hirsel_core_front::NamePart::Text(text) = part {
                names(text, reached);
            }
        }
    }
    template.nested.values().for_each(|t| recipes(t, reached));
}

/// Each `core@1/…` definition path in canonical text, without its type arguments.
fn names(text: &str, reached: &mut BTreeSet<String>) {
    let mut rest = text;
    while let Some(at) = rest.find(CORE) {
        let tail = &rest[at..];
        let end = tail
            .find(|c: char| !(c.is_alphanumeric() || "_@/.$".contains(c)))
            .unwrap_or(tail.len());
        // A callable adapter is its function's value: the function is reached.
        let name = &tail[..end];
        reached.insert(name.strip_suffix("$callable").unwrap_or(name).to_owned());
        rest = &tail[end..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CheckRequest, Driver, LibraryInput, Meter, package};
    use hirsel_core_resolve::{CorpusManifest, ModuleSource, PackageManifest};
    use std::path::Path;

    /// core or std as installed, by the driver's own library discovery.
    fn installed(root: &Path, name: &str) -> PackageManifest {
        let mut manifest =
            PackageManifest::directory(&root.join("core").join(name), package(name), true).unwrap();
        if name != "core" {
            manifest.dependencies.push(package("core"));
        }
        manifest
    }

    fn check(
        root: &Path,
        packages: Vec<PackageManifest>,
        store: &Path,
    ) -> std::sync::Arc<CheckProduct> {
        let mut all = vec![installed(root, "core"), installed(root, "std")];
        all.extend(packages);
        let request = CheckRequest {
            manifest: None,
            inputs: CorpusManifest {
                core: package("core"),
                packages: all,
            },
            store: store.to_path_buf(),
            library: LibraryInput::Sources,
            entry: None,
        };
        let product = Driver::default()
            .check(request, &mut Meter::new(false))
            .unwrap();
        assert!(product.accepted(), "{:?}", product.diagnostics);
        product
    }

    /// Law: an item reaches every core declaration it names (a function, a
    /// header-only host operation, a type written only in a signature) and
    /// every one its checked body holds without naming it (a string literal
    /// is a core String, LANG-2). Each route is the only one to its entry.
    #[test]
    fn the_inventory_holds_what_items_name_and_what_bodies_hold() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("lib.hirsel");
        std::fs::write(
            &file,
            "pub fn stamp(c: Clock) -> u64 { host.wall_ms(c) }\n\
             pub fn count(m: Map[u64, u64]) -> u64 { map.len(m) }\n\
             pub fn literal() -> u64 { let s = \"hi\"; 2 }\n",
        )
        .unwrap();
        let lib = PackageManifest {
            namespace: None,
            package: package("lib"),
            dependencies: vec![package("core"), package("std")],
            sources: vec![ModuleSource {
                foreign: None,
                captured: None,
                file,
                modules: vec!["lib".into()],
                signatures: false,
            }],
            toolchain: false,
        };
        let product = check(&root, vec![lib], &dir.path().join("store"));
        let reached = core_inventory(&product, "lib", &[]).unwrap();
        for expected in [
            "core@1/core/Clock",
            "core@1/host/wall_ms",
            "core@1/core/Map",
            "core@1/map/len",
            "core@1/core/String",
        ] {
            assert!(
                reached.contains(expected),
                "{expected} missing from {reached:?}"
            );
        }
        let declared = core_declarations(&product);
        let unknown: Vec<_> = reached.difference(&declared).collect();
        assert!(unknown.is_empty(), "not core declarations: {unknown:?}");
    }

    /// The deciding experiment's fail line (M4-PLAN §3): every entry of
    /// core/HOOK-ALLOWLIST is a core declaration, and every library it gates
    /// reaches nothing outside it.
    #[test]
    fn gated_libraries_reach_only_allowlisted_core() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap();
        let text = std::fs::read_to_string(root.join("core/HOOK-ALLOWLIST")).unwrap();
        let allowed = allowlist(&text);
        let libraries: Vec<Vec<&str>> = text
            .lines()
            .filter_map(|l| l.strip_prefix("library "))
            .map(|l| l.split_whitespace().collect())
            .collect();
        assert!(
            !libraries.is_empty(),
            "core/HOOK-ALLOWLIST gates no library"
        );
        let dir = tempfile::tempdir().unwrap();
        let product = check(&root, vec![], &dir.path().join("core"));
        let declared = core_declarations(&product);
        let stale: Vec<_> = allowed.difference(&declared).collect();
        assert!(
            stale.is_empty(),
            "allowlisted, but not core declarations: {stale:?}"
        );
        for (i, library) in libraries.iter().enumerate() {
            let [name, manifest, modules @ ..] = library.as_slice() else {
                panic!("library line {library:?}: library PACKAGE MANIFEST|installed [MODULE...]");
            };
            let packages = if *manifest == "installed" {
                vec![]
            } else {
                manifest_closure(&root, manifest, name)
            };
            let product = check(&root, packages, &dir.path().join(i.to_string()));
            let modules: Vec<String> = modules.iter().map(|m| m.to_string()).collect();
            let reached = core_inventory(&product, name, &modules).unwrap();
            let outside: Vec<_> = reached.difference(&allowed).collect();
            assert!(
                outside.is_empty(),
                "{library:?} reaches core outside core/HOOK-ALLOWLIST: {outside:?}"
            );
        }
    }

    /// `name` and the packages of `manifest` it depends on, core and std aside.
    fn manifest_closure(root: &Path, manifest: &str, name: &str) -> Vec<PackageManifest> {
        #[derive(serde::Deserialize)]
        struct Manifest {
            packages: Vec<PackageManifest>,
        }
        let path = root.join(manifest);
        if path
            .file_name()
            .is_some_and(|file| file == crate::package_dir::MANIFEST)
        {
            let directory = crate::package_dir::PackageDir::open(path.parent().unwrap()).unwrap();
            assert_eq!(directory.package.package.name, name);
            return vec![directory.package];
        }
        let all: Manifest = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let mut pending = vec![name.to_string()];
        let mut out: Vec<PackageManifest> = vec![];
        while let Some(next) = pending.pop() {
            if ["core", "std"].contains(&next.as_str())
                || out.iter().any(|p| p.package.name == next)
            {
                continue;
            }
            let mut p = all
                .packages
                .iter()
                .find(|p| p.package.name == next)
                .unwrap_or_else(|| panic!("{manifest} has no package {next}"))
                .clone();
            p.sources
                .iter_mut()
                .for_each(|s| s.file = root.join(&s.file));
            pending.extend(p.dependencies.iter().map(|d| d.name.clone()));
            out.push(p);
        }
        out
    }
}
