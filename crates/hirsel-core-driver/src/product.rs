//! A package's checked image is a toolchain input, not a user-build computation.
//! The bundled source identity is fixed when Cargo/Buck constructs the toolchain.
use super::*;
use hirsel_core_resolve::ModuleSource;

mod bundled {
    include!(concat!(env!("OUT_DIR"), "/core_sources.rs"));
}

/// The ordinary package-image mechanism accepts core today and core+std unchanged.
/// Explicit source manifests use their content digest; bundled core has a build-time
/// digest and needs no source scan at process startup.
pub struct PackageProduct {
    pub image: Arc<CoreImage>,
    pub rebuilt: bool,
}
impl PackageProduct {
    pub fn bundled_digest() -> ContentHash {
        ContentHash(bundled::DIGEST)
    }
    pub fn bundled_manifest(root: &Path) -> PackageManifest {
        PackageManifest {
            package: package("core"),
            dependencies: vec![],
            sources: bundled::SOURCES
                .iter()
                .map(|(module, _)| ModuleSource {
                    file: root.join(format!("{module}.hirsel")),
                    modules: vec![(*module).into()],
                    signatures: true,
                })
                .collect(),
        }
    }
    pub fn default_cache() -> PathBuf {
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".cache")))
            .unwrap_or_else(std::env::temp_dir)
            .join("hirsel/core-packages")
    }
    pub fn bundled(root: &Path, meter: &mut Meter) -> io::Result<Self> {
        let digest = Self::bundled_digest();
        let directory = cache_path(root, digest)?;
        if let Some(image) = CoreImage::open(&directory)?
            && image.source_stamp == digest
        {
            meter.finish("package product load");
            return Ok(Self {
                image,
                rebuilt: false,
            });
        }
        meter.finish("package product lookup");
        // Source extraction is paid only by toolchain production/cache rebuilding.
        let sources = directory.join("sources");
        std::fs::create_dir_all(&sources)?;
        for (module, text) in bundled::SOURCES {
            // Concurrent first users may produce the same package. A source
            // reader must see an entire frozen file, never another writer's prefix.
            use std::io::Write;
            let mut file = tempfile::NamedTempFile::new_in(&sources)?;
            file.write_all(text.as_bytes())?;
            file.persist(sources.join(format!("{module}.hirsel")))?;
        }
        let inputs = CorpusManifest {
            core: package("core"),
            packages: vec![Self::bundled_manifest(&sources)],
        };
        Self::produce(inputs, directory, meter, digest)
    }
    /// Source-owned package producers use this explicit identity once per package
    /// revision. The installed consumer passes only the resulting cache key/image.
    pub fn sources(mut inputs: CorpusManifest, root: &Path, meter: &mut Meter) -> io::Result<Self> {
        inputs.packages.sort_by(|a, b| a.package.cmp(&b.package));
        for package in &mut inputs.packages {
            package.dependencies.sort();
            package.sources.sort_by(|a, b| a.modules.cmp(&b.modules));
        }
        let mut stamps = Vec::new();
        for package in &inputs.packages {
            for source in &package.sources {
                stamps.push((
                    &package.package,
                    &source.modules,
                    source.signatures,
                    ContentHash::of_bytes(&std::fs::read(&source.file)?),
                ));
            }
        }
        let dependencies: Vec<_> = inputs
            .packages
            .iter()
            .map(|p| (&p.package, &p.dependencies))
            .collect();
        let digest = ContentHash::of_bytes(&encode(&(&inputs.core, dependencies, stamps))?);
        let directory = cache_path(root, digest)?;
        if let Some(image) = CoreImage::open(&directory)?
            && image.source_stamp == digest
        {
            meter.finish("package product load");
            return Ok(Self {
                image,
                rebuilt: false,
            });
        }
        meter.finish("package product lookup");
        Self::produce(inputs, directory, meter, digest)
    }
    fn produce(
        inputs: CorpusManifest,
        directory: PathBuf,
        meter: &mut Meter,
        source_stamp: ContentHash,
    ) -> io::Result<Self> {
        let (checked, image) = Driver::default().produce_package(
            CheckRequest {
                inputs,
                store: directory,
                library: LibraryInput::Sources,
                entry: None,
            },
            meter,
            false,
            source_stamp,
        )?;
        let image = image.ok_or_else(|| {
            io::Error::other(format!(
                "package product incomplete: {} refusals, {} gaps; first: {}",
                checked.diagnostics.len(),
                checked.incomplete.len(),
                checked
                    .diagnostics
                    .first()
                    .map(|d| d.message.as_str())
                    .or_else(|| checked.incomplete.first().map(|g| g.message.as_str()))
                    .unwrap_or("unavailable")
            ))
        })?;
        Ok(Self {
            image,
            rebuilt: true,
        })
    }
}
fn cache_path(root: &Path, sources: ContentHash) -> io::Result<PathBuf> {
    let identity = ContentHash::of_parts(
        "hirsel.package-product.v1",
        [&compiler_identity()?.0[..], &sources.0],
    );
    Ok(root.join(
        identity
            .0
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>(),
    ))
}
impl Driver {
    pub fn check_with_product(
        &mut self,
        request: CheckRequest,
        root: &Path,
        meter: &mut Meter,
    ) -> io::Result<Arc<CheckProduct>> {
        self.check_package_product(request, root, meter, false)
    }
    pub fn check_reachable_with_product(
        &mut self,
        request: CheckRequest,
        root: &Path,
        meter: &mut Meter,
    ) -> io::Result<Arc<CheckProduct>> {
        self.check_package_product(request, root, meter, true)
    }
    fn check_package_product(
        &mut self,
        mut request: CheckRequest,
        root: &Path,
        meter: &mut Meter,
        reachable: bool,
    ) -> io::Result<Arc<CheckProduct>> {
        if reachable && let LibraryInput::Image(image) = &request.library {
            request.library = LibraryInput::ReachableImage(Arc::clone(image));
        }
        if !matches!(request.library, LibraryInput::Sources) {
            return self.check(request, meter);
        }
        let Some(core) = request
            .inputs
            .packages
            .iter()
            .find(|p| p.package == request.inputs.core)
        else {
            return self.check(request, meter);
        };
        let compiled = normalized(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../core/core"));
        // Buck embeds package-relative manifest paths; its tests run in the
        // crate, while the command-line runner runs in the workspace root.
        let workspace = normalized(Path::new("core/core"));
        let is_bundled = core.package == package("core")
            && core.dependencies.is_empty()
            && core.sources.len() == bundled::SOURCES.len()
            && core
                .sources
                .iter()
                .zip(bundled::SOURCES)
                .all(|(s, (module, _))| {
                    s.signatures
                        && s.modules == [*module]
                        && [&compiled, &workspace].iter().any(|root| {
                            normalized(&s.file) == root.join(format!("{module}.hirsel"))
                        })
                });
        let product = if is_bundled {
            PackageProduct::bundled(root, meter)?
        } else {
            PackageProduct::sources(
                CorpusManifest {
                    core: request.inputs.core.clone(),
                    packages: vec![core.clone()],
                },
                root,
                meter,
            )?
        };
        request
            .inputs
            .packages
            .retain(|p| p.package != request.inputs.core);
        request.library = if reachable {
            LibraryInput::ReachableImage(product.image)
        } else {
            LibraryInput::Image(product.image)
        };
        self.check(request, meter)
    }
}

fn normalized(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir().unwrap().join(path)
    };
    for component in absolute.components() {
        match component {
            std::path::Component::ParentDir => {
                result.pop();
            }
            std::path::Component::CurDir => {}
            c => result.push(c.as_os_str()),
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn library(source: &str, dir: &Path) -> CorpusManifest {
        let file = dir.join("core.hirsel");
        std::fs::write(&file, source).unwrap();
        CorpusManifest {
            core: package("core"),
            packages: vec![PackageManifest {
                package: package("core"),
                dependencies: vec![],
                sources: vec![ModuleSource {
                    file,
                    modules: vec!["core".into()],
                    signatures: true,
                }],
            }],
        }
    }
    #[test]
    fn source_paths_normalize_relative_to_the_process_directory() {
        let root = std::env::current_dir().unwrap();
        assert_eq!(normalized(Path::new("core/core")), root.join("core/core"));
        assert_eq!(
            normalized(Path::new("core/../core/core")),
            root.join("core/core")
        );
    }
    #[test]
    fn package_product_keys_source_and_compiler_and_rebuilds_foreign_images() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("products");
        let inputs = library(
            "pub fn identity[T](take x: T) -> T { x }\npub fn answer() -> u64 { 7 }\n",
            dir.path(),
        );
        let first =
            PackageProduct::sources(inputs.clone(), &cache, &mut Meter::new(false)).unwrap();
        assert!(first.rebuilt);
        assert!(
            first
                .image
                .demand(&DefPath::from_full("core@1/core/identity").unwrap())
                .unwrap()
                .check
                .template
                .is_some()
        );
        let warm = PackageProduct::sources(inputs.clone(), &cache, &mut Meter::new(false)).unwrap();
        assert!(!warm.rebuilt);
        assert_eq!(first.image.identity, warm.image.identity);
        // A valid store envelope carrying a different compiler is a miss, not data.
        let mut wrong = CoreImage::open(&first.image.store_root()).unwrap().unwrap();
        let image = Arc::get_mut(&mut wrong).unwrap();
        let mut root: crate::image::ImageRoot =
            decode(&image.store.get(image.identity).unwrap().unwrap()).unwrap();
        root.compiler = ContentHash::of_bytes(b"foreign compiler");
        let identity = image.store.put(&encode(&root).unwrap()).unwrap();
        image.store.select("image-root", identity).unwrap();
        let rebuilt =
            PackageProduct::sources(inputs.clone(), &cache, &mut Meter::new(false)).unwrap();
        assert!(rebuilt.rebuilt);
        assert_eq!(rebuilt.image.identity, first.image.identity);
        let inputs = library(
            "pub fn identity[T](take x: T) -> T { x }\npub fn answer() -> u64 { 9 }\n",
            dir.path(),
        );
        let changed = PackageProduct::sources(inputs, &cache, &mut Meter::new(false)).unwrap();
        assert!(changed.rebuilt);
        assert_ne!(changed.image.identity, first.image.identity);
        let answer = DefPath::from_full("core@1/core/answer").unwrap();
        // A valid image from a different source revision in this cache location
        // is also a miss, even if its compiler and record digests are valid.
        for file in std::fs::read_dir(changed.image.store_root()).unwrap() {
            let file = file.unwrap();
            if file.path().is_file() {
                std::fs::copy(file.path(), first.image.store_root().join(file.file_name()))
                    .unwrap();
            }
        }
        let original = library(
            "pub fn identity[T](take x: T) -> T { x }\npub fn answer() -> u64 { 7 }\n",
            dir.path(),
        );
        let repaired = PackageProduct::sources(original, &cache, &mut Meter::new(false)).unwrap();
        assert!(repaired.rebuilt);
        assert_eq!(repaired.image.identity, first.image.identity);
        let body = changed.image.demand(&answer).unwrap();
        assert_eq!(
            hirsel_core_ir::eval::run(&body.ir, body.check.function.unwrap(), &[], 1_000_000)
                .outcome,
            hirsel_core_ir::eval::Outcome::Returned(vec![9])
        );
    }
    #[test]
    fn bundled_product_reopens_without_reading_extracted_core_source() {
        let dir = tempfile::tempdir().unwrap();
        let first = PackageProduct::bundled(dir.path(), &mut Meter::new(false)).unwrap();
        assert!(first.rebuilt);
        std::fs::remove_dir_all(first.image.store_root().join("sources")).unwrap();
        let warm = PackageProduct::bundled(dir.path(), &mut Meter::new(false)).unwrap();
        assert!(!warm.rebuilt);
        assert_eq!(first.image.identity, warm.image.identity);
        assert_eq!(
            (warm.image.decoded_files(), warm.image.decoded_bodies()),
            (0, 0)
        );
    }
}

#[cfg(test)]
mod agreement {
    use super::*;
    fn sorted_json<T: Serialize>(rows: T) -> Vec<String> {
        let value = serde_json::to_value(rows).unwrap();
        let mut rows = value
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>();
        rows.sort();
        rows
    }
    fn compare(source: &CheckProduct, product: &CheckProduct) {
        assert_eq!(
            sorted_json(source.machine_diagnostics().unwrap()),
            sorted_json(product.machine_diagnostics().unwrap())
        );
        assert_eq!(
            sorted_json(source.machine_incomplete().unwrap()),
            sorted_json(product.machine_incomplete().unwrap())
        );
        for (path, body) in &source.definitions {
            if path.0.package == package("core") {
                continue;
            }
            let image = &product.definitions[path];
            assert_eq!(
                hirsel_core_ir::text::print(&body.ir),
                hirsel_core_ir::text::print(&image.ir),
                "CoreIR for {path}"
            );
            assert_eq!(
                encode(&body.check.template).unwrap(),
                encode(&image.check.template).unwrap(),
                "template {path}"
            );
        }
    }
    fn run(inputs: CorpusManifest, entry: Option<DefPath>, dir: &Path, cache: &Path) {
        let mut request = CheckRequest {
            inputs,
            store: dir.join("source"),
            library: LibraryInput::Sources,
            entry,
        };
        let source = Driver::default()
            .check(request.clone(), &mut Meter::new(false))
            .unwrap();
        request.store = dir.join("product");
        let product = Driver::default()
            .check_with_product(request, cache, &mut Meter::new(false))
            .unwrap();
        compare(&source, &product);
    }
    #[test]
    fn all_front_cases_and_m1_closures_agree_with_published_core() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("products");
        fn walk(path: &Path, out: &mut Vec<PathBuf>) {
            for entry in std::fs::read_dir(path).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().is_some_and(|e| e == "hirsel") {
                    out.push(path);
                }
            }
        }
        let mut cases = vec![];
        walk(&root.join("core/front/cases"), &mut cases);
        cases.sort();
        let core =
            PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
        let count = cases.len();
        assert!(count > 200);
        for (i, file) in cases.into_iter().enumerate() {
            let inputs = CorpusManifest {
                core: package("core"),
                packages: vec![
                    core.clone(),
                    PackageManifest {
                        package: package("app"),
                        dependencies: vec![package("core")],
                        sources: vec![ModuleSource {
                            file: file.clone(),
                            modules: vec!["case".into()],
                            signatures: false,
                        }],
                    },
                ],
            };
            // Entry policy is the same one used by the front corpus.
            let text = std::fs::read_to_string(&file).unwrap();
            let entry = text
                .contains("fn main(")
                .then(|| DefPath::from_full("app@1/case/main").unwrap());
            run(inputs, entry, &dir.path().join(format!("case{i}")), &cache);
        }
        let manifest: serde_json::Value = serde_json::from_slice(
            &std::fs::read(root.join("core/programs/m1/manifest.json")).unwrap(),
        )
        .unwrap();
        let mut inputs: CorpusManifest = serde_json::from_value(manifest.clone()).unwrap();
        for p in &mut inputs.packages {
            for s in &mut p.sources {
                s.file = root.join(&s.file);
            }
        }
        for (i, entry) in manifest["entries"].as_array().unwrap().iter().enumerate() {
            let name = entry["package"]["name"].as_str().unwrap();
            let mut selected = inputs.clone();
            selected
                .packages
                .retain(|p| ["core", "std", "m1", name].contains(&p.package.name.as_str()));
            let path = DefPath::from_full(&format!(
                "{name}@1/{}/{}",
                entry["module"].as_str().unwrap(),
                entry["function"].as_str().unwrap()
            ))
            .unwrap();
            run(
                selected,
                Some(path),
                &dir.path().join(format!("m1{i}")),
                &cache,
            );
        }
        eprintln!("package agreement: {count} front cases and 18 M1 entries");
    }
}

#[cfg(test)]
mod open_tests {
    use super::*;
    #[test]
    fn open_reads_only_the_fixed_root_and_not_the_package_index() {
        let dir = tempfile::tempdir().unwrap();
        let product = PackageProduct::bundled(dir.path(), &mut Meter::new(false)).unwrap();
        let root_path = product.image.store_root();
        let root: crate::image::ImageRoot = decode(
            &product
                .image
                .store
                .get(product.image.identity)
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(encode(&root).unwrap().len(), 128);
        std::fs::write(
            product.image.store.path(root.directory),
            b"damaged unused directory",
        )
        .unwrap();
        let opened = CoreImage::open(&root_path).unwrap().unwrap();
        assert_eq!(opened.decoded_files(), 0);
        assert_eq!(opened.decoded_bodies(), 0);
        assert!(opened.directory().is_err());
    }
}

#[cfg(test)]
mod indexed_tests {
    use super::*;
    #[test]
    fn one_name_does_not_walk_unrelated_library_rows() {
        let mut demanded = vec![];
        for count in [8, 2048] {
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("core.hirsel");
            let mut source = "pub fn answer() -> u64 { 7u64 }\n".to_owned();
            for i in 0..count {
                source.push_str(&format!("pub fn unused{i}() -> u64 {{ {i}u64 }}\n"));
            }
            std::fs::write(&file, source).unwrap();
            let inputs = CorpusManifest {
                core: package("core"),
                packages: vec![PackageManifest {
                    package: package("core"),
                    dependencies: vec![],
                    sources: vec![hirsel_core_resolve::ModuleSource {
                        file,
                        modules: vec!["core".into()],
                        signatures: true,
                    }],
                }],
            };
            let product = PackageProduct::sources(
                inputs,
                &dir.path().join("products"),
                &mut Meter::new(false),
            )
            .unwrap();
            let path = product.image.store_root();
            drop(product);
            let image = CoreImage::open(&path).unwrap().unwrap();
            assert_eq!(
                (image.decoded_index_nodes(), image.decoded_index_rows()),
                (0, 0)
            );
            let name = DefPath::from_full("core@1/core/answer").unwrap();
            assert!(image.resolve_name(&name).unwrap().is_some());
            assert!(image.demand_declaration(&name).unwrap());
            let rows = image.decoded_index_rows();
            assert!(
                rows < 16,
                "{count} unrelated bodies demanded {rows} index rows"
            );
            // Each keyed row follows at most two fixed 8-page arrays, a names header and a bounded bucket.
            assert!(image.decoded_index_nodes() <= rows * 18);
            assert_eq!((image.decoded_bodies(), image.decoded_files()), (0, 0));
            assert!(
                image
                    .resolve_name(&DefPath::from_full("core@1/core/missing").unwrap())
                    .unwrap()
                    .is_none()
            );
            demanded.push(rows);
        }
        assert_eq!(demanded[0], demanded[1]);
    }
}
